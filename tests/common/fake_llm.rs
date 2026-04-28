//! A fake [`LlmClient`] that returns pre-queued responses and records incoming
//! requests.
//!
//! Tests push expected responses with [`FakeLlm::push_chat`] or
//! [`FakeLlm::push_tool`] before invoking the command under test. After the
//! command runs, inspect [`FakeLlm::chat_calls`] / [`FakeLlm::tool_calls`] to
//! assert request shape (tool definitions, tool results, etc.).

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use eyre::Result;

use twitch_1337::ai::llm::{
    ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse,
};

pub struct FakeLlm {
    chat_responses: Mutex<VecDeque<String>>,
    tool_responses: Mutex<VecDeque<ToolChatCompletionResponse>>,
    chat_calls: Mutex<Vec<ChatCompletionRequest>>,
    tool_calls: Mutex<Vec<ToolChatCompletionRequest>>,
}

impl FakeLlm {
    pub fn new() -> Self {
        Self {
            chat_responses: Mutex::new(VecDeque::new()),
            tool_responses: Mutex::new(VecDeque::new()),
            chat_calls: Mutex::new(Vec::new()),
            tool_calls: Mutex::new(Vec::new()),
        }
    }

    pub fn push_chat(&self, resp: impl Into<String>) {
        self.chat_responses.lock().unwrap().push_back(resp.into());
    }

    pub fn push_tool(&self, resp: ToolChatCompletionResponse) {
        self.tool_responses.lock().unwrap().push_back(resp);
    }

    pub fn chat_calls(&self) -> Vec<ChatCompletionRequest> {
        self.chat_calls.lock().unwrap().clone()
    }

    pub fn tool_calls(&self) -> Vec<ToolChatCompletionRequest> {
        self.tool_calls.lock().unwrap().clone()
    }
}

impl Default for FakeLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClient for FakeLlm {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String> {
        self.chat_calls.lock().unwrap().push(request);
        self.chat_responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| eyre::eyre!("FakeLlm: no chat response queued"))
    }

    async fn chat_completion_with_tools(
        &self,
        request: ToolChatCompletionRequest,
    ) -> Result<ToolChatCompletionResponse> {
        self.tool_calls.lock().unwrap().push(request);
        self.tool_responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| eyre::eyre!("FakeLlm: no tool response queued"))
    }
}
