use std::{
    collections::HashSet,
    fmt::{self, Display},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use reqwest::StatusCode;
use serde_json::json;
use tokio::{fs, sync::Mutex};
use tracing::warn;
use twitch_irc::login::LoginCredentials;

use crate::util::{APP_USER_AGENT, truncate_response};

pub const FIRST_WHISPER_MAX_CHARS: usize = 500;
pub const WHISPER_MAX_CHARS: usize = 10_000;

#[derive(Debug)]
pub enum WhisperError {
    MissingToken,
    Credentials(String),
    EmptyMessage,
    Api { status: StatusCode, body: String },
    Http(reqwest::Error),
    Unavailable(String),
}

impl WhisperError {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable(reason.into())
    }
}

impl Display for WhisperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToken => write!(f, "missing user access token for whisper API"),
            Self::Credentials(error) => write!(f, "failed to obtain Twitch credentials: {error}"),
            Self::EmptyMessage => write!(f, "whisper message is empty"),
            Self::Api { status, body } => {
                write!(f, "Twitch whisper API returned {status}: {body}")
            }
            Self::Http(error) => write!(f, "Twitch whisper API request failed: {error}"),
            Self::Unavailable(reason) => write!(f, "whisper unavailable: {reason}"),
        }
    }
}

impl std::error::Error for WhisperError {}

impl From<reqwest::Error> for WhisperError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

#[async_trait]
pub trait WhisperSender: Send + Sync {
    /// Sends a whisper and returns the actual text submitted to Twitch.
    async fn send_whisper(&self, to_user_id: &str, message: &str) -> Result<String, WhisperError>;
}

pub fn truncate_whisper_message(message: &str, known_recipient: bool) -> String {
    let limit = if known_recipient {
        WHISPER_MAX_CHARS
    } else {
        FIRST_WHISPER_MAX_CHARS
    };
    truncate_response(message, limit)
}

pub struct HelixWhisperSender<L>
where
    L: LoginCredentials,
{
    http: reqwest::Client,
    credentials: L,
    client_id: String,
    from_user_id: String,
    known_recipients: Arc<Mutex<HashSet<String>>>,
    store_path: PathBuf,
}

impl<L> HelixWhisperSender<L>
where
    L: LoginCredentials,
{
    pub async fn new(
        credentials: L,
        client_id: String,
        from_user_id: String,
        data_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let store_path = data_dir.as_ref().join("whisper_recipients.ron");
        let known_recipients = load_known_recipients(&store_path).await?;
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .build()
            .wrap_err("failed to build Twitch whisper HTTP client")?;

        Ok(Self {
            http,
            credentials,
            client_id,
            from_user_id,
            known_recipients: Arc::new(Mutex::new(known_recipients)),
            store_path,
        })
    }

    async fn remember_recipient(&self, to_user_id: &str) {
        let snapshot = {
            let mut guard = self.known_recipients.lock().await;
            if !guard.insert(to_user_id.to_owned()) {
                return;
            }
            guard.clone()
        };

        if let Err(error) = save_known_recipients(&self.store_path, &snapshot).await {
            warn!(
                error = ?error,
                path = %self.store_path.display(),
                "Failed to persist successful whisper recipient"
            );
        }
    }
}

#[async_trait]
impl<L> WhisperSender for HelixWhisperSender<L>
where
    L: LoginCredentials,
{
    async fn send_whisper(&self, to_user_id: &str, message: &str) -> Result<String, WhisperError> {
        let known_recipient = self.known_recipients.lock().await.contains(to_user_id);
        let message = truncate_whisper_message(message, known_recipient);
        if message.trim().is_empty() {
            return Err(WhisperError::EmptyMessage);
        }

        let credentials = self
            .credentials
            .get_credentials()
            .await
            .map_err(|error| WhisperError::Credentials(error.to_string()))?;
        let token = credentials.token.ok_or(WhisperError::MissingToken)?;

        let response = self
            .http
            .post("https://api.twitch.tv/helix/whispers")
            .query(&[
                ("from_user_id", self.from_user_id.as_str()),
                ("to_user_id", to_user_id),
            ])
            .header("Client-Id", &self.client_id)
            .bearer_auth(token)
            .json(&json!({ "message": message }))
            .send()
            .await?;

        if response.status() == StatusCode::NO_CONTENT {
            self.remember_recipient(to_user_id).await;
            return Ok(message);
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(WhisperError::Api { status, body })
    }
}

async fn load_known_recipients(path: &Path) -> Result<HashSet<String>> {
    match fs::read_to_string(path).await {
        Ok(contents) => {
            ron::from_str(&contents).wrap_err("failed to parse whisper recipient store")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashSet::new()),
        Err(error) => Err(error).wrap_err("failed to read whisper recipient store"),
    }
}

async fn save_known_recipients(path: &Path, recipients: &HashSet<String>) -> Result<()> {
    crate::util::persist::atomic_save_ron_async(recipients, path)
        .await
        .wrap_err("Failed to save whisper recipients")
}
