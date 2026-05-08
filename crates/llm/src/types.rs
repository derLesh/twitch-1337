//! Shared request/response types used by all providers.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The conversation role of a [`Message`]. Wire format is the lowercase
/// variant name; matches what every supported provider expects on the
/// `role` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        })
    }
}

/// A message in a chat completion conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
        }
    }
}

/// A tool result message returned after executing a tool call.
#[derive(Debug, Clone)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    /// The name of the tool that was invoked. Required by Ollama (`tool_name`)
    /// and harmless for OpenAI-compatible providers.
    pub tool_name: String,
    pub content: String,
}

impl ToolResultMessage {
    /// Build a tool-result message that mirrors the call's `id` and `name`.
    /// Both fields are required: OpenAI matches results to calls by
    /// `tool_call_id`; Ollama keys them by `tool_name`.
    pub fn for_call(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: content.into(),
        }
    }
}

/// One round of tool calling: the assistant's `tool_calls` and the matching
/// `tool` role results. Strict providers require the assistant turn carrying
/// `tool_calls` to precede the results referencing its `tool_call_id`s, so
/// multi-round loops must thread both halves back into the next request.
#[derive(Debug, Clone)]
pub struct ToolCallRound {
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResultMessage>,
    /// DeepSeek and other thinking models return a `reasoning_content` field
    /// alongside tool calls; they require it to be echoed back verbatim in the
    /// reconstructed assistant turn, or they reject the request with a 400.
    pub reasoning_content: Option<String>,
}

/// Identity hints forwarded as standard OpenAI body fields. OpenRouter's
/// Langfuse broadcast maps `user` to `userId` and `session_id` to Session ID
/// in the trace UI. Providers that don't recognize the fields (e.g. Ollama)
/// silently ignore them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TraceIds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Request for a chat completion.
#[derive(Debug, Clone)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// Optional reasoning effort hint (provider/model-specific values).
    pub reasoning_effort: Option<String>,
    pub trace: TraceIds,
}

/// Request for a chat completion with tool support.
#[derive(Debug, Clone)]
pub struct ToolChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    /// Optional reasoning effort hint (provider/model-specific values).
    pub reasoning_effort: Option<String>,
    /// Prior tool-call rounds, threaded back in order.
    pub prior_rounds: Vec<ToolCallRound>,
    pub trace: TraceIds,
}

/// Definition of a tool the LLM can call.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Build a `ToolDefinition` whose `parameters` schema is derived from
    /// `T` via [`schemars`]. Pairs with [`ToolCall::parse_args`] to keep
    /// the LLM-facing schema and the deserialize target in sync.
    pub fn derived<T: schemars::JsonSchema>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let schema = schemars::schema_for!(T);
        let parameters = serde_json::to_value(schema)
            .expect("JSON Schema serialization is infallible for derived types");
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A single tool call returned by the LLM.
///
/// Executors MUST check `arguments_parse_error` before inspecting `arguments`:
/// when set, the provider returned an unparseable payload and `arguments` is
/// `Value::Null`. Acting on the empty `arguments` would make a malformed call
/// indistinguishable from a genuinely empty one.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Set when the provider delivered `arguments` as an unparseable string
    /// (OpenAI-compatible APIs only).
    #[serde(default)]
    pub arguments_parse_error: Option<ToolArgsError>,
}

impl ToolCall {
    /// Parse the call's `arguments` into a typed struct. If the provider
    /// already flagged the payload as unparseable, the existing
    /// [`ToolArgsError::Provider`] is returned. Otherwise the call's
    /// `arguments` is deserialized into `T`.
    pub fn parse_args<T: serde::de::DeserializeOwned>(&self) -> Result<T, ToolArgsError> {
        if let Some(err) = &self.arguments_parse_error {
            return Err(err.clone());
        }
        serde_json::from_value(self.arguments.clone()).map_err(Into::into)
    }
}

/// Error from interpreting a tool call's `arguments` payload. The `Provider`
/// variant is set programmatically by the OpenAI provider when the LLM
/// returned an unparseable JSON string. The `Deserialize` variant is produced
/// by [`ToolCall::parse_args`] when the caller-supplied target type cannot
/// be built from the parsed JSON value.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolArgsError {
    #[error("provider returned malformed arguments: {error}")]
    Provider { error: String, raw: String },
    #[error("could not deserialize arguments: {error}")]
    Deserialize { error: String },
}

impl From<serde_json::Error> for ToolArgsError {
    fn from(e: serde_json::Error) -> Self {
        ToolArgsError::Deserialize {
            error: e.to_string(),
        }
    }
}

/// Response from a tool-calling chat completion.
#[derive(Debug, Clone)]
pub enum ToolChatCompletionResponse {
    /// The model returned a text response.
    Message(String),
    ToolCalls {
        calls: Vec<ToolCall>,
        /// Present on thinking/reasoning models (e.g. DeepSeek); must be
        /// echoed back in the assistant turn of subsequent requests.
        reasoning_content: Option<String>,
    },
}

#[cfg(test)]
mod role_tests {
    use super::Role;

    #[test]
    fn role_display_matches_wire_strings() {
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
        assert_eq!(Role::Tool.to_string(), "tool");
    }

    #[test]
    fn role_round_trips_through_json() {
        for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
            let json = serde_json::to_string(&role).unwrap();
            let back: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }
}

#[cfg(test)]
mod message_tests {
    use super::{Message, Role};

    #[test]
    fn constructors_set_the_right_role() {
        assert_eq!(Message::system("hi").role, Role::System);
        assert_eq!(Message::user("hi").role, Role::User);
        assert_eq!(Message::assistant("hi").role, Role::Assistant);
        assert_eq!(Message::tool("hi").role, Role::Tool);
    }

    #[test]
    fn constructors_accept_string_and_str() {
        let owned = String::from("owned");
        let from_owned = Message::system(owned.clone());
        let from_str = Message::system("borrowed");
        assert_eq!(from_owned.content, owned);
        assert_eq!(from_str.content, "borrowed");
    }
}

#[cfg(test)]
mod tool_result_tests {
    use super::{ToolCall, ToolResultMessage};

    #[test]
    fn for_call_threads_id_and_name() {
        let call = ToolCall {
            id: "X".to_string(),
            name: "save_memory".to_string(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: None,
        };
        let result = ToolResultMessage::for_call(&call, "ok");
        assert_eq!(result.tool_call_id, "X");
        assert_eq!(result.tool_name, "save_memory");
        assert_eq!(result.content, "ok");
    }
}

#[cfg(test)]
mod tool_args_error_tests {
    use super::ToolArgsError;

    #[test]
    fn provider_variant_round_trips_through_json() {
        let err = ToolArgsError::Provider {
            error: "unexpected token".to_string(),
            raw: "not json".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: ToolArgsError = serde_json::from_str(&json).unwrap();
        match back {
            ToolArgsError::Provider { error, raw } => {
                assert_eq!(error, "unexpected token");
                assert_eq!(raw, "not json");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn deserialize_variant_built_from_serde_json_error() {
        let parse_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let wrapped: ToolArgsError = parse_err.into();
        let rendered = wrapped.to_string();
        assert!(
            rendered.starts_with("could not deserialize arguments"),
            "got: {rendered}"
        );
    }
}

#[cfg(test)]
mod parse_args_tests {
    use serde::Deserialize;

    use super::{ToolArgsError, ToolCall};

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Demo {
        slug: String,
        n: u32,
    }

    fn call_with(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "X".into(),
            name: "demo".into(),
            arguments: args,
            arguments_parse_error: None,
        }
    }

    #[test]
    fn parse_args_success_returns_typed_value() {
        let call = call_with(serde_json::json!({"slug": "k", "n": 7}));
        let parsed: Demo = call.parse_args().unwrap();
        assert_eq!(
            parsed,
            Demo {
                slug: "k".into(),
                n: 7
            }
        );
    }

    #[test]
    fn parse_args_passes_through_provider_error() {
        let mut call = call_with(serde_json::Value::Null);
        call.arguments_parse_error = Some(ToolArgsError::Provider {
            error: "missing comma".into(),
            raw: "{".into(),
        });
        let err = call.parse_args::<Demo>().unwrap_err();
        let ToolArgsError::Provider { error, raw } = err else {
            panic!("expected Provider variant");
        };
        assert_eq!(error, "missing comma");
        assert_eq!(raw, "{");
    }

    #[test]
    fn parse_args_returns_deserialize_variant_on_type_mismatch() {
        let call = call_with(serde_json::json!({"slug": 1}));
        let err = call.parse_args::<Demo>().unwrap_err();
        match err {
            ToolArgsError::Deserialize { error } => assert!(!error.is_empty()),
            other => panic!("expected Deserialize variant, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tool_definition_tests {
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::ToolDefinition;

    #[derive(Debug, Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct DemoArgs {
        query: String,
        max_results: Option<u32>,
    }

    #[test]
    fn derived_emits_a_top_level_object_schema() {
        let def = ToolDefinition::derived::<DemoArgs>("demo", "Run a demo");
        assert_eq!(def.name, "demo");
        assert_eq!(def.description, "Run a demo");
        assert_eq!(
            def.parameters.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "schema is not an object: {}",
            def.parameters
        );
        let props = def
            .parameters
            .get("properties")
            .expect("properties present");
        assert!(props.get("query").is_some(), "missing query in {props}");
        assert!(props.get("max_results").is_some(), "missing max_results");
    }
}
