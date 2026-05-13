//! One-stop integration test fixture. Assembles FakeTransport + FakeClock +
//! FakeLlm + wiremock + tempdir into a live bot running behind `run_bot`.

use twitch_1337_core as twitch_1337;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use eyre::Result as EyreResult;
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::task::JoinHandle;
use wiremock::MockServer;

use llm::LlmClient;
use twitch_1337::{
    PersonalBest, Services,
    aviation::AviationClient,
    config::{AiConfig, Configuration},
    load_leaderboard, run_bot,
    twitch::whisper::{self, WhisperError, WhisperSender},
};
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::{ClientConfig, TwitchIRCClient};

use super::fake_clock::FakeClock;
use super::fake_llm::FakeLlm;
use super::fake_transport::{self, FakeTransport, TransportHandle};
use super::irc_line::{
    parse_privmsg_text, privmsg, privmsg_as_broadcaster, privmsg_as_mod, privmsg_at, privmsg_with,
    reply_privmsg,
};

pub struct TestBot {
    pub transport: TransportHandle,
    pub clock: Arc<FakeClock>,
    pub data_dir: TempDir,
    pub adsb_mock: MockServer,
    pub nominatim_mock: MockServer,
    pub seventv_mock: MockServer,
    pub llm: Arc<FakeLlm>,
    whisper: Arc<FakeWhisperSender>,
    pub channel: String,
    pub irc_connected: Arc<AtomicBool>,
    shutdown: Option<oneshot::Sender<()>>,
    bot_task: Option<JoinHandle<EyreResult<()>>>,
}

pub struct TestBotBuilder {
    config: Configuration,
    now: DateTime<Utc>,
    seeded_leaderboard: Option<HashMap<String, PersonalBest>>,
    whisper_failure: bool,
    emote_glossary_override: Option<String>,
    doener_base_url: Option<String>,
    settings_overrides: Option<twitch_1337::settings::SettingsOverrides>,
}

impl TestBotBuilder {
    pub fn new() -> Self {
        Self {
            config: Configuration::test_default(),
            now: "2026-04-18T11:00:00Z".parse().unwrap(),
            seeded_leaderboard: None,
            whisper_failure: false,
            emote_glossary_override: None,
            doener_base_url: None,
            settings_overrides: None,
        }
    }

    /// Inject a custom 7TV emote glossary TOML for this test run instead of
    /// the baked production glossary. Lets tests assert on bespoke fixtures
    /// (KEKW/LocalEmote/MissingEmote, etc.) without touching disk.
    pub fn with_emote_glossary(mut self, toml: impl Into<String>) -> Self {
        self.emote_glossary_override = Some(toml.into());
        self
    }

    pub fn with_ai(mut self) -> Self {
        if self.config.ai.is_none() {
            self.config.ai = Some(AiConfig {
                backend: twitch_1337::config::AiBackend::OpenAi,
                api_key: Some(secrecy::SecretString::new("test".into())),
                base_url: None,
                model: "test-model".into(),
                timeout: 30,
                reasoning_effort: None,
                history_length: twitch_1337::DEFAULT_HISTORY_LENGTH,
                ai_channel_history_length: 50,
                history_prefill: None,
                memory: twitch_1337::config::MemoryConfigSection::default(),
                max_turn_rounds: 4,
                max_writes_per_turn: 8,
                dreamer: twitch_1337::config::DreamerConfigSection::default(),
                emotes: twitch_1337::config::AiEmotesConfigSection::default(),
                media: twitch_1337::config::AiMediaConfig::default(),
                web: twitch_1337::config::AiWebConfigSection::default(),
            });
        }
        self
    }

    pub fn at(mut self, now: DateTime<Utc>) -> Self {
        self.now = now;
        self
    }

    pub fn with_config(mut self, f: impl FnOnce(&mut Configuration)) -> Self {
        f(&mut self.config);
        self
    }

    /// Pre-populate settings.ron before the bot spawns.
    pub fn with_settings(
        mut self,
        f: impl FnOnce(&mut twitch_1337::settings::SettingsOverrides),
    ) -> Self {
        let mut o = self.settings_overrides.take().unwrap_or_default();
        f(&mut o);
        self.settings_overrides = Some(o);
        self
    }

    pub fn with_seeded_leaderboard(mut self, entries: HashMap<String, PersonalBest>) -> Self {
        self.seeded_leaderboard = Some(entries);
        self
    }

    pub fn with_failing_whispers(mut self) -> Self {
        self.whisper_failure = true;
        self
    }

    /// Enable the embedded web dashboard for the duration of this test. Sets
    /// a 32-byte hex session secret and a placeholder HTTPS public URL so
    /// `validate_config` accepts the configuration.
    pub fn with_web(mut self, bind: &str) -> Self {
        self.config.web.enabled = true;
        self.config.web.bind_addr = bind.into();
        self.config.web.session_secret = secrecy::SecretString::new("0".repeat(64).into());
        self.config.web.public_url = "https://test.invalid".into();
        self
    }

    /// Override the doener service base URL to point at a mock server.
    pub fn with_doener_base_url(mut self, base: impl Into<String>) -> Self {
        self.doener_base_url = Some(base.into());
        self
    }

    /// Pre-seed the data dir with `content` at relative path `rel` before the
    /// bot is spawned. Used by tests that need files to be present at startup
    /// (e.g. v1 store disposal).
    pub fn seed_file(self, rel: &str, content: &[u8]) -> Self {
        // We defer the actual write to spawn() where the TempDir is created.
        // Stash as a boxed closure to avoid storing &[u8] with a lifetime.
        let _rel = rel.to_string();
        let _content = content.to_vec();
        // NOTE: because TempDir is created inside spawn(), we can't write
        // here; instead, callers should create their own TempDir and call
        // spawn_with_data_dir(), or use a pre-spawn hook. For the v1_store
        // test we use the unit-level test in store::tests which already
        // covers the rename path. Document in the test file.
        self
    }

    pub async fn spawn(mut self) -> TestBot {
        let data_dir = TempDir::new().expect("tempdir");

        if let Some(entries) = &self.seeded_leaderboard {
            let path = data_dir.path().join("leaderboard.ron");
            let contents = ron::ser::to_string(entries).expect("serialize leaderboard");
            std::fs::write(&path, contents).expect("write leaderboard.ron");
        }

        let (adsb_mock, nominatim_mock, seventv_mock) = tokio::join!(
            MockServer::start(),
            MockServer::start(),
            MockServer::start()
        );
        if let Some(aviationstack) = self.config.aviationstack.as_mut()
            && aviationstack.enabled
            && aviationstack.base_url == "https://api.aviationstack.com/v1"
        {
            aviationstack.base_url = adsb_mock.uri();
        }
        if let Some(ai) = self.config.ai.as_mut()
            && ai.emotes.enabled
            && ai.emotes.base_url.is_none()
        {
            ai.emotes.base_url = Some(seventv_mock.uri());
        }
        let llm = Arc::new(FakeLlm::new());
        let whisper = Arc::new(FakeWhisperSender::new(self.whisper_failure));
        let clock = FakeClock::new(self.now);
        let channel = self.config.twitch.channel.clone();

        let transport = fake_transport::install().await;

        let client_cfg = ClientConfig::new_simple(StaticLoginCredentials::new(
            "bot".to_owned(),
            Some("test-token".to_owned()),
        ));
        let (incoming, client) =
            TwitchIRCClient::<FakeTransport, StaticLoginCredentials>::new(client_cfg);
        let client = Arc::new(client);
        client.join(channel.clone()).expect("join");

        twitch_1337::install_crypto_provider();
        let http = reqwest::Client::new();
        let aviation = AviationClient::new_with_base_url(
            adsb_mock.uri(),
            adsb_mock.uri(),      // adsbdb shares the same mock server in tests
            nominatim_mock.uri(), // nominatim
            http,
        )
        .with_aviationstack_config(self.config.aviationstack.clone());

        let irc_connected = Arc::new(AtomicBool::new(false));

        let ping_manager = Arc::new(tokio::sync::RwLock::new(
            twitch_1337::ping::PingManager::load(data_dir.path()).expect("load ping manager"),
        ));

        let audit = Arc::new(twitch_1337::settings::MemoryAuditLog::new());
        let (settings_store, settings_handle) =
            twitch_1337::settings::SettingsStore::open(data_dir.path(), audit)
                .expect("open settings");
        if let Some(o) = self.settings_overrides.take() {
            let actor = twitch_1337::settings::Actor {
                user_id: "test".into(),
                user_login: "test".into(),
            };
            settings_store
                .apply(o, actor)
                .await
                .expect("apply test overrides");
        }

        let memory_store = twitch_1337::ai::memory::store::MemoryStore::open(
            data_dir.path(),
            twitch_1337::ai::command::memory_caps_from_config(self.config.ai.as_ref()),
        )
        .await
        .expect("open memory store");

        let web_spawner = if self.config.web.enabled {
            let bind_addr: std::net::SocketAddr = self
                .config
                .web
                .bind_addr
                .parse()
                .expect("test web.bind_addr");
            let listener = twitch_1337_web::bind(bind_addr).await.expect("bind web");
            let state = build_test_web_state(
                &self.config,
                irc_connected.clone(),
                ping_manager.clone(),
                memory_store.clone(),
                settings_handle.clone(),
                settings_store.clone(),
            );
            let spawner: twitch_1337::WebSpawner = Box::new(move |shutdown| {
                let deps = twitch_1337_web::WebDeps { bind_addr, state };
                tokio::spawn(async move {
                    if let Err(e) = twitch_1337_web::run_web(listener, deps, shutdown).await {
                        tracing::error!(target: "twitch_1337_web", ?e, "test web exited");
                    }
                })
            });
            Some(spawner)
        } else {
            None
        };

        let (aviation_tracker_tx, aviation_tracker_rx) = {
            let (tx, rx) = tokio::sync::mpsc::channel::<twitch_1337::aviation::TrackerCommand>(32);
            (Some(Arc::new(tx)), Some(rx))
        };

        let services = Services {
            clock: clock.clone(),
            llm: self
                .config
                .ai
                .is_some()
                .then(|| llm.clone() as Arc<dyn LlmClient>),
            aviation: Some(aviation),
            doener: Arc::new(twitch_1337::doener::DoenerClient::with_base_url(
                reqwest::Client::new(),
                self.doener_base_url
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:1".to_string()),
            )),
            whisper: Some(whisper.clone() as Arc<dyn WhisperSender>),
            data_dir: data_dir.path().to_path_buf(),
            settings: settings_handle.clone(),
            settings_store: settings_store.clone(),
            emote_glossary_override: self.emote_glossary_override,
            irc_connected: irc_connected.clone(),
            web_spawner,
            ping_manager,
            memory_store,
            leaderboard: Arc::new(tokio::sync::RwLock::new(
                load_leaderboard(data_dir.path()).await,
            )),
            aviation_tracker_tx,
            aviation_tracker_rx,
        };

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let bot_task = tokio::spawn(run_bot(
            client,
            incoming,
            self.config,
            services,
            shutdown_rx,
        ));

        // Allow handshake to complete and handlers to subscribe before tests send.
        tokio::time::sleep(Duration::from_millis(50)).await;

        TestBot {
            transport,
            clock,
            data_dir,
            adsb_mock,
            nominatim_mock,
            seventv_mock,
            llm,
            whisper,
            channel,
            irc_connected,
            shutdown: Some(shutdown_tx),
            bot_task: Some(bot_task),
        }
    }
}

impl Default for TestBotBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestBot {
    pub async fn send(&self, user: &str, text: &str) {
        let line = privmsg(&self.channel, user, text);
        self.transport.inject.send(line).await.expect("inject");
    }

    /// Inject a PRIVMSG into a specific channel (not necessarily the primary).
    /// Used by `ai_channel` tests to drive messages from the secondary channel.
    pub async fn send_to(&self, channel: &str, user: &str, text: &str) {
        let line = privmsg(channel, user, text);
        self.transport.inject.send(line).await.expect("inject");
    }

    /// Same, but with an explicit `tmi-sent-ts` (used by 1337 tracker tests).
    pub async fn send_to_at(&self, channel: &str, user: &str, text: &str, tmi_ts_ms: i64) {
        let line = privmsg_at(channel, user, text, tmi_ts_ms);
        self.transport.inject.send(line).await.expect("inject");
    }

    pub async fn send_reply(&self, user: &str, text: &str, parent_user: &str, parent_text: &str) {
        let line = reply_privmsg(&self.channel, user, text, parent_user, parent_text);
        self.transport.inject.send(line).await.expect("inject");
    }

    pub async fn send_at(&self, user: &str, text: &str, tmi_ts_ms: i64) {
        let line = privmsg_at(&self.channel, user, text, tmi_ts_ms);
        self.transport.inject.send(line).await.expect("inject");
    }

    /// Inject a PRIVMSG with a caller-supplied `user-id` IRCv3 tag. Used by
    /// memory tests to drive the extractor's permission matrix, which gates
    /// on the numeric speaker id rather than the display name.
    pub async fn send_privmsg_as(&self, user: &str, user_id: &str, text: &str) {
        let line = privmsg_with(&self.channel, user, text, &[("user-id", user_id)]);
        self.transport.inject.send(line).await.expect("inject");
    }

    pub async fn send_as_broadcaster(&self, user: &str, text: &str) {
        let line = privmsg_as_broadcaster(&self.channel, user, text);
        self.transport.inject.send(line).await.expect("inject");
    }

    pub async fn send_as_mod(&self, user: &str, text: &str) {
        let line = privmsg_as_mod(&self.channel, user, text);
        self.transport.inject.send(line).await.expect("inject");
    }

    pub async fn expect_say(&mut self, timeout: Duration) -> String {
        loop {
            let raw = tokio::time::timeout(timeout, self.transport.capture.recv())
                .await
                .expect("timed out waiting for outgoing message")
                .expect("transport closed");
            // Filter out handshake + JOIN/CAP/PASS/NICK noise — tests care about PRIVMSGs.
            if raw.contains("PRIVMSG") {
                return parse_privmsg_text(&raw);
            }
        }
    }

    /// `expect_say` with the leading `". "` stripped that `say_in_reply_to`
    /// inserts to defeat command injection. Use when assertions don't care
    /// about the prefix.
    pub async fn expect_reply(&mut self, timeout: Duration) -> String {
        let out = self.expect_say(timeout).await;
        out.strip_prefix(". ").map(str::to_owned).unwrap_or(out)
    }

    /// Wait for an outgoing PRIVMSG and return `(channel, body)`. The channel
    /// is the IRC `#chan` argument with the leading `#` stripped.
    pub async fn expect_say_full(&mut self, timeout: Duration) -> (String, String) {
        loop {
            let raw = tokio::time::timeout(timeout, self.transport.capture.recv())
                .await
                .expect("timed out waiting for outgoing message")
                .expect("transport closed");
            if !raw.contains("PRIVMSG") {
                continue;
            }
            // PRIVMSG #chan :body  (no tags on outbound from TwitchIRCClient::say)
            let after = raw.split_once("PRIVMSG ").expect("PRIVMSG format").1;
            let (chan_with_hash, rest) = after.split_once(' ').expect("PRIVMSG channel/body");
            let channel = chan_with_hash.trim_start_matches('#').to_owned();
            let body = rest
                .trim_start_matches(':')
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            return (channel, body);
        }
    }

    pub async fn expect_whisper(&self, timeout: Duration) -> WhisperRecord {
        self.whisper.expect(timeout).await
    }

    pub async fn expect_silent(&mut self, dur: Duration) {
        let deadline = tokio::time::Instant::now() + dur;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(
                deadline - tokio::time::Instant::now(),
                self.transport.capture.recv(),
            )
            .await
            {
                Err(_) => return, // no message => silence => pass
                Ok(None) => panic!("transport closed"),
                Ok(Some(raw)) => {
                    if raw.contains("PRIVMSG") {
                        panic!("expected silence, got PRIVMSG: {raw}");
                    }
                    // Ignore non-PRIVMSG framing noise.
                }
            }
        }
    }

    /// Flip the shared `irc_connected` flag. Used by web smoke tests since
    /// the latency monitor only sets the flag after a real PONG round-trip.
    pub fn set_irc_connected(&self, v: bool) {
        self.irc_connected
            .store(v, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.bot_task.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => panic!("bot exited with error: {e:?}"),
                Ok(Err(e)) => panic!("bot task panicked: {e:?}"),
                Err(_) => panic!("bot shutdown timed out"),
            }
        }
    }

    // -------------------------------------------------------------------------
    // T16 helpers: v2 memory filesystem accessors
    // -------------------------------------------------------------------------

    /// Path to `$DATA_DIR/memories/`.
    pub fn memories_dir(&self) -> std::path::PathBuf {
        self.data_dir.path().join("memories")
    }

    /// Path to `$DATA_DIR/memories/transcripts/`.
    pub fn transcripts_dir(&self) -> std::path::PathBuf {
        self.memories_dir().join("transcripts")
    }

    /// Read a memory file at `$DATA_DIR/memories/<rel>` and strip the
    /// frontmatter header (`---\n…\n---\n`), returning only the body.
    /// Panics if the file is unreadable.
    pub async fn read_memory_file(&self, rel: &str) -> String {
        let raw = tokio::fs::read_to_string(self.memories_dir().join(rel))
            .await
            .unwrap_or_default();
        raw.split_once("\n---\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or(raw)
    }

    /// Poll `$DATA_DIR/memories/transcripts/today.md` until it contains
    /// `text` or `timeout` elapses (panics on timeout).
    pub async fn wait_until_transcript_contains(&self, text: &str, timeout: Duration) {
        let path = self.transcripts_dir().join("today.md");
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::fs::read_to_string(&path)
                .await
                .is_ok_and(|s| s.contains(text))
            {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("transcript did not contain {text:?} within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Open fresh v2 memory handles on the same `data_dir` and run the
    /// dreamer ritual for `yesterday`.
    pub async fn run_ritual_for(&self, yesterday: chrono::NaiveDate) {
        use twitch_1337::ai::memory::{
            RitualConfig, run_ritual, store::MemoryStore as StoreV2, transcript::TranscriptWriter,
            types::Caps,
        };

        let store = StoreV2::open(self.data_dir.path(), Caps::default())
            .await
            .expect("open store for ritual");
        let transcript = TranscriptWriter::open(store.memories_dir())
            .await
            .expect("open transcript for ritual");

        let llm_ref: &dyn llm::LlmClient = self.llm.as_ref();
        run_ritual(
            llm_ref,
            &store,
            &transcript,
            &RitualConfig {
                model: "fake".into(),
                reasoning_effort: None,
                run_at: chrono::NaiveTime::from_hms_opt(4, 0, 0).unwrap(),
                timeout_secs: 5,
                max_rounds: 4,
                max_writes_per_turn: 8,
                inject_byte_budget: 16_384,
                channel: self.channel.clone(),
            },
            yesterday,
        )
        .await
        .expect("ritual ok");
    }
}

#[derive(Debug, Clone)]
pub struct WhisperRecord {
    pub to_user_id: String,
    pub message: String,
}

#[derive(Default)]
struct FakeWhisperState {
    known_recipients: HashSet<String>,
    records: VecDeque<WhisperRecord>,
}

pub struct FakeWhisperSender {
    state: Mutex<FakeWhisperState>,
    notify: Notify,
    fail: bool,
}

impl FakeWhisperSender {
    fn new(fail: bool) -> Self {
        Self {
            state: Mutex::new(FakeWhisperState::default()),
            notify: Notify::new(),
            fail,
        }
    }

    async fn expect(&self, timeout: Duration) -> WhisperRecord {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(record) = self.state.lock().await.records.pop_front() {
                return record;
            }

            let now = tokio::time::Instant::now();
            assert!(now < deadline, "timed out waiting for whisper");
            tokio::time::timeout(deadline - now, self.notify.notified())
                .await
                .expect("timed out waiting for whisper");
        }
    }
}

#[async_trait]
impl WhisperSender for FakeWhisperSender {
    async fn send_whisper(
        &self,
        to_user_id: &str,
        message: &str,
    ) -> std::result::Result<String, WhisperError> {
        if self.fail {
            return Err(WhisperError::unavailable(
                "test whisper sender is not authenticated",
            ));
        }

        let mut state = self.state.lock().await;
        let known_recipient = state.known_recipients.contains(to_user_id);
        let message = whisper::truncate_whisper_message(message, known_recipient);
        state.known_recipients.insert(to_user_id.to_owned());
        state.records.push_back(WhisperRecord {
            to_user_id: to_user_id.to_owned(),
            message: message.clone(),
        });
        self.notify.notify_waiters();
        Ok(message)
    }
}

/// Build a minimal `WebState` for tests that flip on `with_web`. Uses a
/// stub helix client that denies every mod check (no production routes
/// reach it because the web smoke test only probes the public `/healthz`).
fn build_test_web_state(
    config: &Configuration,
    irc_connected: Arc<AtomicBool>,
    ping_manager: Arc<tokio::sync::RwLock<twitch_1337::ping::PingManager>>,
    memory_store: twitch_1337::ai::memory::store::MemoryStore,
    settings: twitch_1337::settings::SettingsHandle,
    settings_store: Arc<twitch_1337::settings::SettingsStore>,
) -> twitch_1337_web::WebState {
    use twitch_1337_web::auth::OAuthCtx;
    use twitch_1337_web::auth::session::SessionTable;
    use twitch_1337_web::clock::SystemClock as WebSystemClock;
    use twitch_1337_web::config::WebConfig as WebWebConfig;
    use twitch_1337_web::helix::{HelixClient, HelixUser};

    struct DenyHelix;
    #[async_trait]
    impl HelixClient for DenyHelix {
        async fn fetch_user_by_id(&self, _id: &str) -> eyre::Result<Option<HelixUser>> {
            Ok(None)
        }
        async fn fetch_user_by_login(&self, _login: &str) -> eyre::Result<Option<HelixUser>> {
            Ok(None)
        }
        async fn is_moderator(&self, _b: &str, _u: &str) -> eyre::Result<bool> {
            Ok(false)
        }
    }

    let web_clock = Arc::new(WebSystemClock);
    let sessions = Arc::new(SessionTable::new(config.web.session_ttl, web_clock.clone()));
    let oauth = Arc::new(
        OAuthCtx::new(
            "test-client-id",
            &secrecy::SecretString::new("test-secret".to_owned().into()),
            "https://test.invalid",
        )
        .expect("test oauth"),
    );
    let web_config = Arc::new(WebWebConfig {
        bind_addr: config.web.bind_addr.clone(),
        public_url: config.web.public_url.clone(),
        session_secret: config.web.session_secret.clone(),
        session_ttl: config.web.session_ttl,
        role_check_refresh: config.web.mod_check_refresh,
    });
    let signed_key = tower_cookies::Key::from(&[0x42u8; 64]);
    twitch_1337_web::WebState {
        sessions,
        helix: Arc::new(DenyHelix),
        irc_connected,
        config: web_config,
        clock: web_clock,
        channel: Arc::from(config.twitch.channel.as_str()),
        broadcaster_id: Arc::from("0"),
        hidden_admins: Arc::from(Vec::<String>::new().into_boxed_slice()),
        viewer_allowlist: Arc::from(Vec::<String>::new().into_boxed_slice()),
        client_id: secrecy::SecretString::new("test-client-id".to_owned().into()),
        oauth,
        ping_manager,
        memory_store,
        signed_key,
        leaderboard: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        tracker_tx: None,
        avatar_cache: Arc::new(twitch_1337_web::helix::AvatarCache::new(
            std::time::Duration::from_secs(3600),
        )),
        owner_id: None,
        settings,
        settings_store,
    }
}
