use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{debug, error, instrument};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::commands::ai::ChatContext;
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::util::{MAX_RESPONSE_LENGTH, truncate_response};

use super::{Command, CommandContext};

const NEWS_SYSTEM_PROMPT: &str = "Du fasst Twitch-Chat-Verlaeufe knapp und hilfreich zusammen. Antworte auf Deutsch, erfinde keine Details und konzentriere dich auf Themen, Highlights und wichtige Antworten. Halte die Antwort kurz genug fuer eine einzelne Twitch-Chatnachricht.";
const EMPTY_HISTORY_MESSAGE: &str =
    "Ich habe noch keine Chat-Historie für eine Zusammenfassung FDM";
const NO_NEW_MESSAGES_MESSAGE: &str =
    "Seit deiner letzten Nachricht ist noch nichts Neues passiert FDM";

pub struct NewsCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    cooldown: PerUserCooldown,
    timeout: Duration,
    chat_ctx: Option<ChatContext>,
}

impl NewsCommand {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        timeout: Duration,
        cooldown: Duration,
        chat_ctx: Option<ChatContext>,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldown: PerUserCooldown::new(cooldown),
            timeout,
            chat_ctx,
        }
    }

    async fn relevant_history(&self, user: &str, current_message: &str) -> Option<Vec<String>> {
        let chat = self.chat_ctx.as_ref()?;
        let mut snapshot = {
            let buf = chat.history.lock().await;
            buf.snapshot()
        };

        if snapshot.last().is_some_and(|entry| {
            entry.username.eq_ignore_ascii_case(user) && entry.text == current_message
        }) {
            snapshot.pop();
        }

        if snapshot.is_empty() {
            return None;
        }

        let start = snapshot
            .iter()
            .rposition(|entry| entry.username.eq_ignore_ascii_case(user))
            .map_or(0, |idx| idx + 1);

        let messages = snapshot[start..]
            .iter()
            .map(|entry| format!("{}: {}", entry.username, entry.text))
            .collect::<Vec<_>>();

        Some(messages)
    }
}

#[async_trait]
impl<T, L> Command<T, L> for NewsCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!news"
    }

    #[instrument(skip(self, ctx))]
    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;

        if let Some(remaining) = self.cooldown.check(user).await {
            debug!(user = %user, remaining_secs = remaining.as_secs(), "News command on cooldown");
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

        let Some(history_lines) = self.relevant_history(user, &ctx.privmsg.message_text).await
        else {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, EMPTY_HISTORY_MESSAGE.to_string())
                .await
            {
                error!(error = ?e, "Failed to send empty-history message");
            }
            return Ok(());
        };

        if history_lines.is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, NO_NEW_MESSAGES_MESSAGE.to_string())
                .await
            {
                error!(error = ?e, "Failed to send no-new-messages message");
            }
            return Ok(());
        }

        self.cooldown.record(user).await;

        let user_message = format!(
            "Fasse diesen Twitch-Chat seit der letzten Nachricht von {user} zusammen:\n{}",
            history_lines.join("\n")
        );

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: NEWS_SYSTEM_PROMPT.to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: user_message,
                },
            ],
        };

        let result =
            tokio::time::timeout(self.timeout, self.llm_client.chat_completion(request)).await;

        let (response, success) = match result {
            Ok(Ok(text)) => (truncate_response(&text, MAX_RESPONSE_LENGTH), true),
            Ok(Err(e)) => {
                error!(error = ?e, "News AI execution failed");
                ("Da ist was schiefgelaufen FDM".to_string(), false)
            }
            Err(_) => {
                error!("News AI execution timed out");
                ("Das hat zu lange gedauert Waiting".to_string(), false)
            }
        };

        if let (true, Some(chat)) = (success, &self.chat_ctx) {
            chat.history
                .lock()
                .await
                .push_bot(chat.bot_username.clone(), response.clone());
        }

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send news response");
        }

        Ok(())
    }
}
