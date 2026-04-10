use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::llm::{
    self, Message, ToolCall, ToolChatCompletionRequest, ToolChatCompletionResponse,
    ToolDefinition, ToolResultMessage,
};

const MEMORY_FILENAME: &str = "ai_memory.ron";
const MAX_EXTRACTION_ROUNDS: usize = 3;

const EXTRACTION_SYSTEM_PROMPT: &str = "\
You just had a conversation in a Twitch chat. Based on the exchange below, \
decide if any facts are worth remembering long-term about users, the channel, \
or the community. You can save new facts, overwrite outdated ones, or delete \
incorrect ones. Only save things that would be useful across future \
conversations. Do not save trivial or ephemeral things like greetings or \
simple questions.";

/// A single remembered fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub fact: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Persistent store of AI memories, serialized to RON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStore {
    pub memories: HashMap<String, Memory>,
}

impl MemoryStore {
    /// Load from disk. Returns empty store if file doesn't exist.
    pub fn load(data_dir: &Path) -> Result<(Self, PathBuf)> {
        let path = data_dir.join(MEMORY_FILENAME);
        let store = if path.exists() {
            let data =
                std::fs::read_to_string(&path).wrap_err("Failed to read ai_memory.ron")?;
            ron::from_str(&data).wrap_err("Failed to parse ai_memory.ron")?
        } else {
            info!("No ai_memory.ron found, starting with empty memory store");
            Self {
                memories: HashMap::new(),
            }
        };

        info!(count = store.memories.len(), "Loaded AI memories");
        Ok((store, path))
    }

    /// Write current state to disk using write+rename for atomicity.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp_path = path.with_extension("ron.tmp");
        let data = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .wrap_err("Failed to serialize AI memories")?;
        std::fs::write(&tmp_path, &data).wrap_err("Failed to write ai_memory.ron.tmp")?;
        std::fs::rename(&tmp_path, path)
            .wrap_err("Failed to rename ai_memory.ron.tmp to ai_memory.ron")?;
        debug!("Saved AI memories to disk");
        Ok(())
    }

    /// Format memories for injection into the system prompt.
    /// Returns None if there are no memories.
    pub fn format_for_prompt(&self) -> Option<String> {
        if self.memories.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort(); // Deterministic ordering
        Some(format!("\n\n## Known facts\n{}", lines.join("\n")))
    }

    /// Format memories for the extraction prompt (key: fact list).
    pub fn format_for_extraction(&self) -> String {
        if self.memories.is_empty() {
            return "(none)".to_string();
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort();
        lines.join("\n")
    }

    /// Execute a single tool call against the store. Returns a result message.
    pub fn execute_tool_call(&mut self, call: &ToolCall, max_memories: usize) -> String {
        match call.name.as_str() {
            "save_memory" => {
                let key = call.arguments.get("key").and_then(|v| v.as_str());
                let fact = call.arguments.get("fact").and_then(|v| v.as_str());
                match (key, fact) {
                    (Some(key), Some(fact)) => {
                        let now = chrono::Utc::now().to_rfc3339();
                        if self.memories.contains_key(key) {
                            // Update existing
                            let mem = self.memories.get_mut(key).unwrap();
                            mem.fact = fact.to_string();
                            mem.updated_at = now;
                            format!("Updated memory '{}'", key)
                        } else if self.memories.len() >= max_memories {
                            format!(
                                "Memory full ({}/{}) — delete a memory first",
                                self.memories.len(),
                                max_memories
                            )
                        } else {
                            // Create new
                            self.memories.insert(
                                key.to_string(),
                                Memory {
                                    fact: fact.to_string(),
                                    created_at: now.clone(),
                                    updated_at: now,
                                },
                            );
                            format!("Saved memory '{}'", key)
                        }
                    }
                    _ => "Error: save_memory requires 'key' and 'fact' parameters".to_string(),
                }
            }
            "delete_memory" => {
                let key = call.arguments.get("key").and_then(|v| v.as_str());
                match key {
                    Some(key) => {
                        if self.memories.remove(key).is_some() {
                            format!("Deleted memory '{}'", key)
                        } else {
                            format!("No memory with key '{}'", key)
                        }
                    }
                    None => "Error: delete_memory requires 'key' parameter".to_string(),
                }
            }
            other => format!("Unknown tool: {}", other),
        }
    }
}

pub fn memory_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "save_memory".to_string(),
            description: "Save or update a fact. Use a short descriptive slug as the key (e.g. 'chrono-favorite-game'). If the key already exists, the fact is overwritten.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Short slug identifier for this memory"
                    },
                    "fact": {
                        "type": "string",
                        "description": "The fact to remember"
                    }
                },
                "required": ["key", "fact"]
            }),
        },
        ToolDefinition {
            name: "delete_memory".to_string(),
            description: "Delete a stored memory by its key.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key of the memory to delete"
                    }
                },
                "required": ["key"]
            }),
        },
    ]
}

/// Spawn a fire-and-forget task that asks the LLM to extract memories.
/// Errors are logged and swallowed — never affects the user-facing response.
#[allow(clippy::too_many_arguments)]
pub fn spawn_memory_extraction(
    llm_client: Arc<dyn llm::LlmClient>,
    model: String,
    store: Arc<RwLock<MemoryStore>>,
    store_path: PathBuf,
    max_memories: usize,
    username: String,
    user_message: String,
    ai_response: String,
    timeout: std::time::Duration,
) {
    tokio::spawn(async move {
        if let Err(e) = run_memory_extraction(
            &*llm_client,
            &model,
            &store,
            &store_path,
            max_memories,
            &username,
            &user_message,
            &ai_response,
            timeout,
        )
        .await
        {
            debug!("Memory extraction failed (non-critical): {:#}", e);
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_memory_extraction(
    llm_client: &dyn llm::LlmClient,
    model: &str,
    store: &RwLock<MemoryStore>,
    store_path: &Path,
    max_memories: usize,
    username: &str,
    user_message: &str,
    ai_response: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    let current_memories = {
        let store_guard = store.read().await;
        store_guard.format_for_extraction()
    };

    // Note: chat history integration is deferred until #12 (conversation context) lands.
    // When #12 is merged, pass chat history here and include it in the prompt.
    let user_content = format!(
        "Current memories:\n{}\n\nConversation:\nUser ({}): {}\nAssistant: {}",
        current_memories, username, user_message, ai_response
    );

    let tools = memory_tool_definitions();
    let messages = vec![
        Message {
            role: "system".to_string(),
            content: EXTRACTION_SYSTEM_PROMPT.to_string(),
        },
        Message {
            role: "user".to_string(),
            content: user_content,
        },
    ];
    let mut tool_results: Vec<ToolResultMessage> = Vec::new();

    for round in 0..MAX_EXTRACTION_ROUNDS {
        let request = ToolChatCompletionRequest {
            model: model.to_string(),
            messages: messages.clone(),
            tools: tools.clone(),
            tool_results: tool_results.clone(),
        };

        let response =
            tokio::time::timeout(timeout, llm_client.chat_completion_with_tools(request))
                .await
                .wrap_err("Memory extraction timed out")?
                .wrap_err("Memory extraction LLM call failed")?;

        match response {
            ToolChatCompletionResponse::Message(_) => {
                debug!(round, "Memory extraction finished (text response)");
                break;
            }
            ToolChatCompletionResponse::ToolCalls(calls) => {
                debug!(round, count = calls.len(), "Memory extraction: processing tool calls");
                let mut store_guard = store.write().await;
                tool_results.clear();
                for call in &calls {
                    let result = store_guard.execute_tool_call(call, max_memories);
                    info!(tool = %call.name, key = %call.arguments.get("key").and_then(|v| v.as_str()).unwrap_or("?"), result = %result, "Memory tool executed");
                    tool_results.push(ToolResultMessage {
                        tool_call_id: call.id.clone(),
                        content: result,
                    });
                }
                // Persist after each round of tool calls
                store_guard.save(store_path)?;
                // Add assistant tool_calls message to conversation for next round
                // (the LLM client handles this via tool_results in the request)
            }
        }
    }

    Ok(())
}
