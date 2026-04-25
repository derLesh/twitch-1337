//! One-stop integration test fixture. Assembles FakeTransport + FakeClock +
//! FakeLlm + wiremock + tempdir into a live bot running behind `run_bot`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use eyre::Result;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::MockServer;

use twitch_1337::{
    PersonalBest, Services,
    aviation::AviationClient,
    config::{AiConfig, Configuration},
    llm::LlmClient,
    run_bot,
};
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::{ClientConfig, TwitchIRCClient};

use super::fake_clock::FakeClock;
use super::fake_llm::FakeLlm;
use super::fake_transport::{self, FakeTransport, TransportHandle};
use super::irc_line::{
    parse_privmsg_text, privmsg, privmsg_as_broadcaster, privmsg_as_mod, privmsg_with,
};

pub struct TestBot {
    pub transport: TransportHandle,
    pub clock: Arc<FakeClock>,
    pub data_dir: TempDir,
    pub adsb_mock: MockServer,
    pub nominatim_mock: MockServer,
    pub seventv_mock: MockServer,
    pub llm: Arc<FakeLlm>,
    pub channel: String,
    shutdown: Option<oneshot::Sender<()>>,
    bot_task: Option<JoinHandle<Result<()>>>,
}

pub struct TestBotBuilder {
    config: Configuration,
    now: DateTime<Utc>,
    seeded_leaderboard: Option<HashMap<String, PersonalBest>>,
}

impl TestBotBuilder {
    pub fn new() -> Self {
        Self {
            config: Configuration::test_default(),
            now: "2026-04-18T11:00:00Z".parse().unwrap(),
            seeded_leaderboard: None,
        }
    }

    pub fn with_ai(mut self) -> Self {
        if self.config.ai.is_none() {
            self.config.ai = Some(AiConfig {
                backend: twitch_1337::config::AiBackend::OpenAi,
                api_key: Some(secrecy::SecretString::new("test".into())),
                base_url: None,
                model: "test-model".into(),
                system_prompt: "test prompt".into(),
                instruction_template: "{message}".into(),
                timeout: 30,
                reasoning_effort: None,
                history_length: twitch_1337::DEFAULT_HISTORY_LENGTH,
                history_prefill: None,
                memory: twitch_1337::config::MemoryConfigSection::default(),
                extraction: twitch_1337::config::ExtractionConfigSection::default(),
                consolidation: twitch_1337::config::ConsolidationConfigSection::default(),
                emotes: twitch_1337::config::AiEmotesConfigSection::default(),
                max_memories: None,
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

    pub fn with_seeded_leaderboard(mut self, entries: HashMap<String, PersonalBest>) -> Self {
        self.seeded_leaderboard = Some(entries);
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
        if let Some(ai) = self.config.ai.as_mut()
            && ai.emotes.enabled
            && ai.emotes.base_url.is_none()
        {
            ai.emotes.base_url = Some(seventv_mock.uri());
        }
        let llm = Arc::new(FakeLlm::new());
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

        let http = reqwest::Client::new();
        let aviation = AviationClient::new_with_base_url(
            adsb_mock.uri(),
            adsb_mock.uri(),      // adsbdb shares the same mock server in tests
            nominatim_mock.uri(), // nominatim
            http,
        );

        let services = Services {
            clock: clock.clone(),
            llm: self
                .config
                .ai
                .is_some()
                .then(|| llm.clone() as Arc<dyn LlmClient>),
            aviation: Some(aviation),
            data_dir: data_dir.path().to_path_buf(),
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
            channel,
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
}
