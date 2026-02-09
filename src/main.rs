use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{TimeDelta, Timelike, Utc};
use color_eyre::eyre::{self, Result, WrapErr, bail};
use rand::seq::IndexedRandom as _;
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, broadcast, mpsc::UnboundedReceiver},
    time::{Duration, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{RefreshingLoginCredentials, TokenStorage, UserAccessToken},
    message::{NoticeMessage, PrivmsgMessage, ServerMessage},
};

/// StreamElements API client and types for managing bot commands.
///
/// This module provides an HTTP client for interacting with the StreamElements API
/// to retrieve and update bot commands. Used primarily for managing ping commands
/// that notify users about community game sessions.
mod streamelements {
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
    #[derive(Debug)]
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
}

/// SimBrief API client for fetching flight plans and aircraft data.
///
/// This module provides an HTTP client for interacting with the SimBrief API
/// to retrieve flight plans, available aircraft types, and generate dispatch URLs.
/// No authentication is required for these endpoints.
#[allow(dead_code)]
mod simbrief {
    use std::collections::HashMap;

    use eyre::{Result, WrapErr as _};
    use serde::Deserialize;
    use tracing::instrument;

    use crate::APP_USER_AGENT;

    /// Airport information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct Airport {
        /// ICAO airport code (e.g., "EDDF")
        pub icao_code: String,
        /// Full airport name
        #[serde(default)]
        pub name: String,
    }

    /// General flight information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct GeneralInfo {
        /// Flight route string
        #[serde(default)]
        pub route: String,
        /// Flight number (e.g., "DLH123")
        #[serde(default)]
        pub flight_number: String,
        /// Airline ICAO code (e.g., "DLH")
        #[serde(default)]
        pub icao_airline: String,
        /// Total air distance in nautical miles
        #[serde(default)]
        pub air_distance: String,
        /// Route distance in nautical miles
        #[serde(default)]
        pub route_distance: String,
    }

    /// Fuel information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct FuelInfo {
        /// Planned ramp fuel in pounds
        #[serde(default)]
        pub plan_ramp: String,
        /// Enroute burn in pounds
        #[serde(default)]
        pub enroute_burn: String,
        /// Reserve fuel in pounds
        #[serde(default)]
        pub reserve: String,
        /// Alternate fuel in pounds
        #[serde(default)]
        pub alternate_burn: String,
        /// Contingency fuel in pounds
        #[serde(default)]
        pub contingency: String,
        /// Taxi out fuel in pounds
        #[serde(default)]
        pub taxi: String,
    }

    /// Weight information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct WeightInfo {
        /// Payload weight in pounds
        #[serde(default)]
        pub payload: String,
        /// Zero fuel weight in pounds
        #[serde(default)]
        pub est_zfw: String,
        /// Passenger count
        #[serde(default)]
        pub pax_count: String,
        /// Cargo weight in pounds
        #[serde(default)]
        pub cargo: String,
    }

    /// Time information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct TimeInfo {
        /// Estimated time enroute in seconds
        #[serde(default)]
        pub est_time_enroute: String,
        /// Scheduled out time (gate departure) as Unix timestamp
        #[serde(default)]
        pub sched_out: String,
        /// Scheduled off time (takeoff) as Unix timestamp
        #[serde(default)]
        pub sched_off: String,
        /// Scheduled on time (landing) as Unix timestamp
        #[serde(default)]
        pub sched_on: String,
        /// Scheduled in time (gate arrival) as Unix timestamp
        #[serde(default)]
        pub sched_in: String,
    }

    /// Aircraft information from SimBrief flight plan.
    #[derive(Debug, Clone, Deserialize)]
    pub struct AircraftInfo {
        /// Aircraft ICAO type code (e.g., "B738")
        #[serde(default)]
        pub icaocode: String,
        /// Aircraft name
        #[serde(default)]
        pub name: String,
        /// Aircraft registration
        #[serde(default)]
        pub reg: String,
    }

    /// A SimBrief Operational Flight Plan (OFP).
    ///
    /// Contains the essential fields from SimBrief's flight plan response.
    /// The full API response is very large; only the most useful fields are parsed.
    #[derive(Debug, Clone, Deserialize)]
    pub struct FlightPlan {
        /// Origin airport
        pub origin: Airport,
        /// Destination airport
        pub destination: Airport,
        /// General flight information
        pub general: GeneralInfo,
        /// Fuel planning data
        pub fuel: FuelInfo,
        /// Weight and balance data
        pub weights: WeightInfo,
        /// Time scheduling data
        pub times: TimeInfo,
        /// Aircraft information
        #[serde(default)]
        pub aircraft: Option<AircraftInfo>,
    }

    /// Wrapper for SimBrief API response.
    #[derive(Debug, Clone, Deserialize)]
    struct SimBriefResponse {
        #[serde(flatten)]
        plan: FlightPlan,
    }

    /// Information about an available aircraft type in SimBrief.
    #[derive(Debug, Clone, Deserialize)]
    pub struct AircraftTypeInfo {
        /// Aircraft name (e.g., "Boeing 737-800")
        #[serde(default)]
        pub name: String,
        /// SimBrief accuracy rating
        #[serde(default)]
        pub ac_data_accuracy: String,
    }

    /// Information about an OFP layout in SimBrief.
    #[derive(Debug, Clone, Deserialize)]
    pub struct LayoutInfo {
        /// Short name for the layout
        #[serde(default)]
        pub name_short: String,
        /// Full name for the layout
        #[serde(default)]
        pub name_long: String,
    }

    /// List of available aircraft types and OFP layouts from SimBrief.
    #[derive(Debug, Clone, Deserialize)]
    pub struct AircraftList {
        /// Map of aircraft type code to aircraft info
        #[serde(default)]
        pub aircraft: HashMap<String, AircraftTypeInfo>,
        /// Map of layout ID to layout info
        #[serde(default)]
        pub layouts: HashMap<String, LayoutInfo>,
    }

    /// Parameters for generating a SimBrief dispatch URL.
    #[derive(Debug, Clone, Default)]
    pub struct DispatchParams {
        /// Origin airport ICAO code (required)
        pub origin: String,
        /// Destination airport ICAO code (required)
        pub destination: String,
        /// Aircraft type code (e.g., "B738", "A320")
        pub aircraft: String,
        /// Route string (optional)
        pub route: Option<String>,
        /// Alternate airport ICAO code (optional)
        pub alternate: Option<String>,
        /// Airline ICAO code (optional)
        pub airline: Option<String>,
        /// Flight number (optional)
        pub flight_number: Option<String>,
    }

    /// HTTP client for the SimBrief API.
    ///
    /// Provides methods to fetch flight plans, aircraft data, and generate dispatch URLs.
    /// No authentication is required for SimBrief's public API endpoints.
    #[derive(Debug, Clone)]
    pub struct SimBriefClient(reqwest::Client);

    impl SimBriefClient {
        /// Creates a new SimBrief API client.
        ///
        /// # Errors
        ///
        /// Returns an error if the HTTP client cannot be built.
        #[instrument]
        pub fn new() -> Result<Self> {
            let http = reqwest::Client::builder()
                .user_agent(APP_USER_AGENT)
                .build()
                .wrap_err("Failed to build HTTP Client")?;

            Ok(Self(http))
        }

        /// Fetches the latest flight plan for a SimBrief user.
        ///
        /// The `user` parameter can be either:
        /// - A numeric user ID (e.g., "123456")
        /// - A username/pilot ID (e.g., "pilotname")
        ///
        /// The function auto-detects which format is used and makes the appropriate request.
        ///
        /// # Errors
        ///
        /// Returns an error if:
        /// - The user is not found
        /// - The user has no flight plans
        /// - The API request fails
        /// - The response cannot be parsed
        #[instrument]
        pub async fn get_latest_flight_plan(&self, user: &str) -> Result<FlightPlan> {
            // Auto-detect if user is numeric (userid) or string (username)
            let param = if user.chars().all(|c| c.is_ascii_digit()) {
                format!("userid={user}")
            } else {
                format!("username={user}")
            };

            let url = format!(
                "https://www.simbrief.com/api/xml.fetcher.php?{param}&json=1"
            );

            let response = self
                .0
                .get(&url)
                .send()
                .await
                .wrap_err("Failed to send request to SimBrief API")?;

            // SimBrief returns 400 for user not found or no flight plans
            if response.status().is_client_error() {
                return Err(eyre::eyre!(
                    "SimBrief user '{}' not found or has no flight plans",
                    user
                ));
            }

            response
                .error_for_status()
                .wrap_err("SimBrief API returned an error")?
                .json::<SimBriefResponse>()
                .await
                .map(|r| r.plan)
                .wrap_err("Failed to parse SimBrief flight plan response")
        }

        /// Fetches the list of available aircraft types and OFP layouts.
        ///
        /// Returns information about all aircraft types supported by SimBrief
        /// (approximately 286 aircraft) and available OFP layout formats (28 layouts).
        ///
        /// # Errors
        ///
        /// Returns an error if the API request fails or the response cannot be parsed.
        #[instrument]
        pub async fn get_available_aircraft(&self) -> Result<AircraftList> {
            let url = "http://www.simbrief.com/api/inputs.list.json";

            self.0
                .get(url)
                .send()
                .await
                .wrap_err("Failed to send request to SimBrief API")?
                .error_for_status()
                .wrap_err("SimBrief API returned an error")?
                .json::<AircraftList>()
                .await
                .wrap_err("Failed to parse SimBrief aircraft list response")
        }

        /// Generates a SimBrief dispatch URL for creating a new flight plan.
        ///
        /// The returned URL can be opened in a browser to complete flight planning.
        /// This does not make any API calls; it simply constructs the URL.
        ///
        /// # Example
        ///
        /// ```ignore
        /// let url = client.create_dispatch_url(DispatchParams {
        ///     origin: "EDDF".into(),
        ///     destination: "LIRF".into(),
        ///     aircraft: "A320".into(),
        ///     ..Default::default()
        /// });
        /// // Returns: https://www.simbrief.com/system/dispatch.php?orig=EDDF&dest=LIRF&type=A320
        /// ```
        pub fn create_dispatch_url(&self, params: DispatchParams) -> String {
            let mut url = format!(
                "https://www.simbrief.com/system/dispatch.php?orig={}&dest={}&type={}",
                params.origin, params.destination, params.aircraft
            );

            if let Some(route) = &params.route {
                // URL-encode the route as it may contain spaces and special characters
                let encoded_route = urlencoding::encode(route);
                url.push_str(&format!("&route={encoded_route}"));
            }

            if let Some(alternate) = &params.alternate {
                url.push_str(&format!("&altn={alternate}"));
            }

            if let Some(airline) = &params.airline {
                url.push_str(&format!("&airline={airline}"));
            }

            if let Some(flight_number) = &params.flight_number {
                url.push_str(&format!("&fltnum={flight_number}"));
            }

            url
        }
    }
}

/// OpenRouter API client for AI-powered chat responses.
///
/// This module provides an HTTP client for interacting with the OpenRouter API
/// (OpenAI-compatible) to generate responses with optional tool/function calling support.
mod openrouter {
    use eyre::{Result, WrapErr as _};
    use reqwest::header::{self, HeaderValue};
    use serde::{Deserialize, Serialize};
    use tracing::{debug, instrument};

    use crate::APP_USER_AGENT;

    /// A message in the conversation.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Message {
        pub role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_calls: Option<Vec<ToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_call_id: Option<String>,
    }

    /// A tool call requested by the model.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ToolCall {
        pub id: String,
        #[serde(rename = "type")]
        pub call_type: String,
        pub function: FunctionCall,
    }

    /// Function call details within a tool call.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct FunctionCall {
        pub name: String,
        /// Arguments as a JSON string (needs to be parsed)
        pub arguments: String,
    }

    /// Tool definition for the API (OpenAI format).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Tool {
        #[serde(rename = "type")]
        pub tool_type: String,
        pub function: ToolFunction,
    }

    /// Function definition within a tool.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ToolFunction {
        pub name: String,
        pub description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub parameters: Option<serde_json::Value>,
    }

    /// Request body for the OpenRouter chat/completions endpoint.
    #[derive(Debug, Clone, Serialize)]
    pub struct ChatCompletionRequest {
        pub model: String,
        pub messages: Vec<Message>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<Tool>>,
    }

    /// A choice in the chat completion response.
    #[derive(Debug, Clone, Deserialize)]
    pub struct Choice {
        pub message: Message,
        pub finish_reason: Option<String>,
    }

    /// Response from the OpenRouter chat/completions endpoint.
    #[derive(Debug, Clone, Deserialize)]
    pub struct ChatCompletionResponse {
        pub choices: Vec<Choice>,
    }

    /// HTTP client for the OpenRouter API.
    #[derive(Debug, Clone)]
    pub struct OpenRouterClient {
        http: reqwest::Client,
        model: String,
    }

    impl OpenRouterClient {
        /// Creates a new OpenRouter API client.
        ///
        /// # Errors
        ///
        /// Returns an error if the HTTP client cannot be built.
        #[instrument(skip(api_key))]
        pub fn new(api_key: &str, model: &str) -> Result<Self> {
            let mut headers = header::HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );

            // OpenRouter uses Bearer token auth
            let mut auth_value = HeaderValue::from_str(&format!("Bearer {}", api_key))
                .wrap_err("Invalid API key format")?;
            auth_value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, auth_value);

            // Required OpenRouter headers
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://github.com/chronophylos/twitch-1337"),
            );
            headers.insert("X-Title", HeaderValue::from_static("twitch-1337"));

            let http = reqwest::Client::builder()
                .user_agent(APP_USER_AGENT)
                .default_headers(headers)
                .build()
                .wrap_err("Failed to build HTTP Client")?;

            Ok(Self {
                http,
                model: model.to_string(),
            })
        }

        /// Sends a chat completion request to OpenRouter.
        ///
        /// # Errors
        ///
        /// Returns an error if the API request fails or the response cannot be parsed.
        #[instrument(skip(self, request))]
        pub async fn chat_completion(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse> {
            let url = "https://openrouter.ai/api/v1/chat/completions";

            debug!(model = %self.model, "Sending request to OpenRouter API");

            let response = self
                .http
                .post(url)
                .json(&request)
                .send()
                .await
                .wrap_err("Failed to send request to OpenRouter API")?;

            if !response.status().is_success() {
                let status = response.status();
                let error_body = response.text().await.unwrap_or_default();
                return Err(eyre::eyre!(
                    "OpenRouter API error (status {}): {}",
                    status,
                    error_body
                ));
            }

            response
                .json::<ChatCompletionResponse>()
                .await
                .wrap_err("Failed to parse OpenRouter API response")
        }

        /// Returns the model name.
        pub fn model(&self) -> &str {
            &self.model
        }
    }
}

use crate::openrouter::{
    ChatCompletionRequest, Message, OpenRouterClient, Tool, ToolFunction,
};
use crate::simbrief::{DispatchParams, SimBriefClient};
use crate::streamelements::SEClient;

/// Type alias for the authenticated Twitch IRC client
type AuthenticatedTwitchClient =
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>;

const TARGET_HOUR: u32 = 13;
const TARGET_MINUTE: u32 = 37;

/// Maximum number of unique users to track (prevents unbounded memory growth)
const MAX_USERS: usize = 10_000;

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TwitchConfiguration {
    channel: String,
    username: String,
    #[serde(serialize_with = "serialize_secret_string")]
    refresh_token: SecretString,
    #[serde(serialize_with = "serialize_secret_string")]
    client_id: SecretString,
    #[serde(serialize_with = "serialize_secret_string")]
    client_secret: SecretString,
    expected_latency: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StreamelementsConfig {
    #[serde(serialize_with = "serialize_secret_string")]
    api_token: SecretString,
    channel_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OpenRouterConfig {
    #[serde(serialize_with = "serialize_secret_string")]
    api_key: SecretString,
    /// OpenRouter model to use (default: "google/gemini-2.0-flash-exp:free")
    #[serde(default = "default_openrouter_model")]
    model: String,
}

fn default_openrouter_model() -> String {
    "google/gemini-2.0-flash-exp:free".to_string()
}

/// Configuration for a scheduled message loaded from config.toml.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScheduleConfig {
    name: String,
    message: String,
    /// Interval in "hh:mm" format (e.g., "01:30" for 1 hour 30 minutes)
    interval: String,
    /// Start date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    start_date: Option<String>,
    /// End date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    end_date: Option<String>,
    /// Daily active time start in HH:MM format
    #[serde(default)]
    active_time_start: Option<String>,
    /// Daily active time end in HH:MM format
    #[serde(default)]
    active_time_end: Option<String>,
    /// Whether the schedule is enabled (default: true)
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Configuration {
    twitch: TwitchConfiguration,
    streamelements: StreamelementsConfig,
    #[serde(default)]
    openrouter: Option<OpenRouterConfig>,
    #[serde(default)]
    schedules: Vec<ScheduleConfig>,
}

fn serialize_secret_string<S>(secret: &SecretString, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(secret.expose_secret())
}

impl Configuration {
    fn validate(&self) -> Result<()> {
        if self.twitch.channel.trim().is_empty() {
            bail!("twitch.channel cannot be empty");
        }

        if self.twitch.username.trim().is_empty() {
            bail!("twitch.username cannot be empty");
        }

        if self.twitch.expected_latency > 1000 {
            bail!("twitch.expected_latency must be <= 1000ms (got {})", self.twitch.expected_latency);
        }

        if self.streamelements.channel_id.trim().is_empty() {
            bail!("streamelements.channel_id cannot be empty");
        }

        // Validate each schedule config
        for schedule in &self.schedules {
            if schedule.name.trim().is_empty() {
                bail!("Schedule name cannot be empty");
            }
            if schedule.message.trim().is_empty() {
                bail!("Schedule '{}' message cannot be empty", schedule.name);
            }
            if schedule.interval.trim().is_empty() {
                bail!("Schedule '{}' interval cannot be empty", schedule.name);
            }
            // Validate interval format by parsing it
            database::Schedule::parse_interval(&schedule.interval)
                .wrap_err_with(|| format!("Schedule '{}' has invalid interval format", schedule.name))?;
        }

        Ok(())
    }
}

/// Calculates the next occurrence of a daily time in Europe/Berlin timezone.
///
/// If the specified time has already passed today, returns tomorrow's occurrence.
fn calculate_next_occurrence(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
    let berlin_now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    // Create target time today in Berlin timezone
    let mut target = berlin_now
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .expect("Invalid hour/minute for Berlin time")
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()
        .expect("Ambiguous time during DST transition");

    // If target time has already passed today, schedule for tomorrow
    if target <= berlin_now {
        target = (berlin_now + chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .expect("Invalid hour/minute for Berlin time")
            .and_local_timezone(chrono_tz::Europe::Berlin)
            .single()
            .expect("Ambiguous time during DST transition");
    }

    target.with_timezone(&Utc)
}

/// Sleeps until the next occurrence of a daily time in Europe/Berlin timezone.
#[instrument]
async fn wait_until_schedule(hour: u32, minute: u32) {
    let next_run = calculate_next_occurrence(hour, minute);
    let now = Utc::now();

    if next_run > now {
        let duration = (next_run - now)
            .to_std()
            .expect("Duration calculation failed");

        info!(
            next_run_utc = ?next_run,
            next_run_berlin = ?next_run.with_timezone(&chrono_tz::Europe::Berlin),
            wait_seconds = duration.as_secs(),
            "Sleeping until next scheduled time"
        );

        sleep(duration).await;
    }
}

/// Sleeps until a specific time today in Europe/Berlin timezone.
///
/// If the target time has already passed, returns immediately.
#[instrument]
async fn sleep_until_hms(hour: u32, minute: u32, second: u32, expected_latency: u32) {
    let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    let time = now
        .date_naive()
        .and_hms_opt(hour, minute, second)
        .expect("Invalid stats time")
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()
        .expect("Ambiguous time during DST transition");

    let wait_duration =
        (time.with_timezone(&Utc) - Utc::now() - TimeDelta::milliseconds(expected_latency as i64))
            .to_std()
            .unwrap_or(Duration::from_secs(0));

    if wait_duration > Duration::from_secs(0) {
        info!(
            wait_seconds = wait_duration.as_secs(),
            "Waiting until 13:38 to post stats"
        );
        sleep(wait_duration).await;
    }
}

/// Checks if a given user is a clanker
///
/// Returns true if the login name matches any bot in the ignore list.
fn is_clanker(login: &str) -> bool {
    [
        "supibot",
        "potatbotat",
        "streamelements",
        "koknuts",
        "thedagothur",
    ]
    .contains(&login)
}

/// Determines if a message should be counted as a valid 1337 message.
///
/// Filters out clanker messages and checks for keywords "1337" or "DANKIES".
fn is_valid_1337_message(message: &PrivmsgMessage) -> bool {
    if is_clanker(&message.sender.login) {
        return false;
    }
    if message.message_text.contains("DANKIES") || message.message_text.contains("1337") {
        return true;
    }
    false
}

/// Generates a stats message based on the number of users who said 1337.
///
/// Returns a contextual message with emotes based on participation level.
fn generate_stats_message(count: usize, user_list: &[String]) -> String {
    match count {
        0 => one_of(&["Erm", "fuh"]).to_string(),
        1 => one_of(&[
            format!(
                "@{} zumindest einer {}",
                user_list
                    .first()
                    .expect("Count should equal user list length"),
                one_of(&["fuh", "uhh"])
            ),
            format!(
                "War wohl zu viel verlangt {}",
                one_of(&["BRUHSIT", "UltraMad", "Madeg"])
            ),
        ])
        .to_string(),
        2..=3 if !user_list.contains(&"gargoyletec".to_string()) => {
            format!(
                "{count}{} gnocci {}",
                one_of(&[" und nichtmal", ", aber wo", " und ohne"]),
                one_of(&["Sadding", "Sadge", "Sadeg", "SadgeCry", "Saddies"])
            )
        }
        2..=3 => format!(
            "{count}, {}",
            one_of(&[
                "geht besser Okayge",
                "verbesserungswürdig Waiting",
                "unterdurchschnittlich Waiting",
                "ausbaufähig Waiting",
                "entspricht nicht ganz den Erwarungen Waiting",
                "bemüht",
                "anpassungsfähig YEP"
            ])
        ),
        4 => one_of(&[
            "3.6, nicht gut, nicht dramatisch".to_string(),
            format!("{count}, {}", one_of(&["Standard Performance", "solide"])),
        ])
        .to_string(),
        5..=7 => {
            format!(
                "{count}, gute Auslese {}",
                one_of(&["bieberbutzemanPepe", "peepoHappy"])
            )
        }
        _ => {
            format!(
                "{count}, {}",
                one_of(&["insane quota Pag", "rekordverdächtig WICKED"])
            )
        }
    }
}

/// Returns a random element from an array.
///
/// Used for adding variety to bot responses by randomly selecting from predefined options.
fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}

/// System prompt for the AI command, instructing the model how to behave.
const AI_SYSTEM_PROMPT: &str = r#"You are a helpful Twitch chat bot assistant. Keep responses brief (2-3 sentences max) since they'll appear in chat. Be friendly and casual. You have access to SimBrief flight planning tools which you can use when users ask about flight plans or aircraft.

When using tools:
- For flight plans, always ask the user to specify their SimBrief username if not provided
- Format flight information concisely (origin -> destination, aircraft, key stats)
- If a tool fails, explain the error briefly

Respond in the same language the user writes in (German or English)."#;

/// Maximum response length for Twitch chat (to stay within limits).
const MAX_RESPONSE_LENGTH: usize = 500;

/// Maximum number of tool call iterations to prevent infinite loops.
const MAX_TOOL_ITERATIONS: usize = 5;

/// Builds the SimBrief tool definitions for OpenRouter (OpenAI format).
fn build_simbrief_tools() -> Vec<Tool> {
    vec![
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "get_flight_plan".to_string(),
                description: "Fetch the latest flight plan for a SimBrief user. Returns flight details including origin, destination, route, fuel, and times.".to_string(),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "user": {
                            "type": "string",
                            "description": "The SimBrief username or numeric user ID"
                        }
                    },
                    "required": ["user"]
                })),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "get_aircraft_list".to_string(),
                description: "Get a list of available aircraft types in SimBrief. Returns aircraft codes and names.".to_string(),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            },
        },
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "create_dispatch_url".to_string(),
                description: "Generate a SimBrief dispatch URL for creating a new flight plan. Returns a URL the user can open.".to_string(),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "origin": {
                            "type": "string",
                            "description": "Origin airport ICAO code (e.g., EDDF)"
                        },
                        "destination": {
                            "type": "string",
                            "description": "Destination airport ICAO code (e.g., LIRF)"
                        },
                        "aircraft": {
                            "type": "string",
                            "description": "Aircraft type code (e.g., B738, A320)"
                        },
                        "route": {
                            "type": "string",
                            "description": "Optional route string"
                        },
                        "alternate": {
                            "type": "string",
                            "description": "Optional alternate airport ICAO code"
                        },
                        "airline": {
                            "type": "string",
                            "description": "Optional airline ICAO code"
                        },
                        "flight_number": {
                            "type": "string",
                            "description": "Optional flight number"
                        }
                    },
                    "required": ["origin", "destination", "aircraft"]
                })),
            },
        },
    ]
}

/// Executes a single tool call and returns the result as JSON.
async fn execute_tool(
    name: &str,
    args: &serde_json::Value,
    simbrief_client: &SimBriefClient,
) -> serde_json::Value {
    debug!(tool = %name, args = ?args, "Executing tool");

    match name {
        "get_flight_plan" => {
            let user = args
                .get("user")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if user.is_empty() {
                return serde_json::json!({
                    "error": "Missing required parameter: user"
                });
            }

            match simbrief_client.get_latest_flight_plan(user).await {
                Ok(plan) => {
                    serde_json::json!({
                        "origin": format!("{} ({})", plan.origin.icao_code, plan.origin.name),
                        "destination": format!("{} ({})", plan.destination.icao_code, plan.destination.name),
                        "route": plan.general.route,
                        "flight_number": plan.general.flight_number,
                        "airline": plan.general.icao_airline,
                        "distance_nm": plan.general.air_distance,
                        "aircraft": plan.aircraft.map(|a| format!("{} ({})", a.icaocode, a.name)),
                        "fuel_ramp_lbs": plan.fuel.plan_ramp,
                        "fuel_enroute_lbs": plan.fuel.enroute_burn,
                        "time_enroute_seconds": plan.times.est_time_enroute,
                        "passengers": plan.weights.pax_count,
                    })
                }
                Err(e) => {
                    serde_json::json!({
                        "error": format!("Failed to fetch flight plan: {}", e)
                    })
                }
            }
        }
        "get_aircraft_list" => {
            match simbrief_client.get_available_aircraft().await {
                Ok(list) => {
                    // Return a summary (top 20 aircraft by name for brevity)
                    let aircraft: Vec<serde_json::Value> = list
                        .aircraft
                        .iter()
                        .take(20)
                        .map(|(code, info)| {
                            serde_json::json!({
                                "code": code,
                                "name": info.name
                            })
                        })
                        .collect();

                    serde_json::json!({
                        "aircraft_count": list.aircraft.len(),
                        "sample_aircraft": aircraft,
                        "note": "Showing first 20 of available aircraft. Use specific codes like B738, A320, B77W, etc."
                    })
                }
                Err(e) => {
                    serde_json::json!({
                        "error": format!("Failed to fetch aircraft list: {}", e)
                    })
                }
            }
        }
        "create_dispatch_url" => {
            let origin = args
                .get("origin")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let destination = args
                .get("destination")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let aircraft = args
                .get("aircraft")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if origin.is_empty() || destination.is_empty() || aircraft.is_empty() {
                return serde_json::json!({
                    "error": "Missing required parameters: origin, destination, and aircraft are required"
                });
            }

            let params = DispatchParams {
                origin: origin.to_string(),
                destination: destination.to_string(),
                aircraft: aircraft.to_string(),
                route: args.get("route").and_then(|v| v.as_str()).map(String::from),
                alternate: args.get("alternate").and_then(|v| v.as_str()).map(String::from),
                airline: args.get("airline").and_then(|v| v.as_str()).map(String::from),
                flight_number: args.get("flight_number").and_then(|v| v.as_str()).map(String::from),
            };

            let url = simbrief_client.create_dispatch_url(params);

            serde_json::json!({
                "url": url,
                "message": "Open this URL to create your flight plan in SimBrief"
            })
        }
        _ => {
            serde_json::json!({
                "error": format!("Unknown tool: {}", name)
            })
        }
    }
}

/// Executes the AI command with tool support.
///
/// Orchestrates the conversation with OpenRouter, handling tool calls in a loop.
async fn execute_ai_with_tools(
    instruction: &str,
    openrouter_client: &OpenRouterClient,
    simbrief_client: &SimBriefClient,
) -> Result<String> {
    let tools = build_simbrief_tools();

    // Build initial messages with system prompt and user instruction
    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: Some(AI_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: "user".to_string(),
            content: Some(instruction.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    // Tool calling loop
    for iteration in 0..MAX_TOOL_ITERATIONS {
        debug!(iteration, "AI tool calling iteration");

        let request = ChatCompletionRequest {
            model: openrouter_client.model().to_string(),
            messages: messages.clone(),
            tools: Some(tools.clone()),
        };

        let response = openrouter_client.chat_completion(request).await?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("No choices in OpenRouter response"))?;

        // Check if we got tool calls or final text
        let has_tool_calls = choice.message.tool_calls.is_some()
            && !choice.message.tool_calls.as_ref().unwrap().is_empty();

        if has_tool_calls {
            let tool_calls = choice.message.tool_calls.as_ref().unwrap();

            // Add assistant's message with tool calls to conversation
            messages.push(choice.message.clone());

            // Execute each tool and add results
            for tool_call in tool_calls {
                debug!(
                    name = %tool_call.function.name,
                    args = %tool_call.function.arguments,
                    "OpenRouter requested tool call"
                );

                // Parse arguments from JSON string
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

                // Execute the tool
                let result = execute_tool(&tool_call.function.name, &args, simbrief_client).await;

                // Add tool result message
                messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&result).unwrap_or_default()),
                    tool_calls: None,
                    tool_call_id: Some(tool_call.id.clone()),
                });
            }
        } else {
            // No tool calls - return final text response
            if let Some(text) = choice.message.content {
                return Ok(text);
            }
            return Err(eyre::eyre!("No text response from OpenRouter"));
        }
    }

    Err(eyre::eyre!("Max tool iterations exceeded"))
}

/// Truncates a string to the maximum length at a word boundary.
fn truncate_response(text: &str, max_len: usize) -> String {
    // Collapse whitespace and newlines
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.len() <= max_len {
        return collapsed;
    }

    // Find last space before max_len
    let truncated = &collapsed[..max_len];
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..last_space])
    } else {
        format!("{}...", truncated)
    }
}

/// Monitors broadcast messages and tracks users who say 1337 during the target minute.
///
/// Runs in a loop until the broadcast channel closes or an error occurs.
/// Only tracks messages sent during the configured TARGET_HOUR:TARGET_MINUTE.
#[instrument(skip(broadcast_rx, total_users))]
async fn monitor_1337_messages(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    total_users: Arc<Mutex<HashSet<String>>>,
) {
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // Check time and message content
                let local = privmsg
                    .server_timestamp
                    .with_timezone(&chrono_tz::Europe::Berlin);
                if (local.hour(), local.minute()) != (TARGET_HOUR, TARGET_MINUTE) {
                    continue;
                }

                if is_valid_1337_message(&privmsg) {
                    let username = &privmsg.sender.login;
                    debug!(user = %username, "User said 1337 at 13:37");

                    let mut users = total_users.lock().await;
                    // Double-check minute to prevent race condition
                    let current_minute = privmsg
                        .server_timestamp
                        .with_timezone(&chrono_tz::Europe::Berlin)
                        .minute();
                    if current_minute == TARGET_MINUTE {
                        if users.len() < MAX_USERS {
                            users.insert(privmsg.sender.login);
                        } else {
                            error!(max = MAX_USERS, "User limit reached");
                        }
                    } else {
                        debug!("Skipping insert, minute changed to {current_minute}");
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "1337 handler lagged, skipped messages");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, 1337 monitor exiting");
                break;
            }
        }
    }
}


/// Token storage implementation that persists tokens to disk.
///
/// Falls back to initial refresh token from config on first load if no token file exists.
#[derive(Debug)]
struct FileBasedTokenStorage {
    path: PathBuf,
    initial_refresh_token: SecretString,
}

impl FileBasedTokenStorage {
    fn new(initial_refresh_token: SecretString) -> Self {
        Self {
            path: PathBuf::from("./token.ron"),
            initial_refresh_token,
        }
    }
}

#[async_trait]
impl TokenStorage for FileBasedTokenStorage {
    type LoadError = eyre::Report;
    type UpdateError = eyre::Report;

    #[instrument(skip(self))]
    async fn load_token(&mut self) -> Result<UserAccessToken, Self::LoadError> {
        // Try to load from file first
        match fs::read_to_string(&self.path).await {
            Ok(contents) => {
                debug!(
                    path = %self.path.display(),
                    "Loading user access token from file"
                );
                Ok(ron::from_str(&contents)?)
            }
            Err(_) => {
                // File doesn't exist, use initial refresh token from configuration
                warn!("Token file not found, using refresh token from configuration");
                let token = UserAccessToken {
                    access_token: String::new(),
                    refresh_token: self.initial_refresh_token.expose_secret().to_string(),
                    created_at: chrono::Utc::now(),
                    expires_at: None,
                };

                // Save the token for future use
                self.update_token(&token).await?;

                Ok(token)
            }
        }
    }

    #[instrument(skip(self, token))]
    async fn update_token(&mut self, token: &UserAccessToken) -> Result<(), Self::UpdateError> {
        debug!(path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        File::create(&self.path).await?.write_all(&buffer).await?;
        Ok(())
    }
}

fn install_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};

    let fmt_layer = fmt::layer().with_target(false);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
}

const CONFIG_PATH: &str = "./config.toml";

async fn load_configuration() -> Result<Configuration> {
    let data = tokio::fs::read_to_string(CONFIG_PATH)
        .await
        .wrap_err_with(|| format!(
            "Failed to read config file: {}\nPlease create config.toml from config.toml.example",
            CONFIG_PATH
        ))?;

    info!("Loading configuration from {}", CONFIG_PATH);

    let config: Configuration = toml::from_str(&data)
        .wrap_err("Failed to parse config.toml - check for syntax errors")?;

    config.validate()?;

    Ok(config)
}

/// Parse a datetime string in ISO 8601 format (YYYY-MM-DDTHH:MM:SS).
fn parse_datetime(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .wrap_err_with(|| format!("Invalid datetime format '{}' (expected YYYY-MM-DDTHH:MM:SS)", s))
}

/// Parse a time string in HH:MM format.
fn parse_time(s: &str) -> Result<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .wrap_err_with(|| format!("Invalid time format '{}' (expected HH:MM)", s))
}

/// Convert a ScheduleConfig from config.toml into a database::Schedule.
fn schedule_config_to_schedule(config: &ScheduleConfig) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&config.interval)?;

    let start_date = config.start_date.as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let end_date = config.end_date.as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let active_time_start = config.active_time_start.as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let active_time_end = config.active_time_end.as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let schedule = database::Schedule {
        name: config.name.clone(),
        start_date,
        end_date,
        active_time_start,
        active_time_end,
        interval,
        message: config.message.clone(),
    };

    schedule.validate()?;

    Ok(schedule)
}

/// Load schedules from the Configuration struct.
/// Filters out disabled schedules and validates all enabled ones.
fn load_schedules_from_config(config: &Configuration) -> Vec<database::Schedule> {
    let mut schedules = Vec::new();

    for schedule_config in &config.schedules {
        if !schedule_config.enabled {
            debug!(schedule = %schedule_config.name, "Skipping disabled schedule");
            continue;
        }

        match schedule_config_to_schedule(schedule_config) {
            Ok(schedule) => schedules.push(schedule),
            Err(e) => {
                error!(
                    schedule = %schedule_config.name,
                    error = ?e,
                    "Failed to parse schedule config, skipping"
                );
            }
        }
    }

    schedules
}

/// Reload configuration from config.toml and extract schedules.
/// Returns None if config cannot be loaded or parsed.
fn reload_schedules_from_config() -> Option<Vec<database::Schedule>> {
    let data = match std::fs::read_to_string(CONFIG_PATH) {
        Ok(data) => data,
        Err(e) => {
            error!(error = ?e, "Failed to read config.toml for reload");
            return None;
        }
    };

    let config: Configuration = match toml::from_str(&data) {
        Ok(config) => config,
        Err(e) => {
            error!(error = ?e, "Failed to parse config.toml for reload");
            return None;
        }
    };

    if let Err(e) = config.validate() {
        error!(error = ?e, "Config validation failed during reload");
        return None;
    }

    Some(load_schedules_from_config(&config))
}

/// Config file watcher service that monitors config.toml for changes.
/// Uses notify-debouncer-mini with 2 second debounce to avoid rapid reloads.
#[instrument(skip(cache))]
async fn run_config_watcher_service(
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
) {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
    use std::time::Duration as StdDuration;

    info!("Config watcher service started");

    // Create channel for receiving file change events
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);

    // Get absolute path to config file for watching
    let config_path = match std::fs::canonicalize(CONFIG_PATH) {
        Ok(p) => p,
        Err(e) => {
            error!(error = ?e, "Failed to get absolute path for config.toml");
            return;
        }
    };

    // Spawn blocking task for the file watcher (notify is sync)
    let watcher_config_path = config_path.clone();
    let mut watcher_handle = tokio::task::spawn_blocking(move || {
        let tx = tx;
        let config_path = watcher_config_path;

        // Create debouncer with 2 second timeout
        let mut debouncer = match new_debouncer(
            StdDuration::from_secs(2),
            move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify_debouncer_mini::notify::Error>| {
                match res {
                    Ok(events) => {
                        for event in events {
                            debug!(path = ?event.path, "File change event received");
                            // Use blocking_send since we're in a sync context
                            if tx.blocking_send(()).is_err() {
                                // Channel closed, watcher should stop
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, "File watcher error");
                    }
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                error!(error = ?e, "Failed to create file watcher");
                return;
            }
        };

        // Watch the config file's parent directory
        let watch_path = config_path.parent().unwrap_or(Path::new("."));
        if let Err(e) = debouncer.watcher().watch(watch_path, RecursiveMode::NonRecursive) {
            error!(error = ?e, path = ?watch_path, "Failed to watch config directory");
            return;
        }

        info!(path = ?watch_path, "Watching for config changes");

        // Keep the watcher alive by parking the thread
        // The watcher will be dropped when the main task exits
        loop {
            std::thread::park();
        }
    });

    // Main loop: handle file change events
    loop {
        tokio::select! {
            Some(()) = rx.recv() => {
                info!("Config file changed, reloading schedules");

                if let Some(schedules) = reload_schedules_from_config() {
                    let mut cache_guard = cache.write().await;
                    let old_count = cache_guard.schedules.len();
                    cache_guard.update(schedules);

                    info!(
                        old_count,
                        new_count = cache_guard.schedules.len(),
                        version = cache_guard.version,
                        "Schedules reloaded from config"
                    );
                } else {
                    warn!("Failed to reload config, keeping existing schedules");
                }
            }
            _ = &mut watcher_handle => {
                error!("File watcher task exited unexpectedly");
                break;
            }
        }
    }
}

/// Main entry point for the twitch-1337 bot.
///
/// Establishes a persistent Twitch IRC connection and runs multiple handlers in parallel:
/// - Daily 1337 tracker: monitors 13:37 messages, posts stats at 13:38
/// - Generic commands: handles !toggle-ping and other bot commands
///
/// # Errors
///
/// Returns an error if required environment variables are missing or connection fails.
#[tokio::main]
#[instrument]
pub async fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    // Initialize tracing subscriber
    install_tracing();

    let config = load_configuration().await?;

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    let schedules_enabled = !config.schedules.is_empty();

    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedules_enabled,
        schedule_count = config.schedules.len(),
        "Starting twitch-1337 bot"
    );

    ensure_data_dir().await?;

    // Setup, connect, join channel, and verify authentication (all in one step)
    let (incoming_messages, client) = setup_and_verify_twitch_client(&config.twitch).await?;

    // Wrap client in Arc for sharing across handlers
    let client = Arc::new(client);

    // Create broadcast channel for message distribution (capacity: 100 messages)
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    // Spawn message router task
    let router_handle = tokio::spawn(run_message_router(incoming_messages, broadcast_tx.clone()));

    // Optionally spawn config watcher service and scheduled message handler
    let (watcher_service, handler_scheduled_messages) = if schedules_enabled {
        info!(
            count = config.schedules.len(),
            "Schedules configured, starting scheduled message system"
        );

        // Load initial schedules from config
        let initial_schedules = load_schedules_from_config(&config);
        info!(
            loaded = initial_schedules.len(),
            "Loaded initial schedules from config"
        );

        // Create schedule cache for dynamic scheduled messages
        let mut cache = database::ScheduleCache::new();
        cache.update(initial_schedules);
        let schedule_cache = Arc::new(tokio::sync::RwLock::new(cache));

        // Spawn config watcher service
        let watcher = tokio::spawn({
            let cache = schedule_cache.clone();
            async move {
                run_config_watcher_service(cache).await;
            }
        });

        // Spawn scheduled message handler
        let handler = tokio::spawn({
            let client = client.clone();
            let cache = schedule_cache.clone();
            let channel = config.twitch.channel.clone();
            async move { run_scheduled_message_handler(client, cache, channel).await }
        });

        (Some(watcher), Some(handler))
    } else {
        info!("No schedules configured, scheduled messages disabled");
        (None, None)
    };

    // Spawn 1337 handler task
    let handler_1337 = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let expected_latency = config.twitch.expected_latency;
        async move {
            run_1337_handler(broadcast_tx, client, channel, expected_latency).await;
        }
    });

    let handler_generic_commands = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let se_config = config.streamelements.clone();
        let openrouter_config = config.openrouter.clone();
        async move { run_generic_command_handler(broadcast_tx, client, se_config, openrouter_config).await }
    });

    if schedules_enabled {
        info!(
            "Bot running with continuous connection. Handlers: Config watcher, 1337 tracker, Generic commands, Scheduled messages"
        );
        info!("Scheduled messages: Loaded from config.toml, reloads on file change");
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Generic commands"
        );
    }
    info!(
        "1337 tracker scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR,
        TARGET_MINUTE - 1
    );

    // Keep the program running until shutdown signal or any task exits
    info!("Bot is running. Press Ctrl+C to stop.");

    // Handle optional scheduled message handlers
    match (watcher_service, handler_scheduled_messages) {
        (Some(watcher), Some(handler)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received, exiting gracefully");
                }
                result = router_handle => {
                    error!("Message router exited unexpectedly: {result:?}");
                }
                result = watcher => {
                    error!("Config watcher service exited unexpectedly: {result:?}");
                }
                result = handler_1337 => {
                    error!("1337 handler exited unexpectedly: {result:?}");
                }
                result = handler_generic_commands => {
                    error!("Generic Command Handler exited unexpectedly: {result:?}");
                }
                result = handler => {
                    error!("Scheduled message handler exited unexpectedly: {result:?}");
                }
            }
        }
        _ => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received, exiting gracefully");
                }
                result = router_handle => {
                    error!("Message router exited unexpectedly: {result:?}");
                }
                result = handler_1337 => {
                    error!("1337 handler exited unexpectedly: {result:?}");
                }
                result = handler_generic_commands => {
                    error!("Generic Command Handler exited unexpectedly: {result:?}");
                }
            }
        }
    }

    info!("Bot shutdown complete");
    Ok(())
}

fn get_data_dir() -> PathBuf {
    std::env::var("DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/twitch-1337".to_string())
        .into()
}

#[instrument]
async fn ensure_data_dir() -> Result<()> {
    let data_dir = get_data_dir();
    if !data_dir.exists() {
        tokio::fs::create_dir_all(data_dir).await?;
    }
    Ok(())
}

#[instrument(skip(config))]
fn setup_twitch_client(config: &TwitchConfiguration) -> (UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient) {
    // Create authenticated IRC client with refreshing tokens
    let credentials = RefreshingLoginCredentials::init_with_username(
        Some(config.username.clone()),
        config.client_id.expose_secret().to_string(),
        config.client_secret.expose_secret().to_string(),
        FileBasedTokenStorage::new(config.refresh_token.clone()),
    );
    let twitch_config = ClientConfig::new_simple(credentials);
    TwitchIRCClient::<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>::new(
        twitch_config,
    )
}

/// Sets up and verifies the Twitch IRC connection.
///
/// Creates the client, connects, joins the configured channel, and verifies authentication
/// by waiting for a GlobalUserState message. Returns the verified client and message receiver.
///
/// # Errors
///
/// Returns an error if connection times out (30s) or authentication fails.
#[instrument(skip(config))]
async fn setup_and_verify_twitch_client(
    config: &TwitchConfiguration,
) -> Result<(UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient)> {
    info!("Setting up and verifying Twitch connection");

    let (mut incoming_messages, client) = setup_twitch_client(config);

    // Connect to Twitch IRC
    info!("Connecting to Twitch IRC");
    client.connect().await;

    // Join the configured channel
    info!(channel = %config.channel, "Joining channel");
    let channels = [config.channel.clone()].into();
    client.set_wanted_channels(channels)?;

    // Verify authentication by waiting for GlobalUserState message
    let verification = async {
        while let Some(message) = incoming_messages.recv().await {
            trace!(message = ?message, "Received IRC message during verification");

            match message {
                ServerMessage::Notice(NoticeMessage { message_text, .. })
                    if message_text == "Login authentication failed" =>
                {
                    error!(
                        "Authentication with Twitch IRC Servers failed: {}",
                        message_text
                    );
                    bail!(
                        "Failed to authenticate with Twitch. This is likely due to missing token scopes. \
                        Ensure your token has 'chat:read' and 'chat:edit' scopes."
                    );
                }
                ServerMessage::Notice(NoticeMessage { message_text, .. })
                    if message_text == "Login unsuccessful" =>
                {
                    error!(
                        "Authentication with Twitch IRC Servers failed: {}",
                        message_text
                    );
                    bail!(
                        "Failed to authenticate with Twitch. This is likely due to an invalid or expired token. \
                        Check your TWITCH_ACCESS_TOKEN and TWITCH_REFRESH_TOKEN."
                    );
                }
                ServerMessage::GlobalUserState(_) => {
                    info!("Connection verified and authenticated");
                    return Ok(());
                }
                _ => continue,
            }
        }
        bail!("Connection closed during verification")
    };

    match tokio::time::timeout(Duration::from_secs(30), verification).await {
        Err(_) => {
            error!("Connection to Twitch IRC Server timed out");
            bail!("Connection to Twitch timed out")
        }
        Ok(result) => result?,
    };

    Ok((incoming_messages, client))
}

/// Message router task that broadcasts incoming IRC messages to all handlers.
///
/// Reads from the twitch-irc receiver and broadcasts to all subscribed handlers.
/// Exits when the incoming_messages channel is closed.
#[instrument(skip(incoming_messages, broadcast_tx))]
async fn run_message_router(
    mut incoming_messages: UnboundedReceiver<ServerMessage>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
) {
    info!("Message router started");

    while let Some(message) = incoming_messages.recv().await {
        trace!(message = ?message, "Routing IRC message");

        // Broadcast to all listeners (ignore errors if no receivers)
        let _ = broadcast_tx.send(message);
    }

    debug!("Message router exited (connection closed)");
}

/// Handler for the daily 1337 tracking feature.
///
/// Monitors messages during the 13:37 window, tracks unique users, and posts stats at 13:38.
/// Runs continuously, resetting state daily.
#[instrument(skip(broadcast_tx, client, channel))]
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    expected_latency: u32,
) {
    info!("1337 handler started");

    loop {
        // Wait until 13:36 to start monitoring
        wait_until_schedule(TARGET_HOUR, TARGET_MINUTE - 1).await;

        info!("Starting daily 1337 monitoring session");

        // Fresh HashSet for today's users
        let total_users = Arc::new(Mutex::new(HashSet::with_capacity(MAX_USERS)));

        // Spawn message monitoring subtask
        let monitor_handle = tokio::spawn({
            let total_users = total_users.clone();
            // Subscribe fresh when we wake up - only see messages from now on
            let broadcast_rx = broadcast_tx.subscribe();

            async move {
                monitor_1337_messages(broadcast_rx, total_users).await;
            }
        });

        // Wait until 13:36:30 to send reminder
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30, expected_latency).await;

        info!("Posting reminder to channel");
        if let Err(e) = client
            .say(channel.clone(), "PausersHype".to_string())
            .await
        {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0, expected_latency).await;

        // Get user list and count
        let (count, user_list) = {
            let users = total_users.lock().await;
            let count = users.len();
            let mut user_vec: Vec<String> = users.iter().cloned().collect();
            user_vec.sort(); // Sort alphabetically for consistency
            (count, user_vec)
        };

        let message = generate_stats_message(count, &user_list);

        // Post stats message
        info!(count = count, "Posting stats to channel");
        if let Err(e) = client.say(channel.clone(), message).await {
            error!(error = ?e, count = count, "Failed to send stats message");
        } else {
            info!("Stats posted successfully");
        }

        // Abort the monitor task
        monitor_handle.abort();

        info!("Daily 1337 session completed, waiting for next day");
    }
}


/// Handler for generic text commands that start with `!`.
///
/// Monitors chat for commands and dispatches them to appropriate handlers.
/// Currently supports:
/// - `!toggle-ping <command>` - Adds/removes user from StreamElements ping command
/// - `!ai <instruction>` - AI-powered responses with SimBrief tools (if OpenRouter configured)
///
/// Runs continuously in a loop, processing all incoming messages.
#[instrument(skip(broadcast_tx, client, se_config, openrouter_config))]
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    se_config: StreamelementsConfig,
    openrouter_config: Option<OpenRouterConfig>,
) {
    info!("Generic Command Handler started");

    // Subscribe to the broadcast channel
    let broadcast_rx = broadcast_tx.subscribe();

    // Initialize StreamElements client
    let se_client = match SEClient::new(se_config.api_token.expose_secret()) {
        Ok(client) => client,
        Err(e) => {
            error!(error = ?e, "Failed to initialize StreamElements client");
            error!("Generic Command Handler cannot start without valid StreamElements API token");
            return;
        }
    };

    // Initialize OpenRouter and SimBrief clients (optional)
    let (openrouter_client, simbrief_client) = if let Some(ref openrouter_cfg) = openrouter_config {
        let openrouter = match OpenRouterClient::new(
            openrouter_cfg.api_key.expose_secret(),
            &openrouter_cfg.model,
        ) {
            Ok(client) => client,
            Err(e) => {
                error!(error = ?e, "Failed to initialize OpenRouter client");
                error!("AI command will be disabled");
                return run_generic_command_handler_inner(
                    broadcast_rx, client, se_client, se_config.channel_id, None, None,
                ).await;
            }
        };

        let simbrief = match SimBriefClient::new() {
            Ok(client) => client,
            Err(e) => {
                error!(error = ?e, "Failed to initialize SimBrief client");
                error!("AI command will be disabled");
                return run_generic_command_handler_inner(
                    broadcast_rx, client, se_client, se_config.channel_id, None, None,
                ).await;
            }
        };

        info!(model = %openrouter_cfg.model, "OpenRouter AI command enabled");
        (Some(openrouter), Some(simbrief))
    } else {
        debug!("OpenRouter not configured, AI command disabled");
        (None, None)
    };

    run_generic_command_handler_inner(
        broadcast_rx,
        client,
        se_client,
        se_config.channel_id,
        openrouter_client,
        simbrief_client,
    )
    .await;
}

/// Inner loop for the generic command handler.
#[instrument(skip(broadcast_rx, client, se_client, openrouter_client, simbrief_client))]
async fn run_generic_command_handler_inner(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    se_client: SEClient,
    channel_id: String,
    openrouter_client: Option<OpenRouterClient>,
    simbrief_client: Option<SimBriefClient>,
) {
    // Cooldown tracking for AI command
    let ai_cooldowns: Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // Catch any errors from command handling to prevent task crash
                if let Err(e) = handle_generic_commands(
                    &privmsg,
                    &client,
                    &se_client,
                    &channel_id,
                    openrouter_client.as_ref(),
                    simbrief_client.as_ref(),
                    &ai_cooldowns,
                )
                .await
                {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        message = %privmsg.message_text,
                        "Error handling generic command"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Generic Command Handler lagged, skipped messages");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, Generic Command Handler exiting");
                break;
            }
        }
    }
}

/// Dispatches chat messages to the appropriate command handler.
///
/// Parses the first word of the message and routes to specialized handlers.
/// This acts as a simple command router for all `!` commands.
///
/// # Errors
///
/// Returns an error if command execution fails, but does not crash the handler.
#[instrument(skip(privmsg, client, se_client, channel_id, openrouter_client, simbrief_client, ai_cooldowns))]
async fn handle_generic_commands(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    openrouter_client: Option<&OpenRouterClient>,
    simbrief_client: Option<&SimBriefClient>,
    ai_cooldowns: &Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
) -> Result<()> {
    let mut words = privmsg.message_text.split_whitespace();
    let Some(first_word) = words.next() else {
        return Ok(());
    };

    if first_word == "!toggle-ping" {
        toggle_ping_command(privmsg, client, se_client, channel_id, words.next()).await?;
    } else if first_word == "!list-pings" {
        list_pings_command(privmsg, client, se_client, channel_id, words.next()).await?;
    } else if first_word == "!ai" {
        // Check if AI is enabled
        if let (Some(openrouter), Some(simbrief)) = (openrouter_client, simbrief_client) {
            // Collect remaining words as the instruction
            let instruction: String = words.collect::<Vec<_>>().join(" ");
            ai_command(privmsg, client, openrouter, simbrief, ai_cooldowns, &instruction).await?;
        } else {
            // AI not configured - silently ignore or could add a message
            debug!("AI command received but OpenRouter not configured");
        }
    }

    Ok(())
}

const PING_COMMANDS: &[&str] = &[
    "ackern",
    "amra",
    "arbeitszeitbetrug",
    "dayz",
    "dbd",
    "deadlock",
    "eft",
    "euv",
    "fetentiere",
    "front",
    "hoi",
    "kluft",
    "kreuzzug",
    "ron",
    "ttt",
    "vicky",
];

/// Toggles a user's mention in a StreamElements ping command.
///
/// Ping commands are used to notify community members about game sessions.
/// This function adds the requesting user's @mention to the command reply if not present,
/// or removes it if already present.
///
/// # Command Format
///
/// `!toggle-ping <command_name>`
///
/// # Behavior
///
/// 1. Searches for a StreamElements command matching `<command_name>` with the "pinger" keyword
/// 2. If user's @mention exists in the reply, removes it (case-insensitive)
/// 3. If not present, adds @mention after the first existing @ symbol (or at the start)
/// 4. Updates the command via StreamElements API
/// 5. Confirms success to the user
///
/// # Error Responses
///
/// - "Das kann ich nicht FDM" - No command name provided
/// - "Das finde ich nicht FDM" - Command not found
///
/// # Errors
///
/// Returns an error if IRC communication or StreamElements API calls fail.
/// User-facing errors are sent as chat messages before returning the error.
#[instrument(skip(privmsg, client, se_client, channel_id))]
async fn toggle_ping_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    command_name: Option<&str>,
) -> Result<()> {
    let Some(command_name) = command_name else {
        // Best-effort reply, log but don't fail if this specific reply fails
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das kann ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'no command name' error message");
        }
        return Ok(());
    };

    if !PING_COMMANDS.contains(&command_name) {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das finde ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    }

    // Fetch all commands from StreamElements
    let commands = se_client
        .get_all_commands(channel_id)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    // Find the matching command with "pinger" keyword
    let Some(mut command) = commands
        .into_iter()
        .find(|command| command.command == command_name)
    else {
        // Best-effort reply
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das gibt es nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    };

    // Create case-insensitive regex to find user's mention
    // Use regex::escape to prevent username from being interpreted as regex
    let escaped_username = regex::escape(&privmsg.sender.login);
    let re = Regex::new(&format!("(?i)@?\\s*{}", escaped_username))
        .wrap_err("Failed to create username regex")?;

    // Toggle user's mention in the command reply
    let mut has_added_ping = false;
    let new_reply = if re.is_match(&command.reply) {
        // Remove user's mention
        re.replace_all(&command.reply, "").to_string()
    } else {
        has_added_ping = true;
        // Add user's mention
        if let Some(insert_location) = command.reply.find('@') {
            // Insert after first @ symbol
            let (head, tail) = command.reply.split_at(insert_location);
            format!("{head} @{} {tail}", privmsg.sender.name)
        } else {
            // No @ found, add at the beginning
            format!("@{} {}", privmsg.sender.name, command.reply)
        }
    };

    // Clean up whitespaces
    command.reply = new_reply.split_whitespace().collect::<Vec<_>>().join(" ");

    debug!(
        command_name = %command_name,
        user = %privmsg.sender.login,
        new_reply = %command.reply,
        "Updating ping command"
    );

    // Update the command via StreamElements API
    se_client
        .update_command(channel_id, command)
        .await
        .wrap_err("Failed to update command via StreamElements API")?;

    // Confirm success to the user
    client
        .say_in_reply_to(
            privmsg,
            format!(
                "Hab ich {} gemacht Okayge",
                match has_added_ping {
                    true => "an",
                    false => "aus",
                }
            ),
        )
        .await
        .wrap_err("Failed to send success confirmation message")?;

    Ok(())
}

#[instrument(skip(privmsg, client, se_client, channel_id))]
async fn list_pings_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    enabled_option: Option<&str>,
) -> Result<()> {
    let filter = enabled_option.unwrap_or("enabled");

    let commands = se_client
        .get_all_commands(channel_id)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    let response = match filter {
        "enabled" => &commands
            .iter()
            .filter(|command| PING_COMMANDS.contains(&command.command.as_str()))
            .filter(|command| {
                command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "disabled" => &commands
            .iter()
            .filter(|command| PING_COMMANDS.contains(&command.command.as_str()))
            .filter(|command| {
                !command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "all" => &PING_COMMANDS.join(" "),
        _ => "Das weiß ich nicht Sadding",
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response.to_string()).await {
        error!(error = ?e, "Failed to send response message");
    }

    Ok(())
}

/// Cooldown duration for the AI command (30 seconds).
const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

/// Handles the `!ai` command for AI-powered responses.
///
/// Takes user instructions, processes them with OpenRouter, and optionally
/// uses SimBrief tools to fulfill flight planning requests.
///
/// # Command Format
///
/// `!ai <instruction>`
///
/// # Rate Limiting
///
/// Per-user cooldown of 30 seconds to prevent spam.
///
/// # Errors
///
/// Returns an error if the OpenRouter API call fails.
#[instrument(skip(privmsg, client, openrouter_client, simbrief_client, cooldowns))]
async fn ai_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    openrouter_client: &OpenRouterClient,
    simbrief_client: &SimBriefClient,
    cooldowns: &Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    instruction: &str,
) -> Result<()> {
    let user = &privmsg.sender.login;

    // Check cooldown
    {
        let cooldowns_guard = cooldowns.lock().await;
        if let Some(last_use) = cooldowns_guard.get(user) {
            let elapsed = last_use.elapsed();
            if elapsed < AI_COMMAND_COOLDOWN {
                let remaining = AI_COMMAND_COOLDOWN - elapsed;
                debug!(
                    user = %user,
                    remaining_secs = remaining.as_secs(),
                    "AI command on cooldown"
                );
                if let Err(e) = client
                    .say_in_reply_to(privmsg, "Bitte warte noch ein bisschen Waiting".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send cooldown message");
                }
                return Ok(());
            }
        }
    }

    // Check for empty instruction
    if instruction.trim().is_empty() {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Benutzung: !ai <anweisung>".to_string())
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    }

    debug!(user = %user, instruction = %instruction, "Processing AI command");

    // Update cooldown before making the API call
    {
        let mut cooldowns_guard = cooldowns.lock().await;
        cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
    }

    // Execute AI with timeout
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        execute_ai_with_tools(instruction, openrouter_client, simbrief_client),
    )
    .await;

    let response = match result {
        Ok(Ok(text)) => {
            // Truncate response for Twitch chat
            truncate_response(&text, MAX_RESPONSE_LENGTH)
        }
        Ok(Err(e)) => {
            error!(error = ?e, "AI execution failed");
            "Da ist was schiefgelaufen FDM".to_string()
        }
        Err(_) => {
            error!("AI execution timed out");
            "Das hat zu lange gedauert Waiting".to_string()
        }
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send AI response");
    }

    Ok(())
}

/// Run a single schedule task.
/// This task will run the schedule at its configured interval,
/// checking if it's still active before each post.
#[instrument(skip(client, cache, channel), fields(schedule = %schedule.name))]
async fn run_schedule_task(
    schedule: database::Schedule,
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
) {
    use chrono::Utc;
    use tokio::time::{Duration, sleep};

    info!(
        schedule = %schedule.name,
        interval_seconds = schedule.interval.num_seconds(),
        "Schedule task started"
    );

    loop {
        // Wait for the configured interval
        let interval_duration = Duration::from_secs(schedule.interval.num_seconds() as u64);
        sleep(interval_duration).await;

        // Check if schedule still exists in cache
        let still_exists = {
            let cache_guard = cache.read().await;
            cache_guard
                .schedules
                .iter()
                .any(|s| s.name == schedule.name)
        };

        if !still_exists {
            info!(
                schedule = %schedule.name,
                "Schedule no longer in cache, stopping task"
            );
            break;
        }

        // Check if schedule is currently active (respects date range and time window)
        let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

        if !schedule.is_active(now) {
            debug!(
                schedule = %schedule.name,
                "Schedule not active at current time, skipping post"
            );
            continue;
        }

        // Post the message
        info!(
            schedule = %schedule.name,
            message = %schedule.message,
            "Posting scheduled message"
        );

        if let Err(e) = client
            .say(channel.clone(), schedule.message.clone())
            .await
        {
            error!(
                error = ?e,
                schedule = %schedule.name,
                "Failed to send scheduled message"
            );
        } else {
            debug!(schedule = %schedule.name, "Scheduled message posted successfully");
        }
    }

    info!(schedule = %schedule.name, "Schedule task exiting");
}

/// Dynamic scheduled message handler that monitors cache for changes.
/// Spawns and stops tasks dynamically based on cache updates.
#[instrument(skip(client, cache, channel))]
async fn run_scheduled_message_handler(
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
) {
    use std::collections::HashMap;
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, interval};

    info!("Dynamic scheduled message handler started");

    // Track running tasks by schedule name
    let mut running_tasks: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut current_version = 0u64;

    // Monitor cache for changes every 30 seconds
    let mut check_interval = interval(Duration::from_secs(30));

    loop {
        check_interval.tick().await;

        let (schedules, version) = {
            let cache_guard = cache.read().await;
            (cache_guard.schedules.clone(), cache_guard.version)
        };

        // Check if cache version has changed
        if version != current_version {
            info!(
                old_version = current_version,
                new_version = version,
                schedule_count = schedules.len(),
                "Cache version changed, updating tasks"
            );

            current_version = version;

            // Build set of schedule names that should be running
            let desired_schedules: HashMap<String, database::Schedule> =
                schedules.into_iter().map(|s| (s.name.clone(), s)).collect();

            // Stop tasks for schedules that no longer exist or have changed
            running_tasks.retain(|name, handle| {
                if !desired_schedules.contains_key(name) {
                    info!(schedule = %name, "Stopping task for removed/changed schedule");
                    handle.abort();
                    false
                } else {
                    true
                }
            });

            // Start tasks for new schedules
            for (name, schedule) in desired_schedules {
                let channel = channel.clone();
                running_tasks.entry(name.clone()).or_insert_with(|| {
                    info!(schedule = %name, "Starting task for new schedule");

                    tokio::spawn(run_schedule_task(
                        schedule.clone(),
                        client.clone(),
                        cache.clone(),
                        channel,
                    ))
                });
            }

            info!(active_tasks = running_tasks.len(), "Task update complete");
        }
    }
}

mod database {
    use chrono::{DateTime, NaiveDateTime, NaiveTime, TimeDelta, Utc};
    use chrono_tz::Tz;
    use eyre::{Result, eyre};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct Schedule {
        pub name: String,
        pub start_date: Option<NaiveDateTime>,
        pub end_date: Option<NaiveDateTime>,
        pub active_time_start: Option<NaiveTime>,
        pub active_time_end: Option<NaiveTime>,
        pub interval: TimeDelta,
        pub message: String,
    }

    impl Schedule {
        /// Check if the schedule is currently active based on date range and time window.
        pub fn is_active(&self, now: DateTime<Tz>) -> bool {
            // Check date range
            if let Some(start) = self.start_date {
                let start_utc = start.and_utc();
                if now < start_utc {
                    return false;
                }
            }

            if let Some(end) = self.end_date {
                let end_utc = end.and_utc();
                if now > end_utc {
                    return false;
                }
            }

            // Check time window (if specified)
            if let (Some(start_time), Some(end_time)) =
                (self.active_time_start, self.active_time_end)
            {
                let current_time = now.time();

                // Handle midnight-spanning windows (e.g., 22:00 - 02:00)
                if end_time < start_time {
                    // Window spans midnight: active if time >= start OR time < end
                    if !(current_time >= start_time || current_time < end_time) {
                        return false;
                    }
                } else {
                    // Normal window: active if time is within range
                    if !(current_time >= start_time && current_time < end_time) {
                        return false;
                    }
                }
            }

            true
        }

        /// Parse interval string into TimeDelta.
        /// Supports formats:
        /// - "hh:mm" (e.g., "01:30" for 1 hour 30 minutes)
        /// - Legacy "30m", "1h", "2h30m" format (backwards compatibility)
        pub fn parse_interval(s: &str) -> Result<TimeDelta> {
            let s = s.trim();
            if s.is_empty() {
                return Err(eyre!("Interval string is empty"));
            }

            // Try parsing as hh:mm format first
            if s.contains(':') {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() != 2 {
                    return Err(eyre!("Invalid hh:mm format: {}", s));
                }

                let hours: i64 = parts[0]
                    .parse()
                    .map_err(|_| eyre!("Invalid hours in hh:mm format: {}", parts[0]))?;

                let minutes: i64 = parts[1]
                    .parse()
                    .map_err(|_| eyre!("Invalid minutes in hh:mm format: {}", parts[1]))?;

                if hours < 0 || !(0..60).contains(&minutes) {
                    return Err(eyre!(
                        "Invalid hh:mm values (hours={}, minutes={})",
                        hours,
                        minutes
                    ));
                }

                let total_seconds = hours * 3600 + minutes * 60;

                // Enforce minimum interval of 1 minute to prevent spam
                if total_seconds < 60 {
                    return Err(eyre!("Interval must be at least 1 minute (got {})", s));
                }

                return TimeDelta::try_seconds(total_seconds)
                    .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds));
            }

            // Legacy format parsing (e.g., "30m", "1h", "2h30m")
            let s = s.to_lowercase();
            let mut total_seconds = 0i64;
            let mut current_num = String::new();

            for ch in s.chars() {
                if ch.is_ascii_digit() {
                    current_num.push(ch);
                } else if ch == 'h' || ch == 'm' || ch == 's' {
                    if current_num.is_empty() {
                        return Err(eyre!("No number before unit '{}'", ch));
                    }

                    let num: i64 = current_num
                        .parse()
                        .map_err(|_| eyre!("Invalid number: {}", current_num))?;

                    total_seconds += match ch {
                        'h' => num * 3600,
                        'm' => num * 60,
                        's' => num,
                        _ => unreachable!(),
                    };

                    current_num.clear();
                } else {
                    return Err(eyre!("Invalid character in interval: '{}'", ch));
                }
            }

            if !current_num.is_empty() {
                return Err(eyre!(
                    "Number without unit at end of interval: {}",
                    current_num
                ));
            }

            if total_seconds == 0 {
                return Err(eyre!("Interval must be greater than zero"));
            }

            // Enforce minimum interval of 1 minute to prevent spam
            if total_seconds < 60 {
                return Err(eyre!(
                    "Interval must be at least 1 minute (got {} seconds)",
                    total_seconds
                ));
            }

            TimeDelta::try_seconds(total_seconds)
                .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds))
        }

        /// Validate the schedule for required fields and logical consistency.
        pub fn validate(&self) -> Result<()> {
            // Name is required and must not be empty
            if self.name.trim().is_empty() {
                return Err(eyre!("Schedule name cannot be empty"));
            }

            // Message is required and must not be empty
            if self.message.trim().is_empty() {
                return Err(eyre!("Schedule message cannot be empty"));
            }

            // Interval must be positive
            if self.interval.num_seconds() <= 0 {
                return Err(eyre!("Interval must be positive"));
            }

            // Interval must be at least 1 minute
            if self.interval.num_seconds() < 60 {
                return Err(eyre!("Interval must be at least 1 minute"));
            }

            // If both start_date and end_date are set, end must be after start
            if let (Some(start), Some(end)) = (self.start_date, self.end_date)
                && end <= start
            {
                return Err(eyre!("End date must be after start date"));
            }

            // Time window validation: both or neither must be set
            match (self.active_time_start, self.active_time_end) {
                (Some(_), None) => {
                    return Err(eyre!(
                        "active_time_end must be set if active_time_start is set"
                    ));
                }
                (None, Some(_)) => {
                    return Err(eyre!(
                        "active_time_start must be set if active_time_end is set"
                    ));
                }
                _ => {} // Both set or both None is valid
            }

            Ok(())
        }
    }

    /// Cache structure for storing loaded schedules with metadata.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct ScheduleCache {
        pub schedules: Vec<Schedule>,
        pub last_updated: DateTime<Utc>,
        pub version: u64,
    }

    impl ScheduleCache {
        /// Create a new empty cache.
        pub fn new() -> Self {
            Self {
                schedules: Vec::new(),
                last_updated: Utc::now(),
                version: 0,
            }
        }

        /// Update cache with new schedules, incrementing version.
        pub fn update(&mut self, schedules: Vec<Schedule>) {
            self.schedules = schedules;
            self.last_updated = Utc::now();
            self.version += 1;
        }
    }
}

