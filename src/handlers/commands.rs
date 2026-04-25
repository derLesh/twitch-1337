//! Wires the `CommandDispatcher` and registers all `!`-prefixed commands.
//! Owns the long-running task that filters PRIVMSGs from the broadcast channel
//! and routes them to the matching `Command` implementation.

use std::{collections::HashMap, sync::Arc};

use tokio::{sync::broadcast, time::Duration};
use tracing::{debug, error, info, instrument};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::ServerMessage, transport::Transport,
};

use crate::{
    ChatHistory, ChatHistoryBuffer, PersonalBest, aviation, commands,
    config::{AiConfig, CooldownsConfig, SuspendConfig},
    flight_tracker, llm, ping, prefill,
    seventv::SevenTvEmoteProvider,
    suspend::SuspensionManager,
};

/// Configuration for the generic command handler.
pub struct CommandHandlerConfig<T: Transport, L: LoginCredentials> {
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    pub client: Arc<TwitchIRCClient<T, L>>,
    /// Full AI config (system prompt, history, memory settings). `None` disables `!ai`.
    pub ai_config: Option<AiConfig>,
    /// Pre-built LLM client. When `None`, `!ai` is disabled regardless of `ai_config`.
    /// Injected so tests can supply a fake and production can call [`llm::build_llm_client`].
    pub llm: Option<Arc<dyn llm::LlmClient>>,
    /// Pre-built memory bundle (store handle + extractor deps). Built in
    /// `run_bot` so the consolidation task in `lib.rs` can share the same
    /// `store` handle and `path`. `None` disables memory for `!ai`.
    pub ai_memory: Option<commands::ai::AiMemory>,
    pub leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    pub ping_manager: Arc<tokio::sync::RwLock<ping::PingManager>>,
    pub hidden_admin_ids: Vec<String>,
    pub default_cooldown: Duration,
    pub pings_public: bool,
    pub cooldowns: CooldownsConfig,
    pub tracker_tx: Option<tokio::sync::mpsc::Sender<flight_tracker::TrackerCommand>>,
    pub aviation_client: Option<aviation::AviationClient>,
    pub admin_channel: Option<String>,
    pub bot_username: String,
    pub channel: String,
    pub data_dir: std::path::PathBuf,
    pub suspension_manager: Arc<SuspensionManager>,
    pub suspend: SuspendConfig,
}

/// Handler for generic text commands that start with `!`.
#[instrument(skip(cfg))]
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
        ai_memory,
        leaderboard,
        ping_manager,
        hidden_admin_ids,
        default_cooldown,
        pings_public,
        cooldowns,
        tracker_tx,
        aviation_client,
        admin_channel,
        bot_username,
        channel,
        data_dir,
        suspension_manager,
        suspend,
    } = cfg;

    let default_suspend_duration = Duration::from_secs(suspend.default_duration_secs);

    let broadcast_rx = broadcast_tx.subscribe();

    // Extract history_length before ai_config is consumed
    let history_length = ai_config.as_ref().map_or(0, |c| c.history_length) as usize;
    let prefill_config = ai_config.as_ref().and_then(|c| c.history_prefill.clone());

    // Combine pre-built LLM client with AI config; both must be present to enable !ai.
    let llm_client: Option<(Arc<dyn llm::LlmClient>, AiConfig)> = match (llm, ai_config) {
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
    let chat_history: Option<ChatHistory> = if history_length > 0 {
        let buffer = if let Some(ref prefill_cfg) = prefill_config {
            let prefilled =
                prefill::prefill_chat_history(&channel, history_length, prefill_cfg).await;
            ChatHistoryBuffer::from_prefill(history_length, prefilled)
        } else {
            ChatHistoryBuffer::new(history_length)
        };
        Some(Arc::new(tokio::sync::Mutex::new(buffer)))
    } else {
        None
    };

    let emote_provider = llm_client
        .as_ref()
        .and_then(|(_, cfg)| cfg.emotes.enabled.then_some(cfg.emotes.clone()))
        .and_then(|emotes_cfg| match SevenTvEmoteProvider::new(emotes_cfg, &data_dir) {
            Ok(provider) => {
                info!("7TV emote glossary prompt grounding enabled");
                Some(Arc::new(provider))
            }
            Err(e) => {
                error!(error = ?e, "Failed to initialize 7TV emote provider; AI emotes disabled");
                None
            }
        });

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
        Box::new(commands::random_flight::RandomFlightCommand),
        Box::new(commands::flights_above::FlightsAboveCommand::new(
            aviation_client,
            Duration::from_secs(cooldowns.up),
        )),
        Box::new(commands::leaderboard::LeaderboardCommand::new(leaderboard)),
        Box::new(commands::feedback::FeedbackCommand::new(
            data_dir.clone(),
            Duration::from_secs(cooldowns.feedback),
        )),
    ];

    if let Some(tx) = tracker_tx {
        cmd_list.push(Box::new(commands::track::TrackCommand::new(tx.clone())));
        cmd_list.push(Box::new(commands::untrack::UntrackCommand::new(tx.clone())));
        cmd_list.push(Box::new(commands::flights::FlightsCommand::new(tx.clone())));
        cmd_list.push(Box::new(commands::flights::FlightCommand::new(tx)));
    }

    if let Some((llm, cfg)) = llm_client {
        let ai_chat_ctx = chat_history
            .clone()
            .map(|history| commands::ai::ChatContext {
                history,
                bot_username: bot_username.clone(),
            });
        let news_chat_ctx = chat_history
            .clone()
            .map(|history| commands::ai::ChatContext {
                history,
                bot_username: bot_username.clone(),
            });

        cmd_list.push(Box::new(commands::ai::AiCommand::new(
            commands::ai::AiCommandDeps {
                llm_client: llm.clone(),
                model: cfg.model.clone(),
                prompts: commands::ai::AiPrompts {
                    system: cfg.system_prompt,
                    instruction_template: cfg.instruction_template,
                },
                timeout: Duration::from_secs(cfg.timeout),
                cooldown: Duration::from_secs(cooldowns.ai),
                chat_ctx: ai_chat_ctx,
                memory: ai_memory,
                emotes: emote_provider,
            },
        )));
        cmd_list.push(Box::new(commands::news::NewsCommand::new(
            llm,
            cfg.model,
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(cooldowns.news),
            news_chat_ctx,
        )));
    }

    // PingTriggerCommand must be last: it matches any !<name> that is a registered ping,
    // so built-in commands earlier in the list take priority and can't be shadowed.
    cmd_list.push(Box::new(commands::ping_trigger::PingTriggerCommand::new(
        ping_manager,
        default_cooldown,
        pings_public,
    )));

    run_command_dispatcher(
        broadcast_rx,
        client,
        cmd_list,
        admin_channel,
        chat_history,
        suspension_manager,
    )
    .await;
}

/// Main dispatch loop for trait-based commands.
pub(crate) async fn run_command_dispatcher<T, L>(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<TwitchIRCClient<T, L>>,
    commands: Vec<Box<dyn crate::commands::Command<T, L>>>,
    admin_channel: Option<String>,
    chat_history: Option<ChatHistory>,
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

                // In the admin channel, only the broadcaster can use commands
                if let Some(ref admin_ch) = admin_channel
                    && privmsg.channel_login == *admin_ch
                    && !privmsg.badges.iter().any(|b| b.name == "broadcaster")
                {
                    continue;
                }

                // Record message in chat history (main channel only)
                if let Some(ref history) = chat_history {
                    let is_admin_channel = admin_channel
                        .as_ref()
                        .is_some_and(|ch| privmsg.channel_login == *ch);
                    if !is_admin_channel {
                        history
                            .lock()
                            .await
                            .push_user(privmsg.sender.login.clone(), privmsg.message_text.clone());
                    }
                }

                let mut words = privmsg.message_text.split_whitespace();
                let Some(first_word) = words.next() else {
                    continue;
                };

                let Some(cmd) = commands
                    .iter()
                    .find(|c| c.enabled() && c.matches(first_word))
                else {
                    continue;
                };

                // Must match SuspendCommand's normalization, else admin
                // suspensions silently miss the dispatcher hook.
                let suspend_key = crate::commands::normalize_command_name(first_word);
                if suspension_manager
                    .is_suspended(&suspend_key)
                    .await
                    .is_some()
                {
                    debug!(command = %first_word, "Skipping suspended command");
                    continue;
                }

                let ctx = crate::commands::CommandContext {
                    privmsg: &privmsg,
                    client: &client,
                    trigger: first_word,
                    args: words.collect(),
                };

                if let Err(e) = cmd.execute(ctx).await {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        command = %first_word,
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
