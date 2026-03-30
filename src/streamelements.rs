use eyre::{Result, WrapErr as _};
use reqwest::header::{self, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::APP_USER_AGENT;

/// A StreamElements bot command with all its configuration.
///
/// Commands can be triggered by users in chat and have various settings
/// like cooldowns, access levels, and the reply text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub cooldown: CommandCooldown,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub enabled: bool,
    pub enabled_online: bool,
    pub enabled_offline: bool,
    pub hidden: bool,
    pub cost: i64,
    #[serde(rename = "type")]
    pub command_type: String,
    pub access_level: i64,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    pub reply: String,
    pub command: String,
    pub channel: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Cooldown settings for a command.
///
/// Defines how long users must wait between command uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCooldown {
    /// Per-user cooldown in seconds
    pub user: i64,
    /// Global cooldown in seconds (affects all users)
    pub global: i64,
}

/// Error response from the StreamElements API.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    status_code: i64,
    error: String,
    message: String,
    details: Vec<ErrorDetail>,
}

/// Detailed error information for a specific field.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    path: Vec<String>,
    message: String,
}

/// HTTP client for the StreamElements API.
///
/// Handles authentication and provides methods to interact with bot commands.
#[derive(Debug, Clone)]
pub struct SEClient(reqwest::Client);

impl SEClient {
    /// Creates a new StreamElements API client with the given authentication token.
    ///
    /// # Errors
    ///
    /// Returns an error if the token format is invalid or the HTTP client cannot be built.
    #[instrument(skip(token))]
    // TODO: make secret
    pub fn new(token: &str) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        auth_value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth_value);
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .default_headers(headers)
            .build()
            .wrap_err("Failed to build HTTP Client")?;

        Ok(Self(http))
    }

    /// Retrieves all bot commands for a given channel.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or the response cannot be parsed.
    #[instrument]
    pub async fn get_all_commands(&self, channel_id: &str) -> Result<Vec<Command>> {
        let commands = self
            .0
            .get(format!(
                "https://api.streamelements.com/kappa/v2/bot/commands/{channel_id}"
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<Command>>()
            .await?;

        Ok(commands)
    }

    /// Updates an existing bot command.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or the response cannot be parsed.
    #[instrument(skip(command))]
    pub async fn update_command(&self, channel_id: &str, command: Command) -> Result<()> {
        self.0
            .put(format!(
                "https://api.streamelements.com/kappa/v2/bot/commands/{channel_id}/{}",
                command.id
            ))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Command>()
            .await?;

        Ok(())
    }
}
