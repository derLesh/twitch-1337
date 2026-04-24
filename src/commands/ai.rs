use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
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

/// Dependencies needed to fire the fire-and-forget memory-extraction task
/// after each successful `!ai` exchange. Kept separate from chat generation
/// so the extractor can use a different `model` / `timeout` / round budget.
pub struct AiExtractionDeps {
    /// Gate for spawning per-turn extraction. Mirrors `[ai.extraction].enabled`.
    /// When `false`, the extractor never runs; chat generation is unaffected.
    pub enabled: bool,
    pub llm: Arc<dyn LlmClient>,
    pub model: String,
    pub timeout: Duration,
    pub max_rounds: usize,
}

/// Memory store handle + per-turn extractor deps bundled together so
/// `AiCommand` only has to carry one optional field for the whole
/// memory subsystem.
pub struct AiMemory {
    pub config: memory::MemoryConfig,
    pub extraction_deps: AiExtractionDeps,
}

pub struct AiCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    cooldown: PerUserCooldown,
    prompts: AiPrompts,
    timeout: Duration,
    chat_ctx: Option<ChatContext>,
    memory: Option<AiMemory>,
}

impl AiCommand {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        prompts: AiPrompts,
        timeout: Duration,
        cooldown: Duration,
        chat_ctx: Option<ChatContext>,
        memory: Option<AiMemory>,
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

        let now = Utc::now();
        let facts = if let Some(ref mem) = self.memory {
            let mut store_guard = mem.config.store.write().await;
            store_guard.format_for_prompt(now).unwrap_or_default()
        } else {
            String::new()
        };
        let system_prompt = format!(
            "{}{}\n\nCurrent time: {}",
            self.prompts.system,
            facts,
            now.with_timezone(&chrono_tz::Europe::Berlin)
                .format("%Y-%m-%d %H:%M %Z")
        );

        let user_message = self
            .prompts
            .instruction_template
            .replace("{message}", &instruction);

        let chat_history_text = if let Some(ref chat) = self.chat_ctx {
            let buf = chat.history.lock().await;
            if buf.is_empty() {
                String::new()
            } else {
                buf.iter()
                    .map(|(user, msg, ts)| {
                        let ts_berlin = ts.with_timezone(&chrono_tz::Europe::Berlin);
                        format!("[{}] {user}: {msg}", ts_berlin.format("%H:%M"))
                    })
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
                    buf.push_back((chat.bot_username.clone(), truncated.clone(), Utc::now()));
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

        // Spawn fire-and-forget memory extraction (only on successful AI responses
        // AND when the `[ai.extraction].enabled` flag allows it).
        if let (true, Some(mem)) = (success, &self.memory)
            && mem.extraction_deps.enabled
        {
            let ext_ctx = memory::ExtractionContext {
                speaker_id: ctx.privmsg.sender.id.clone(),
                speaker_username: ctx.privmsg.sender.login.clone(),
                speaker_role: memory::classify_role(&ctx.privmsg.badges),
                user_message: instruction.clone(),
                ai_response: response.clone(),
            };
            memory::spawn_memory_extraction(
                memory::ExtractionDeps {
                    llm: mem.extraction_deps.llm.clone(),
                    model: mem.extraction_deps.model.clone(),
                    store: mem.config.store.clone(),
                    store_path: mem.config.path.clone(),
                    caps: mem.config.caps.clone(),
                    half_life_days: mem.config.half_life_days,
                    timeout: mem.extraction_deps.timeout,
                    max_rounds: mem.extraction_deps.max_rounds,
                },
                ext_ctx,
            );
        }

        Ok(())
    }
}
