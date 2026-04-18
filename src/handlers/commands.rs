use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use secrecy::ExposeSecret as _;
use tokio::{
    sync::broadcast,
    time::Duration,
};
use tracing::{debug, error, info, instrument};
use twitch_irc::message::ServerMessage;

use crate::{
    AuthenticatedTwitchClient, ChatHistory, PersonalBest, aviation, commands, flight_tracker,
    get_data_dir, llm, memory, ping, prefill,
    config::{AiBackend, AiConfig, CooldownsConfig},
};

/// Configuration for the generic command handler.
///
/// Uses the production concrete client type because `Box<dyn Command>` (the command
/// registry) requires a fixed `CommandContext` type — making this generic over
/// `Transport + LoginCredentials` would prevent `dyn Command` dispatch.
/// Handlers that don't use `dyn Command` (1337, latency, schedules) are generic.
pub struct CommandHandlerConfig {
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    pub client: Arc<AuthenticatedTwitchClient>,
    pub ai_config: Option<AiConfig>,
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
}

/// Handler for generic text commands that start with `!`.
#[instrument(skip(cfg))]
pub async fn run_generic_command_handler(cfg: CommandHandlerConfig) {
    info!("Generic Command Handler started");

    let CommandHandlerConfig {
        broadcast_tx,
        client,
        ai_config,
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
    } = cfg;

    // Subscribe to the broadcast channel
    let broadcast_rx = broadcast_tx.subscribe();

    // Extract history_length before ai_config is consumed
    let history_length = ai_config.as_ref().map_or(0, |c| c.history_length) as usize;
    let prefill_config = ai_config.as_ref().and_then(|c| c.history_prefill.clone());

    // Initialize LLM client (optional)
    let llm_client: Option<(Arc<dyn llm::LlmClient>, AiConfig)> =
        if let Some(ai_cfg) = ai_config {
            let client_result = match ai_cfg.backend {
                AiBackend::OpenAi => {
                    let api_key = ai_cfg
                        .api_key
                        .as_ref()
                        .expect("validated: openai backend has api_key");
                    llm::openai::OpenAiClient::new(
                        api_key.expose_secret(),
                        &ai_cfg.model,
                        ai_cfg.base_url.as_deref(),
                    )
                    .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>)
                }
                AiBackend::Ollama => {
                    llm::ollama::OllamaClient::new(&ai_cfg.model, ai_cfg.base_url.as_deref())
                        .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>)
                }
            };
            match client_result {
                Ok(client) => {
                    info!(backend = ?ai_cfg.backend, model = %ai_cfg.model, "AI command enabled");
                    Some((client, ai_cfg))
                }
                Err(e) => {
                    error!(error = ?e, "Failed to initialize LLM client, AI command disabled");
                    None
                }
            }
        } else {
            debug!("AI not configured, AI command disabled");
            None
        };

    // Create chat history buffer for AI context (if history_length > 0)
    let chat_history: Option<ChatHistory> = if history_length > 0 {
        let buf = if let Some(ref prefill_cfg) = prefill_config {
            prefill::prefill_chat_history(&channel, history_length, prefill_cfg).await
        } else {
            VecDeque::with_capacity(history_length)
        };
        Some(Arc::new(tokio::sync::Mutex::new(buf)))
    } else {
        None
    };

    let data_dir = get_data_dir();

    let mut cmd_list: Vec<Box<dyn commands::Command>> = vec![
        Box::new(commands::ping_admin::PingAdminCommand::new(
            ping_manager.clone(),
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
        // Load memory store if enabled
        let memory_config = if cfg.memory_enabled {
            match memory::MemoryStore::load(&data_dir) {
                Ok((store, path)) => Some(memory::MemoryConfig {
                    store: Arc::new(tokio::sync::RwLock::new(store)),
                    path,
                    max_memories: cfg.max_memories,
                }),
                Err(e) => {
                    error!(error = ?e, "Failed to load AI memory store, memory disabled");
                    None
                }
            }
        } else {
            None
        };

        let chat_ctx = chat_history
            .clone()
            .map(|history| commands::ai::ChatContext {
                history,
                history_length,
                bot_username: bot_username.clone(),
            });

        cmd_list.push(Box::new(commands::ai::AiCommand::new(
            llm,
            cfg.model,
            commands::ai::AiPrompts {
                system: cfg.system_prompt,
                instruction_template: cfg.instruction_template,
            },
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(cooldowns.ai),
            chat_ctx,
            memory_config,
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
        history_length,
    )
    .await;
}

/// Main dispatch loop for trait-based commands.
pub async fn run_command_dispatcher(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    commands: Vec<Box<dyn crate::commands::Command>>,
    admin_channel: Option<String>,
    chat_history: Option<ChatHistory>,
    history_length: usize,
) {
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
                        let mut buf = history.lock().await;
                        if buf.len() >= history_length {
                            buf.pop_front();
                        }
                        buf.push_back((privmsg.sender.login.clone(), privmsg.message_text.clone()));
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
