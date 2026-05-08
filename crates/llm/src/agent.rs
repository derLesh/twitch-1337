//! Multi-round tool-calling agent runner.
//!
//! Drives a [`LlmClient::chat_completion_with_tools`] loop, dispatching
//! each tool call through a [`ToolExecutor`] and threading the round
//! results back into the next request. Returns when the model emits a
//! plain-text response, when `max_rounds` is reached, or when a per-round
//! timeout fires.

use std::time::Duration;

use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::client::LlmClient;
use crate::error::Result;
use crate::types::{
    ToolCall, ToolCallRound, ToolChatCompletionRequest, ToolChatCompletionResponse,
    ToolResultMessage,
};

/// Per-call dispatch hook for the agent loop. Each [`ToolCall`] returned
/// by the LLM is fed through `execute`; the returned [`ToolResultMessage`]
/// is threaded back into the next round of the conversation.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> ToolResultMessage;
}

/// Knobs for [`run_agent`].
#[derive(Debug, Clone)]
pub struct AgentOpts {
    /// Maximum number of LLM round-trips. After this many tool rounds the
    /// runner returns [`AgentOutcome::MaxRoundsExceeded`].
    pub max_rounds: usize,
    /// Optional per-LLM-call timeout. Wraps each
    /// `chat_completion_with_tools` call only — tool execution is not
    /// timed out by the runner.
    pub per_round_timeout: Option<Duration>,
}

/// Terminal state of [`run_agent`].
#[derive(Debug)]
pub enum AgentOutcome {
    /// The model returned a plain-text response (final answer).
    Text(String),
    /// The agent hit `max_rounds` before producing a text response.
    MaxRoundsExceeded,
    /// The per-round timeout fired during round `round` (0-indexed).
    Timeout { round: usize },
}

/// Drive a tool-calling conversation to completion.
#[instrument(
    skip(client, executor, request),
    fields(model = %request.model, max_rounds = opts.max_rounds),
)]
pub async fn run_agent<E: ToolExecutor + ?Sized>(
    client: &dyn LlmClient,
    mut request: ToolChatCompletionRequest,
    executor: &E,
    opts: AgentOpts,
) -> Result<AgentOutcome> {
    for round in 0..opts.max_rounds {
        let response = call_with_optional_timeout(client, &request, opts.per_round_timeout).await;

        let response = match response {
            Ok(r) => r?,
            Err(_) => return Ok(AgentOutcome::Timeout { round }),
        };

        match response {
            ToolChatCompletionResponse::Message(text) => {
                return Ok(AgentOutcome::Text(text));
            }
            ToolChatCompletionResponse::ToolCalls {
                calls,
                reasoning_content,
            } => {
                debug!(round, calls = calls.len(), "agent round");
                let mut results = Vec::with_capacity(calls.len());
                for call in &calls {
                    results.push(executor.execute(call).await);
                }
                request.prior_rounds.push(ToolCallRound {
                    calls,
                    results,
                    reasoning_content,
                });
            }
        }
    }

    Ok(AgentOutcome::MaxRoundsExceeded)
}

async fn call_with_optional_timeout(
    client: &dyn LlmClient,
    request: &ToolChatCompletionRequest,
    timeout: Option<Duration>,
) -> std::result::Result<Result<ToolChatCompletionResponse>, tokio::time::error::Elapsed> {
    let fut = client.chat_completion_with_tools(request.clone());
    match timeout {
        Some(d) => tokio::time::timeout(d, fut).await,
        None => Ok(fut.await),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::client::LlmClient;
    use crate::error::LlmError;
    use crate::types::{
        ChatCompletionRequest, Message, ToolCall, ToolChatCompletionRequest,
        ToolChatCompletionResponse, ToolResultMessage,
    };

    enum Scripted {
        Response(ToolChatCompletionResponse),
        Error(LlmError),
        Sleep(Duration),
    }

    struct ScriptedClient {
        queue: Mutex<Vec<Scripted>>,
    }

    impl ScriptedClient {
        fn new(steps: Vec<Scripted>) -> Self {
            Self {
                queue: Mutex::new(steps),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        async fn chat_completion(&self, _r: ChatCompletionRequest) -> Result<String> {
            unreachable!("agent runner only invokes chat_completion_with_tools");
        }

        async fn chat_completion_with_tools(
            &self,
            _r: ToolChatCompletionRequest,
        ) -> Result<ToolChatCompletionResponse> {
            let next = self.queue.lock().unwrap().remove(0);
            match next {
                Scripted::Response(r) => Ok(r),
                Scripted::Error(e) => Err(e),
                Scripted::Sleep(d) => {
                    tokio::time::sleep(d).await;
                    Ok(ToolChatCompletionResponse::Message("ignored".into()))
                }
            }
        }
    }

    struct EchoExecutor;

    #[async_trait]
    impl ToolExecutor for EchoExecutor {
        async fn execute(&self, call: &ToolCall) -> ToolResultMessage {
            ToolResultMessage::for_call(call, format!("echoed:{}", call.name))
        }
    }

    fn base_request() -> ToolChatCompletionRequest {
        ToolChatCompletionRequest {
            model: "test".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            reasoning_effort: None,
            prior_rounds: vec![],
            trace: crate::types::TraceIds::default(),
        }
    }

    fn opts(max_rounds: usize) -> AgentOpts {
        AgentOpts {
            max_rounds,
            per_round_timeout: None,
        }
    }

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: None,
        }
    }

    #[tokio::test]
    async fn returns_text_on_first_round() {
        let client = ScriptedClient::new(vec![Scripted::Response(
            ToolChatCompletionResponse::Message("hello".into()),
        )]);
        let outcome = run_agent(&client, base_request(), &EchoExecutor, opts(3))
            .await
            .unwrap();
        match outcome {
            AgentOutcome::Text(t) => assert_eq!(t, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_text_after_two_tool_rounds() {
        let client = ScriptedClient::new(vec![
            Scripted::Response(ToolChatCompletionResponse::ToolCalls {
                calls: vec![tool_call("c1", "noop")],
                reasoning_content: None,
            }),
            Scripted::Response(ToolChatCompletionResponse::ToolCalls {
                calls: vec![tool_call("c2", "noop")],
                reasoning_content: None,
            }),
            Scripted::Response(ToolChatCompletionResponse::Message("done".into())),
        ]);
        let outcome = run_agent(&client, base_request(), &EchoExecutor, opts(5))
            .await
            .unwrap();
        match outcome {
            AgentOutcome::Text(t) => assert_eq!(t, "done"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn max_rounds_exceeded_when_tool_calls_dont_terminate() {
        let client = ScriptedClient::new(vec![
            Scripted::Response(ToolChatCompletionResponse::ToolCalls {
                calls: vec![tool_call("c1", "noop")],
                reasoning_content: None,
            }),
            Scripted::Response(ToolChatCompletionResponse::ToolCalls {
                calls: vec![tool_call("c2", "noop")],
                reasoning_content: None,
            }),
        ]);
        let outcome = run_agent(&client, base_request(), &EchoExecutor, opts(2))
            .await
            .unwrap();
        assert!(matches!(outcome, AgentOutcome::MaxRoundsExceeded));
    }

    #[tokio::test]
    async fn timeout_returns_outcome_not_error() {
        let client = ScriptedClient::new(vec![Scripted::Sleep(Duration::from_millis(100))]);
        let outcome = run_agent(
            &client,
            base_request(),
            &EchoExecutor,
            AgentOpts {
                max_rounds: 3,
                per_round_timeout: Some(Duration::from_millis(10)),
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, AgentOutcome::Timeout { round: 0 }));
    }

    #[tokio::test]
    async fn llm_error_propagates() {
        let client = ScriptedClient::new(vec![Scripted::Error(LlmError::EmptyResponse)]);
        let err = run_agent(&client, base_request(), &EchoExecutor, opts(1))
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::EmptyResponse));
    }

    #[tokio::test]
    async fn tool_results_threaded_into_next_request() {
        struct CapturingClient {
            queue: Mutex<Vec<Scripted>>,
            captured: Mutex<Vec<ToolChatCompletionRequest>>,
        }

        #[async_trait]
        impl LlmClient for CapturingClient {
            async fn chat_completion(&self, _r: ChatCompletionRequest) -> Result<String> {
                unreachable!()
            }
            async fn chat_completion_with_tools(
                &self,
                r: ToolChatCompletionRequest,
            ) -> Result<ToolChatCompletionResponse> {
                self.captured.lock().unwrap().push(r);
                let next = self.queue.lock().unwrap().remove(0);
                match next {
                    Scripted::Response(r) => Ok(r),
                    _ => unreachable!(),
                }
            }
        }

        let client = CapturingClient {
            queue: Mutex::new(vec![
                Scripted::Response(ToolChatCompletionResponse::ToolCalls {
                    calls: vec![tool_call("c1", "tool_a")],
                    reasoning_content: Some("thinking".into()),
                }),
                Scripted::Response(ToolChatCompletionResponse::Message("done".into())),
            ]),
            captured: Mutex::new(Vec::new()),
        };

        run_agent(&client, base_request(), &EchoExecutor, opts(3))
            .await
            .unwrap();

        let captured = client.captured.lock().unwrap();
        assert_eq!(captured.len(), 2, "two LLM calls expected");
        assert!(captured[0].prior_rounds.is_empty());
        assert_eq!(captured[1].prior_rounds.len(), 1);
        let round = &captured[1].prior_rounds[0];
        assert_eq!(round.calls[0].id, "c1");
        assert_eq!(round.results[0].tool_call_id, "c1");
        assert_eq!(round.results[0].content, "echoed:tool_a");
        assert_eq!(round.reasoning_content.as_deref(), Some("thinking"));
    }
}
