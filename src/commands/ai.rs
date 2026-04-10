use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;
use tracing::{debug, error, instrument};

use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::{truncate_response, ChatHistory, MAX_RESPONSE_LENGTH};

use super::{Command, CommandContext};

/// Cooldown duration for the AI command (30 seconds).
const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

pub struct AiCommand {
    llm_client: Box<dyn LlmClient>,
    model: String,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    system_prompt: String,
    instruction_template: String,
    timeout: Duration,
    chat_history: Option<ChatHistory>,
    history_length: usize,
    bot_username: String,
}

impl AiCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm_client: Box<dyn LlmClient>,
        model: String,
        system_prompt: String,
        instruction_template: String,
        timeout: Duration,
        chat_history: Option<ChatHistory>,
        history_length: usize,
        bot_username: String,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            system_prompt,
            instruction_template,
            timeout,
            chat_history,
            history_length,
            bot_username,
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
                if elapsed < AI_COMMAND_COOLDOWN {
                    let remaining = AI_COMMAND_COOLDOWN - elapsed;
                    debug!(
                        user = %user,
                        remaining_secs = remaining.as_secs(),
                        "AI command on cooldown"
                    );
                    if let Err(e) = ctx
                        .client
                        .say_in_reply_to(
                            ctx.privmsg,
                            "Bitte warte noch ein bisschen Waiting".to_string(),
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
                    content: self.system_prompt.clone(),
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

        let response = match result {
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
                truncated
            }
            Ok(Err(e)) => {
                error!(error = ?e, "AI execution failed");
                "Da ist was schiefgelaufen FDM".to_string()
            }
            Err(_) => {
                error!("AI execution timed out");
                "Das hat zu lange gedauert Waiting".to_string()
            }
        };

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send AI response");
        }

        Ok(())
    }
}
