use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;
use tracing::{debug, error, instrument};

use crate::openrouter::OpenRouterClient;
use crate::{execute_ai_request, truncate_response, AuthenticatedTwitchClient, MAX_RESPONSE_LENGTH};

use super::{Command, CommandContext};

/// Cooldown duration for the AI command (30 seconds).
const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

pub struct AiCommand {
    pub openrouter_client: Option<OpenRouterClient>,
    pub cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl AiCommand {
    pub fn new(openrouter_client: Option<OpenRouterClient>) -> Self {
        Self {
            openrouter_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for AiCommand {
    fn name(&self) -> &str {
        "!ai"
    }

    fn enabled(&self) -> bool {
        self.openrouter_client.is_some()
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        if let Some(openrouter_client) = &self.openrouter_client {
            let instruction = ctx.args.join(" ");
            ai_command(
                ctx.privmsg,
                ctx.client,
                openrouter_client,
                &self.cooldowns,
                &instruction,
            )
            .await?;
        } else {
            debug!("AI command received but OpenRouter not configured");
        }
        Ok(())
    }
}

/// Handles the `!ai` command for AI-powered responses.
///
/// Takes user instructions and processes them with OpenRouter.
///
/// # Command Format
///
/// `!ai <instruction>`
///
/// # Rate Limiting
///
/// Per-user cooldown of 30 seconds to prevent spam.
///
/// # Errors
///
/// Returns an error if the OpenRouter API call fails.
#[instrument(skip(privmsg, client, openrouter_client, cooldowns))]
pub(crate) async fn ai_command(
    privmsg: &twitch_irc::message::PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    openrouter_client: &OpenRouterClient,
    cooldowns: &Arc<Mutex<HashMap<String, std::time::Instant>>>,
    instruction: &str,
) -> Result<()> {
    let user = &privmsg.sender.login;

    // Check cooldown
    {
        let cooldowns_guard = cooldowns.lock().await;
        if let Some(last_use) = cooldowns_guard.get(user) {
            let elapsed = last_use.elapsed();
            if elapsed < AI_COMMAND_COOLDOWN {
                let remaining = AI_COMMAND_COOLDOWN - elapsed;
                debug!(
                    user = %user,
                    remaining_secs = remaining.as_secs(),
                    "AI command on cooldown"
                );
                if let Err(e) = client
                    .say_in_reply_to(privmsg, "Bitte warte noch ein bisschen Waiting".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send cooldown message");
                }
                return Ok(());
            }
        }
    }

    // Check for empty instruction
    if instruction.trim().is_empty() {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Benutzung: !ai <anweisung>".to_string())
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    }

    debug!(user = %user, instruction = %instruction, "Processing AI command");

    // Update cooldown before making the API call
    {
        let mut cooldowns_guard = cooldowns.lock().await;
        cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
    }

    // Execute AI with timeout
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        execute_ai_request(instruction, openrouter_client),
    )
    .await;

    let response = match result {
        Ok(Ok(text)) => {
            // Truncate response for Twitch chat
            truncate_response(&text, MAX_RESPONSE_LENGTH)
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

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send AI response");
    }

    Ok(())
}
