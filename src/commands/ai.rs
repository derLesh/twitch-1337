use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{debug, error, instrument};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::memory;
use crate::util::{ChatHistory, MAX_RESPONSE_LENGTH, truncate_response};

use super::{Command, CommandContext};

/// Groups the shared chat history buffer with its capacity and the bot's username.
pub struct ChatContext {
    pub history: ChatHistory,
    pub history_length: usize,
    pub bot_username: String,
}

/// Prompt templates for the AI command.
pub struct AiPrompts {
    pub system: String,
    pub instruction_template: String,
}

pub struct AiCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    cooldown: PerUserCooldown,
    prompts: AiPrompts,
    timeout: Duration,
    chat_ctx: Option<ChatContext>,
    memory: Option<memory::MemoryConfig>,
}

impl AiCommand {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        prompts: AiPrompts,
        timeout: Duration,
        cooldown: Duration,
        chat_ctx: Option<ChatContext>,
        memory: Option<memory::MemoryConfig>,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldown: PerUserCooldown::new(cooldown),
            prompts,
            timeout,
            chat_ctx,
            memory,
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for AiCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!ai"
    }

    #[instrument(skip(self, ctx))]
    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;

        // Check cooldown
        if let Some(remaining) = self.cooldown.check(user).await {
            debug!(user = %user, remaining_secs = remaining.as_secs(), "AI command on cooldown");
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    format!(
                        "Bitte warte noch {} Waiting",
                        format_cooldown_remaining(remaining)
                    ),
                )
                .await
            {
                error!(error = ?e, "Failed to send cooldown message");
            }
            return Ok(());
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

        self.cooldown.record(user).await;

        // Build system prompt with memories injected
        let system_prompt = if let Some(ref mem) = self.memory {
            let store_guard = mem.store.read().await;
            match store_guard.format_for_prompt() {
                Some(facts) => format!("{}{}", self.prompts.system, facts),
                None => self.prompts.system.clone(),
            }
        } else {
            self.prompts.system.clone()
        };

        let user_message = self
            .prompts
            .instruction_template
            .replace("{message}", &instruction);

        // Build chat history string
        let chat_history_text = if let Some(ref chat) = self.chat_ctx {
            let buf = chat.history.lock().await;
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
        let result =
            tokio::time::timeout(self.timeout, self.llm_client.chat_completion(request)).await;

        let (response, success) = match result {
            Ok(Ok(text)) => {
                let truncated = truncate_response(&text, MAX_RESPONSE_LENGTH);
                // Record successful response in chat history
                if let Some(ref chat) = self.chat_ctx {
                    let mut buf = chat.history.lock().await;
                    if buf.len() >= chat.history_length {
                        buf.pop_front();
                    }
                    buf.push_back((chat.bot_username.clone(), truncated.clone()));
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
        if let Err(e) = ctx
            .client
            .say_in_reply_to(ctx.privmsg, response.clone())
            .await
        {
            error!(error = ?e, "Failed to send AI response");
        }

        // Spawn fire-and-forget memory extraction (only on successful AI responses)
        if let (true, Some(mem)) = (success, &self.memory) {
            memory::spawn_memory_extraction(
                self.llm_client.clone(),
                self.model.clone(),
                mem,
                user.clone(),
                instruction,
                response,
                self.timeout,
            );
        }

        Ok(())
    }
}
