use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, instrument};

use crate::cooldown::format_cooldown_remaining;
use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::memory;
use crate::{truncate_response, ChatHistory, MAX_RESPONSE_LENGTH};

use super::{Command, CommandContext};

pub struct AiCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    cooldown: Duration,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    system_prompt: String,
    instruction_template: String,
    timeout: Duration,
    chat_history: Option<ChatHistory>,
    history_length: usize,
    bot_username: String,
    memory_store: Option<Arc<RwLock<memory::MemoryStore>>>,
    memory_store_path: Option<PathBuf>,
    max_memories: usize,
}

impl AiCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        system_prompt: String,
        instruction_template: String,
        timeout: Duration,
        cooldown: Duration,
        chat_history: Option<ChatHistory>,
        history_length: usize,
        bot_username: String,
        memory_store: Option<Arc<RwLock<memory::MemoryStore>>>,
        memory_store_path: Option<PathBuf>,
        max_memories: usize,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldown,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            system_prompt,
            instruction_template,
            timeout,
            chat_history,
            history_length,
            bot_username,
            memory_store,
            memory_store_path,
            max_memories,
        }
    }
}

#[async_trait]
impl Command for AiCommand {
    fn name(&self) -> &str {
        "!ai"
    }

    #[instrument(skip(self, ctx))]
    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;

        // Check cooldown
        {
            let cooldowns_guard = self.cooldowns.lock().await;
            if let Some(last_use) = cooldowns_guard.get(user) {
                let elapsed = last_use.elapsed();
                if elapsed < self.cooldown {
                    let remaining = self.cooldown - elapsed;
                    debug!(
                        user = %user,
                        remaining_secs = remaining.as_secs(),
                        "AI command on cooldown"
                    );
                    if let Err(e) = ctx
                        .client
                        .say_in_reply_to(
                            ctx.privmsg,
                            format!("Bitte warte noch {} Waiting", format_cooldown_remaining(remaining)),
                        )
                        .await
                    {
                        error!(error = ?e, "Failed to send cooldown message");
                    }
                    return Ok(());
                }
            }
        }

        let instruction = ctx.args.join(" ");

        // Check for empty instruction
        if instruction.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !ai <anweisung>".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        debug!(user = %user, instruction = %instruction, "Processing AI command");

        // Update cooldown before making the API call
        {
            let mut cooldowns_guard = self.cooldowns.lock().await;
            cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
        }

        // Build system prompt with memories injected
        let system_prompt = if let Some(ref store) = self.memory_store {
            let store_guard = store.read().await;
            match store_guard.format_for_prompt() {
                Some(facts) => format!("{}{}", self.system_prompt, facts),
                None => self.system_prompt.clone(),
            }
        } else {
            self.system_prompt.clone()
        };

        let user_message = self.instruction_template.replace("{message}", &instruction);

        // Build chat history string
        let chat_history_text = if let Some(ref history) = self.chat_history {
            let buf = history.lock().await;
            if buf.is_empty() {
                String::new()
            } else {
                buf.iter()
                    .map(|(user, msg)| format!("{user}: {msg}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        } else {
            String::new()
        };

        let user_message = user_message.replace("{chat_history}", &chat_history_text);

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: system_prompt,
                },
                Message {
                    role: "user".to_string(),
                    content: user_message,
                },
            ],
        };

        // Execute AI with timeout
        let result = tokio::time::timeout(
            self.timeout,
            self.llm_client.chat_completion(request),
        )
        .await;

        let (response, success) = match result {
            Ok(Ok(text)) => {
                let truncated = truncate_response(&text, MAX_RESPONSE_LENGTH);
                // Record successful response in chat history
                if let Some(ref history) = self.chat_history {
                    let mut buf = history.lock().await;
                    if buf.len() >= self.history_length {
                        buf.pop_front();
                    }
                    buf.push_back((self.bot_username.clone(), truncated.clone()));
                }
                (truncated, true)
            }
            Ok(Err(e)) => {
                error!(error = ?e, "AI execution failed");
                ("Da ist was schiefgelaufen FDM".to_string(), false)
            }
            Err(_) => {
                error!("AI execution timed out");
                ("Das hat zu lange gedauert Waiting".to_string(), false)
            }
        };

        // Send response to chat immediately
        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response.clone()).await {
            error!(error = ?e, "Failed to send AI response");
        }

        // Spawn fire-and-forget memory extraction (only on successful AI responses)
        if let (true, Some(store), Some(store_path)) =
            (success, self.memory_store.as_ref(), self.memory_store_path.as_ref())
        {
            memory::spawn_memory_extraction(
                self.llm_client.clone(),
                self.model.clone(),
                store.clone(),
                store_path.clone(),
                self.max_memories,
                user.to_string(),
                instruction,
                response,
                self.timeout,
            );
        }

        Ok(())
    }
}
