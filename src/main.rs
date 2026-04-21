use std::sync::Arc;

use chrono::Utc;
use color_eyre::eyre::Result;
use tokio::sync::oneshot;
use tracing::info;
use tracing_error::ErrorLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};
use twitch_1337::{
    Services, aviation, clock::SystemClock, ensure_data_dir, get_data_dir, llm,
    load_configuration, run_bot, setup_and_verify_twitch_client,
};

fn install_tracing() {
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

#[tokio::main]
pub async fn main() -> Result<()> {
    color_eyre::install()?;
    install_tracing();

    let config = load_configuration().await?;

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedules_enabled = !config.schedules.is_empty(),
        schedule_count = config.schedules.len(),
        "Starting twitch-1337 bot"
    );

    ensure_data_dir().await?;

    let (incoming, client) = setup_and_verify_twitch_client(&config).await?;
    let client = Arc::new(client);

    let llm_client = llm::build_llm_client(config.ai.as_ref())?;

    let aviation_client = match aviation::AviationClient::new() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!(
                error = ?e,
                "Failed to initialize aviation client; aviation commands and flight tracker disabled"
            );
            None
        }
    };

    let services = Services {
        clock: Arc::new(SystemClock),
        llm: llm_client,
        aviation: aviation_client,
        data_dir: get_data_dir(),
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        let _ = shutdown_tx.send(());
    });

    run_bot(client, incoming, config, services, shutdown_rx).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_prefill_threshold_validation() {
        assert!((0.0..=1.0).contains(&0.0));
        assert!((0.0..=1.0).contains(&0.5));
        assert!((0.0..=1.0).contains(&1.0));
        assert!(!(0.0..=1.0).contains(&-0.1));
        assert!(!(0.0..=1.0).contains(&1.1));
    }
}
