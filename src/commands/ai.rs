use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;
use tracing::{debug, error, instrument};

use crate::openrouter::OpenRouterClient;
use crate::{execute_ai_request, truncate_response, MAX_RESPONSE_LENGTH};

use super::{Command, CommandContext};

/// Cooldown duration for the AI command (30 seconds).
const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

pub struct AiCommand {
    openrouter_client: OpenRouterClient,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    system_prompt: String,
    instruction_template: String,
}

impl AiCommand {
    pub fn new(
        openrouter_client: OpenRouterClient,
        system_prompt: String,
        instruction_template: String,
    ) -> Self {
        Self {
            openrouter_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            system_prompt,
            instruction_template,
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
                        .say_in_reply_to(ctx.privmsg, "Bitte warte noch ein bisschen Waiting".to_string())
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

        // Execute AI with timeout
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            execute_ai_request(&user_message, &self.openrouter_client, &self.system_prompt),
        )
        .await;

        let response = match result {
            Ok(Ok(text)) => truncate_response(&text, MAX_RESPONSE_LENGTH),
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
