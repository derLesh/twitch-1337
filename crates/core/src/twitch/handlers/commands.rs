//! Wires the `CommandDispatcher` and registers all `!`-prefixed commands.
//! Owns the long-running task that filters PRIVMSGs from the broadcast channel
//! and routes them to the matching `Command` implementation.

use std::{collections::HashMap, sync::Arc};

use llm::LlmClient;
use tokio::{sync::broadcast, time::Duration};
use tracing::{debug, error, info};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::ServerMessage, transport::Transport,
};

use crate::{
    ChatHistory, ChatHistoryBuffer, PersonalBest, ai, aviation, commands,
    config::{AiConfig, SuspendConfig},
    ping,
    settings::SettingsHandle,
    suspend::SuspensionManager,
    twitch::{seventv::SevenTvEmoteProvider, whisper::WhisperSender},
};

const GROK_ALIAS_TRIGGER: &str = "@grok";

/// Configuration for the generic command handler.
pub struct CommandHandlerConfig<T: Transport, L: LoginCredentials> {
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    pub client: Arc<TwitchIRCClient<T, L>>,
    /// Full AI config (system prompt, history, memory settings). `None` disables `!ai`.
    pub ai_config: Option<AiConfig>,
    /// Pre-built LLM client. When `None`, `!ai` is disabled regardless of `ai_config`.
    /// Injected so tests can supply a fake and production can call [`crate::llm_factory::build_llm_client`].
    pub llm: Option<Arc<dyn LlmClient>>,
    /// Pre-built memory v2 bundle. `None` disables v2 memory for `!ai`.
    pub ai_memory_v2: Option<ai::command::AiMemoryV2>,
    pub leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    pub ping_manager: Arc<tokio::sync::RwLock<ping::PingManager>>,
    pub hidden_admin_ids: Vec<String>,
    pub settings: SettingsHandle,
    pub tracker_tx: Option<tokio::sync::mpsc::Sender<aviation::TrackerCommand>>,
    pub aviation_client: Option<aviation::AviationClient>,
    pub whisper: Option<Arc<dyn WhisperSender>>,
    pub admin_channel: Option<String>,
    pub ai_channel: Option<String>,
    pub bot_username: String,
    pub channel: String,
    pub data_dir: std::path::PathBuf,
    pub doener: Arc<crate::doener::DoeneratlasClient>,
    pub suspension_manager: Arc<SuspensionManager>,
    pub suspend: SuspendConfig,
    /// Pre-built 7TV emote provider. `None` disables emote grounding for `!ai`.
    pub emote_provider: Option<Arc<SevenTvEmoteProvider>>,
}

/// Handler for generic text commands that start with `!`.
pub async fn run_generic_command_handler<T, L>(cfg: CommandHandlerConfig<T, L>)
where
    T: Transport + Send + Sync + 'static,
    L: LoginCredentials + Send + Sync + 'static,
{
    info!("Generic Command Handler started");

    let CommandHandlerConfig {
        broadcast_tx,
        client,
        ai_config,
        llm,
        ai_memory_v2,
        leaderboard,
        ping_manager,
        hidden_admin_ids,
        settings,
        tracker_tx,
        aviation_client,
        whisper,
        admin_channel,
        ai_channel,
        bot_username,
        channel,
        data_dir,
        doener,
        suspension_manager,
        suspend,
        emote_provider,
    } = cfg;

    // Snapshot of the dashboard-managed settings at startup. Reads below use
    // these values; Tasks 6+ make selected commands consume the handle live.
    let snapshot = settings.load_full();

    let default_suspend_duration = Duration::from_secs(suspend.default_duration_secs);

    let broadcast_rx = broadcast_tx.subscribe();

    // Extract history lengths before ai_config is consumed
    let history_length = ai_config.as_ref().map_or(0, |c| c.history_length) as usize;
    let ai_channel_history_length = ai_config
        .as_ref()
        .map_or(0, |c| c.ai_channel_history_length) as usize;
    let prefill_config = ai_config.as_ref().and_then(|c| c.history_prefill.clone());

    // Combine pre-built LLM client with AI config; both must be present to enable !ai.
    let llm_client: Option<(Arc<dyn LlmClient>, AiConfig)> = match (llm, ai_config) {
        (Some(llm_arc), Some(ai_cfg)) => {
            info!(backend = ?ai_cfg.backend, model = %ai_cfg.model, "AI command enabled");
            Some((llm_arc, ai_cfg))
        }
        _ => {
            debug!("AI not configured or LLM client unavailable, AI command disabled");
            None
        }
    };

    // Create chat history buffer for AI context (if history_length > 0)
    let primary_history: Option<ChatHistory> = if history_length > 0 {
        let buffer = if let Some(ref prefill_cfg) = prefill_config {
            let prefilled =
                ai::prefill::prefill_chat_history(&channel, history_length, prefill_cfg).await;
            ChatHistoryBuffer::from_prefill(history_length, prefilled)
        } else {
            ChatHistoryBuffer::new(history_length)
        };
        Some(Arc::new(tokio::sync::Mutex::new(buffer)))
    } else {
        None
    };

    // ai_channel buffer: allocated only when an ai_channel is configured AND
    // chat history is enabled. Capacity from ai.ai_channel_history_length.
    let ai_channel_history: Option<ChatHistory> = match (&ai_channel, primary_history.is_some()) {
        (Some(_), true) if ai_channel_history_length > 0 => Some(Arc::new(
            tokio::sync::Mutex::new(ChatHistoryBuffer::new(ai_channel_history_length)),
        )),
        _ => None,
    };

    let mut cmd_list: Vec<Box<dyn commands::Command<T, L>>> = vec![
        Box::new(commands::ping_admin::PingAdminCommand::new(
            ping_manager.clone(),
            hidden_admin_ids.clone(),
        )),
        Box::new(commands::suspend::SuspendCommand::new(
            suspension_manager.clone(),
            hidden_admin_ids.clone(),
            default_suspend_duration,
        )),
        Box::new(commands::suspend::UnsuspendCommand::new(
            suspension_manager.clone(),
            hidden_admin_ids,
        )),
        Box::new(aviation::commands::random_flight::RandomFlightCommand),
        Box::new(aviation::commands::flights_above::FlightsAboveCommand::new(
            aviation_client,
            Duration::from_secs(snapshot.cooldowns.up),
        )),
        Box::new(commands::leaderboard::LeaderboardCommand::new(
            leaderboard.clone(),
        )),
        Box::new(commands::pb::PbCommand::new(leaderboard)),
        Box::new(commands::feedback::FeedbackCommand::new(
            data_dir.clone(),
            Duration::from_secs(snapshot.cooldowns.feedback),
        )),
        Box::new(commands::doener::DoenerCommand::new(
            doener.clone(),
            Duration::from_secs(snapshot.cooldowns.doener),
        )),
        Box::new(commands::doener_calc::DoenerCalcCommand::new(
            doener.clone(),
            Duration::from_secs(snapshot.cooldowns.doener),
        )),
    ];

    if let Some(tx) = tracker_tx {
        cmd_list.push(Box::new(aviation::commands::track::TrackCommand::new(
            tx.clone(),
        )));
        cmd_list.push(Box::new(aviation::commands::untrack::UntrackCommand::new(
            tx.clone(),
        )));
        cmd_list.push(Box::new(aviation::commands::flights::FlightsCommand::new(
            tx.clone(),
        )));
        cmd_list.push(Box::new(aviation::commands::flights::FlightCommand::new(
            tx,
        )));
    }

    if let (Some((llm, cfg)), Some(ai_memory_v2)) = (llm_client, ai_memory_v2) {
        let web = if cfg.web.enabled {
            let search = match ai::content::SearchClient::new(
                &cfg.web.base_url,
                Duration::from_secs(cfg.web.timeout),
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    error!(error = ?e, "Failed to initialize ai.web search client; disabling web tools");
                    None
                }
            };

            let media_http = reqwest::Client::builder()
                .user_agent(crate::APP_USER_AGENT)
                .build()
                .expect("build media HTTP client");
            let provider_base_url = cfg.base_url.clone().unwrap_or_else(|| match cfg.backend {
                crate::config::AiBackend::OpenAi => "https://api.openai.com/v1".to_string(),
                crate::config::AiBackend::Ollama => "http://localhost:11434/v1".to_string(),
            });
            let media = Arc::new(ai::content::MediaClient::new(
                media_http,
                provider_base_url,
                cfg.api_key.clone(),
                cfg.media.model.clone(),
                Duration::from_secs(cfg.media.timeout),
            ));
            search.map(|client| ai::command::AiWeb {
                executor: Arc::new(ai::content::ContentToolExecutor::new(
                    client,
                    media,
                    cfg.media.clone(),
                    cfg.web.max_results,
                    Duration::from_secs(cfg.web.cache_ttl_secs),
                    cfg.web.cache_capacity,
                )),
                max_rounds: cfg.web.max_rounds,
            })
        } else {
            None
        };

        let chat_ctx = primary_history
            .clone()
            .map(|history| ai::command::ChatContext {
                primary_history: history,
                primary_login: channel.clone(),
                ai_channel_history: ai_channel_history.clone(),
                ai_channel_login: ai_channel.clone(),
            });

        cmd_list.push(Box::new(ai::command::AiCommand::new(
            ai::command::AiCommandDeps {
                llm_client: llm.clone(),
                model: cfg.model.clone(),
                reasoning_effort: cfg.reasoning_effort.clone(),
                cooldown: Duration::from_secs(snapshot.cooldowns.ai),
                chat_ctx: chat_ctx.clone(),
                memory: ai_memory_v2,
                web: web.clone(),
                emotes: emote_provider,
                bot_username: bot_username.clone(),
                doener: doener.clone(),
            },
        )));
        cmd_list.push(Box::new(commands::news::NewsCommand::new(
            llm.clone(),
            cfg.model.clone(),
            commands::news::NewsMode::News,
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(snapshot.cooldowns.news),
            chat_ctx.clone(),
            whisper.clone(),
        )));
        cmd_list.push(Box::new(commands::news::NewsCommand::new(
            llm,
            cfg.model,
            commands::news::NewsMode::Tldr,
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(snapshot.cooldowns.news),
            chat_ctx,
            whisper,
        )));
    }

    // PingTriggerCommand must be last: it matches any !<name> that is a registered ping,
    // so built-in commands earlier in the list take priority and can't be shadowed.
    cmd_list.push(Box::new(commands::ping_trigger::PingTriggerCommand::new(
        ping_manager,
        settings.clone(),
    )));

    run_command_dispatcher(
        broadcast_rx,
        client,
        cmd_list,
        admin_channel,
        ai_channel,
        primary_history,
        ai_channel_history,
        suspension_manager,
    )
    .await;
}

struct CommandInvocation<'a> {
    trigger: &'a str,
    args: Vec<&'a str>,
}

fn command_invocation(message_text: &str) -> Option<CommandInvocation<'_>> {
    let words = message_text.split_whitespace().collect::<Vec<_>>();
    let first_word = *words.first()?;

    if let Some((idx, trigger)) = words.iter().enumerate().find(|(idx, word)| {
        word.eq_ignore_ascii_case(GROK_ALIAS_TRIGGER)
            && words[..*idx].iter().all(|word| is_twitch_mention(word))
    }) {
        return Some(CommandInvocation {
            trigger,
            args: words[idx + 1..].to_vec(),
        });
    }

    Some(CommandInvocation {
        trigger: first_word,
        args: words[1..].to_vec(),
    })
}

fn is_twitch_mention(word: &str) -> bool {
    word.strip_prefix('@')
        .is_some_and(|name| !name.is_empty() && name.chars().all(is_twitch_login_char))
}

fn is_twitch_login_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Returns true if the trigger word resolves to the `!ai` command.
///
/// Mirrors `AiCommand::matches` plus the Grok alias used by `command_invocation`.
fn is_ai_trigger(trigger: &str) -> bool {
    let trimmed = trigger.strip_prefix('!').unwrap_or(trigger);
    trimmed.eq_ignore_ascii_case("ai") || trigger.eq_ignore_ascii_case(GROK_ALIAS_TRIGGER)
}

/// Main dispatch loop for trait-based commands.
// Two separate history buffers (primary + ai_channel) push this over the 7-arg limit;
// wrapping them in a struct would add noise without clarity gain.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_command_dispatcher<T, L>(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<TwitchIRCClient<T, L>>,
    commands: Vec<Box<dyn crate::commands::Command<T, L>>>,
    admin_channel: Option<String>,
    ai_channel: Option<String>,
    primary_history_for_dispatch: Option<ChatHistory>,
    ai_channel_history_for_dispatch: Option<ChatHistory>,
    suspension_manager: Arc<SuspensionManager>,
) where
    T: Transport,
    L: LoginCredentials,
{
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                let is_ai_channel = ai_channel
                    .as_ref()
                    .is_some_and(|ch| privmsg.channel_login == *ch);

                // In the admin channel, only the broadcaster can use commands.
                if let Some(ref admin_ch) = admin_channel
                    && privmsg.channel_login == *admin_ch
                    && !privmsg.badges.iter().any(|b| b.name == "broadcaster")
                {
                    continue;
                }

                // Record message in the appropriate chat history buffer.
                let target_buffer: Option<&ChatHistory> = if admin_channel
                    .as_ref()
                    .is_some_and(|ch| privmsg.channel_login == *ch)
                {
                    None
                } else if is_ai_channel {
                    ai_channel_history_for_dispatch.as_ref()
                } else {
                    primary_history_for_dispatch.as_ref()
                };
                if let Some(buffer) = target_buffer {
                    buffer.lock().await.push_user_at(
                        privmsg.sender.login.clone(),
                        privmsg.message_text.clone(),
                        privmsg.server_timestamp,
                    );
                }

                let Some(invocation) = command_invocation(&privmsg.message_text) else {
                    continue;
                };

                // In the ai channel, only `!ai` is reachable. Drop everything else so the
                // channel stays free of unrelated bot output.
                if is_ai_channel && !is_ai_trigger(invocation.trigger) {
                    continue;
                }

                let Some(cmd) = commands
                    .iter()
                    .find(|c| c.enabled() && c.matches(invocation.trigger))
                else {
                    continue;
                };

                // Must match SuspendCommand's normalization, else admin
                // suspensions silently miss the dispatcher hook.
                let suspend_key = crate::commands::normalize_command_name(invocation.trigger);
                if suspension_manager
                    .is_suspended(&suspend_key)
                    .await
                    .is_some()
                {
                    debug!(command = %invocation.trigger, "Skipping suspended command");
                    continue;
                }

                let trigger = invocation.trigger;
                let ctx = crate::commands::CommandContext {
                    privmsg: &privmsg,
                    client: &client,
                    trigger,
                    args: invocation.args,
                };

                if let Err(e) = cmd.execute(ctx).await {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        command = %trigger,
                        "Error handling command"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Command handler lagged, skipped messages");
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, command handler exiting");
                break;
            }
        }
    }
}

#[cfg(test)]
mod ai_trigger_tests {
    use super::is_ai_trigger;

    #[test]
    fn matches_bang_ai_case_insensitive() {
        assert!(is_ai_trigger("!ai"));
        assert!(is_ai_trigger("!AI"));
        assert!(is_ai_trigger("!Ai"));
    }

    #[test]
    fn matches_grok_alias() {
        // GROK_ALIAS_TRIGGER lives in the same module; whatever it is,
        // is_ai_trigger should accept the literal value plus its uppercase form.
        assert!(is_ai_trigger(super::GROK_ALIAS_TRIGGER));
        assert!(is_ai_trigger(&super::GROK_ALIAS_TRIGGER.to_uppercase()));
    }

    #[test]
    fn rejects_other_triggers() {
        assert!(!is_ai_trigger("!lb"));
        assert!(!is_ai_trigger("!p"));
        assert!(!is_ai_trigger("!track"));
        assert!(!is_ai_trigger("!up"));
        assert!(!is_ai_trigger("!fb"));
        assert!(!is_ai_trigger(""));
        assert!(!is_ai_trigger("!"));
        assert!(!is_ai_trigger("ai_chan"));
    }
}
