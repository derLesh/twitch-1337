//! Twitch IRC client construction and connection verification.
//!
//! Exposed as library functions so `main.rs` stays thin and integration tests
//! can reference the same type aliases without duplicating setup code.

use std::collections::HashSet;

use eyre::{Context as _, Result, bail};
use secrecy::ExposeSecret as _;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Duration;
use tracing::{error, info, instrument, trace};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{LoginCredentials as _, RefreshingLoginCredentials},
    message::{NoticeMessage, ServerMessage},
};

use crate::{
    AuthenticatedLoginCredentials, AuthenticatedTwitchClient, FileBasedTokenStorage,
    config::Configuration,
};

/// Create the Twitch IRC client and message receiver without connecting.
#[instrument(skip(config))]
pub async fn setup_twitch_client(
    config: &Configuration,
) -> Result<(
    UnboundedReceiver<ServerMessage>,
    AuthenticatedTwitchClient,
    AuthenticatedLoginCredentials,
)> {
    let credentials = RefreshingLoginCredentials::init_with_username(
        Some(config.twitch.username.clone()),
        config.twitch.client_id.expose_secret().to_string(),
        config.twitch.client_secret.expose_secret().to_string(),
        FileBasedTokenStorage::new(config.twitch.refresh_token.clone()),
    );
    credentials
        .get_credentials()
        .await
        .wrap_err("Failed to obtain initial credentials")?;
    let twitch_config = ClientConfig::new_simple(credentials.clone());
    let (incoming, client) = TwitchIRCClient::<
        SecureTCPTransport,
        RefreshingLoginCredentials<FileBasedTokenStorage>,
    >::new(twitch_config);
    Ok((incoming, client, credentials))
}

/// Connect, join channel(s), and verify authentication via `GlobalUserState`.
///
/// Returns `Err` if connection times out (30 s) or authentication fails.
#[instrument(skip(config))]
pub async fn setup_and_verify_twitch_client(
    config: &Configuration,
) -> Result<(
    UnboundedReceiver<ServerMessage>,
    AuthenticatedTwitchClient,
    AuthenticatedLoginCredentials,
    String,
)> {
    info!("Setting up and verifying Twitch connection");

    let (mut incoming_messages, client, credentials) = setup_twitch_client(config).await?;

    info!("Connecting to Twitch IRC");
    client.connect().await;

    let mut channels: HashSet<String> = [config.twitch.channel.clone()].into();
    if let Some(ref admin_channel) = config.twitch.admin_channel {
        info!(admin_channel = %admin_channel, "Joining admin channel");
        channels.insert(admin_channel.clone());
    }
    info!(channel = %config.twitch.channel, "Joining channel");
    client.set_wanted_channels(channels)?;

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
                ServerMessage::GlobalUserState(state) => {
                    info!("Connection verified and authenticated");
                    return Ok(state.user_id);
                }
                _ => {}
            }
        }
        bail!("Connection closed during verification")
    };

    let bot_user_id = match tokio::time::timeout(Duration::from_secs(30), verification).await {
        Err(_) => {
            error!("Connection to Twitch IRC Server timed out");
            bail!("Connection to Twitch timed out")
        }
        Ok(result) => result?,
    };

    Ok((incoming_messages, client, credentials, bot_user_id))
}
