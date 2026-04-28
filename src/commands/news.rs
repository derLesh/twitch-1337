use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use tracing::{debug, error, instrument, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::ai::command::ChatContext;
use crate::ai::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::twitch::whisper::{WHISPER_MAX_CHARS, WhisperSender};
use crate::util::{MAX_RESPONSE_LENGTH, truncate_response};

use super::{Command, CommandContext};

const NEWS_PREFIX: &str = "ICYMI:";
const NEWS_SYSTEM_PROMPT: &str = "Du fasst Twitch-Chat-Verläufe hilfreich zusammen. Antworte auf Deutsch, erfinde keine Details und konzentriere dich auf Themen, Highlights und wichtige Antworten. Beginne die Antwort mit \"ICYMI:\". Wenn du mehrere Themen auflistest, trenne sie mit \" | \". Bleibe kompakt, aber du musst dich nicht auf eine einzelne Twitch-Chatnachricht beschränken.";
const TLDR_SYSTEM_PROMPT: &str = "Du erstellst ein hilfreiches TLDR der letzten verfügbaren 24 Stunden eines Twitch-Chats. Antworte auf Deutsch, erfinde keine Details und strukturiere die wichtigsten Themen, Highlights, Fragen/Antworten, Running Gags und offenen Punkte knapp. Beginne die Antwort mit \"In den letzten 24h:\".";
const EMPTY_HISTORY_MESSAGE: &str =
    "Ich habe noch keine Chat-Historie für eine Zusammenfassung FDM";
const NO_NEW_MESSAGES_MESSAGE: &str =
    "Seit deiner letzten Nachricht ist noch nichts Neues passiert FDM";
const NO_TLDR_MESSAGES_MESSAGE: &str = "chat dead no tldr Deadge";
const RECENT_USER_CONTEXT_MESSAGES: usize = 20;
const TLDR_WINDOW_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy)]
pub enum NewsMode {
    News,
    Tldr,
}

impl NewsMode {
    fn trigger(self) -> &'static str {
        match self {
            Self::News => "!news",
            Self::Tldr => "!tldr",
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Self::News => NEWS_SYSTEM_PROMPT,
            Self::Tldr => TLDR_SYSTEM_PROMPT,
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::News => EMPTY_HISTORY_MESSAGE,
            Self::Tldr => NO_TLDR_MESSAGES_MESSAGE,
        }
    }
}

pub struct NewsCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    mode: NewsMode,
    cooldown: PerUserCooldown,
    timeout: Duration,
    chat_ctx: Option<ChatContext>,
    whisper: Option<Arc<dyn WhisperSender>>,
}

impl NewsCommand {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        mode: NewsMode,
        timeout: Duration,
        cooldown: Duration,
        chat_ctx: Option<ChatContext>,
        whisper: Option<Arc<dyn WhisperSender>>,
    ) -> Self {
        Self {
            llm_client,
            model,
            mode,
            cooldown: PerUserCooldown::new(cooldown),
            timeout,
            chat_ctx,
            whisper,
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

        let messages = match self.mode {
            NewsMode::News => {
                let recent_start = snapshot.len().saturating_sub(RECENT_USER_CONTEXT_MESSAGES);
                let start = snapshot
                    .iter()
                    .rposition(|entry| entry.username.eq_ignore_ascii_case(user))
                    .filter(|idx| *idx < recent_start)
                    .map_or(0, |idx| idx + 1);

                snapshot[start..]
                    .iter()
                    .map(|entry| format!("{}: {}", entry.username, entry.text))
                    .collect::<Vec<_>>()
            }
            NewsMode::Tldr => {
                let cutoff = Utc::now() - chrono::Duration::hours(TLDR_WINDOW_HOURS);
                snapshot
                    .iter()
                    .filter(|entry| entry.timestamp >= cutoff)
                    .map(|entry| format!("{}: {}", entry.username, entry.text))
                    .collect::<Vec<_>>()
            }
        };

        Some(messages)
    }

    async fn send_news_response<T, L>(
        &self,
        ctx: &CommandContext<'_, T, L>,
        response: &str,
    ) -> String
    where
        T: Transport,
        L: LoginCredentials,
    {
        if let Some(whisper) = &self.whisper {
            match whisper.send_whisper(&ctx.privmsg.sender.id, response).await {
                Ok(sent) => return sent,
                Err(error) => {
                    warn!(
                        error = ?error,
                        user = %ctx.privmsg.sender.login,
                        "News whisper is not authenticated or unavailable; falling back to chat"
                    );
                }
            }
        } else {
            warn!(
                user = %ctx.privmsg.sender.login,
                "News whisper is not configured or authenticated; falling back to chat"
            );
        }

        let chat_response = truncate_response(response, MAX_RESPONSE_LENGTH);
        if let Err(error) = ctx
            .client
            .say_in_reply_to(ctx.privmsg, chat_response.clone())
            .await
        {
            error!(error = ?error, "Failed to send news response");
        }
        chat_response
    }
}

fn format_news_response(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let prefixed = if trimmed
        .get(..NEWS_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(NEWS_PREFIX))
    {
        format!("{NEWS_PREFIX}{}", &trimmed[NEWS_PREFIX.len()..])
    } else {
        format!("{NEWS_PREFIX} {trimmed}")
    };

    truncate_response(&prefixed, max_chars)
}

#[async_trait]
impl<T, L> Command<T, L> for NewsCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        self.mode.trigger()
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
                .say_in_reply_to(ctx.privmsg, self.mode.empty_message().to_string())
                .await
            {
                error!(error = ?e, "Failed to send empty-history message");
            }
            return Ok(());
        };

        if history_lines.is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    match self.mode {
                        NewsMode::News => NO_NEW_MESSAGES_MESSAGE,
                        NewsMode::Tldr => NO_TLDR_MESSAGES_MESSAGE,
                    }
                    .to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send no-new-messages message");
            }
            return Ok(());
        }

        self.cooldown.record(user).await;

        let user_message = format!(
            "{}\n{}",
            match self.mode {
                NewsMode::News => format!(
                    "Fasse diesen Twitch-Chat für {user} zusammen. Wenn eine ältere Nachricht von {user} als Kontextgrenze genutzt wurde, beginnt der Verlauf danach; neuere Nachrichten von {user} sind Teil des Verlaufs."
                ),
                NewsMode::Tldr =>
                    "Erstelle ein TLDR der verfügbaren Chat-Historie aus den letzten 24 Stunden."
                        .to_string(),
            },
            history_lines.join("\n"),
        );

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: self.mode.system_prompt().to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: user_message,
                },
            ],
            reasoning_effort: None,
        };

        let result =
            tokio::time::timeout(self.timeout, self.llm_client.chat_completion(request)).await;

        let (response, success) = match result {
            Ok(Ok(text)) => (format_news_response(&text, WHISPER_MAX_CHARS), true),
            Ok(Err(e)) => {
                error!(error = ?e, "News AI execution failed");
                ("Da ist was schiefgelaufen FDM".to_string(), false)
            }
            Err(_) => {
                error!("News AI execution timed out");
                ("Das hat zu lange gedauert Waiting".to_string(), false)
            }
        };

        if success {
            self.send_news_response(&ctx, &response).await;
        } else {
            let chat_response = truncate_response(&response, MAX_RESPONSE_LENGTH);
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, chat_response.clone())
                .await
            {
                error!(error = ?e, "Failed to send news response");
            }
        }

        Ok(())
    }
}
