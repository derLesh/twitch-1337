use async_trait::async_trait;
use chrono::Utc;
use color_eyre::eyre::{self, Result};
use secrecy::{ExposeSecret, SecretString};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};
use tracing::{debug, instrument, warn};
use twitch_irc::login::{TokenStorage, UserAccessToken};

use crate::util::get_data_dir;

/// Token storage implementation that persists tokens to disk.
///
/// Falls back to initial refresh token from config on first load if no token file exists.
#[derive(Debug)]
pub struct FileBasedTokenStorage {
    path: std::path::PathBuf,
    initial_refresh_token: SecretString,
}

impl FileBasedTokenStorage {
    pub fn new(initial_refresh_token: SecretString) -> Self {
        Self {
            path: get_data_dir().join("token.ron"),
            initial_refresh_token,
        }
    }

    fn fresh_token_from_config(&self) -> UserAccessToken {
        let now = Utc::now();
        UserAccessToken {
            access_token: String::new(),
            refresh_token: self.initial_refresh_token.expose_secret().to_string(),
            created_at: now,
            expires_at: Some(now),
        }
    }
}

#[async_trait]
impl TokenStorage for FileBasedTokenStorage {
    type LoadError = eyre::Report;
    type UpdateError = eyre::Report;

    #[instrument(skip(self))]
    async fn load_token(&mut self) -> Result<UserAccessToken, Self::LoadError> {
        match fs::read_to_string(&self.path).await {
            Ok(contents) => {
                debug!(
                    path = %self.path.display(),
                    "Loading user access token from file"
                );
                let token: UserAccessToken = ron::from_str(&contents)?;
                let config_token = self.initial_refresh_token.expose_secret();
                if token.refresh_token != config_token {
                    warn!("Refresh token in config differs from stored token; using config token");
                    return Ok(self.fresh_token_from_config());
                }
                Ok(token)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                warn!("Token file not found, using refresh token from configuration");
                Ok(self.fresh_token_from_config())
            }
            Err(e) => Err(eyre::Report::from(e).wrap_err("Failed to read token file")),
        }
    }

    #[instrument(skip(self, token))]
    async fn update_token(&mut self, token: &UserAccessToken) -> Result<(), Self::UpdateError> {
        debug!(path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        let tmp_path = self.path.with_extension("ron.tmp");
        File::create(&tmp_path).await?.write_all(&buffer).await?;
        fs::rename(&tmp_path, &self.path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use twitch_irc::login::UserAccessToken;

    fn make_token(expires_at: Option<chrono::DateTime<Utc>>) -> UserAccessToken {
        UserAccessToken {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            created_at: Utc::now(),
            expires_at,
        }
    }

    #[test]
    fn ron_roundtrip_expires_at_none() {
        let t = make_token(None);
        let s = ron::to_string(&t).unwrap();
        let t2: UserAccessToken = ron::from_str(&s).unwrap();
        assert_eq!(t2.expires_at, None);
    }

    #[test]
    fn ron_roundtrip_expires_at_some() {
        let now = Utc::now();
        let t = make_token(Some(now));
        let s = ron::to_string(&t).unwrap();
        let t2: UserAccessToken = ron::from_str(&s).unwrap();
        // subsecond precision may differ; compare at second granularity
        assert_eq!(t2.expires_at.unwrap().timestamp(), now.timestamp());
    }
}
