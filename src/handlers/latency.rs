use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use chrono::Utc;
use tokio::{
    sync::broadcast,
    time::{Duration, sleep},
};
use tracing::{debug, info, instrument, warn};
use twitch_irc::{
    TwitchIRCClient, irc,
    login::LoginCredentials,
    message::{PongMessage, ServerMessage},
    transport::Transport,
};

/// Interval between PING measurements
pub(crate) const LATENCY_PING_INTERVAL: Duration = Duration::from_secs(300);

/// Timeout waiting for PONG response
pub(crate) const LATENCY_PING_TIMEOUT: Duration = Duration::from_secs(10);

/// EMA smoothing factor (0.2 = moderate responsiveness)
pub(crate) const LATENCY_EMA_ALPHA: f64 = 0.2;

/// Only log EMA changes at info level when delta exceeds this threshold
pub(crate) const LATENCY_LOG_THRESHOLD: u32 = 10;

/// Periodically measures IRC latency via PING/PONG and updates a shared EMA estimate.
///
/// Sends a PING with a unique nonce every 5 minutes, measures the round-trip time
/// from the matching PONG response, and updates an exponential moving average (EMA)
/// of the one-way latency. The EMA is stored in a shared `AtomicU32` that other
/// handlers (e.g., the 1337 handler) read for timing adjustments.
///
/// The handler is fully independent — PING failures or PONG timeouts are logged
/// but never crash the handler or affect the EMA.
#[instrument(skip(client, broadcast_tx, latency))]
pub async fn run_latency_handler<T, L>(
    client: Arc<TwitchIRCClient<T, L>>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
    latency: Arc<AtomicU32>,
) where
    T: Transport,
    L: LoginCredentials,
{
    let initial = latency.load(Ordering::Relaxed);
    info!(initial_latency_ms = initial, "Latency handler started");

    let mut ema: f64 = f64::from(initial);
    let mut last_logged_ema: u32 = initial;

    loop {
        sleep(LATENCY_PING_INTERVAL).await;

        let nonce = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();

        // Subscribe before sending so we don't miss the PONG on fast connections
        let mut broadcast_rx = broadcast_tx.subscribe();

        let send_time = tokio::time::Instant::now();
        if let Err(e) = client.send_message(irc!["PING", nonce.clone()]).await {
            warn!(error = ?e, "Failed to send PING");
            continue;
        }
        let pong_result = tokio::time::timeout(LATENCY_PING_TIMEOUT, async {
            loop {
                match broadcast_rx.recv().await {
                    // Pongs with mismatched nonces (library keepalives) fall
                    // through the guard to the wildcard arm and keep waiting.
                    Ok(ServerMessage::Pong(PongMessage { source, .. }))
                        if source.params.get(1).map(String::as_str) == Some(nonce.as_str()) =>
                    {
                        return send_time.elapsed();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("Broadcast channel closed during PONG wait");
                        return send_time.elapsed();
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "Latency handler lagged during PONG wait");
                    }
                    _ => {}
                }
            }
        })
        .await;

        let rtt = match pong_result {
            Ok(elapsed) => elapsed,
            Err(_) => {
                warn!("PONG timeout after {:?}", LATENCY_PING_TIMEOUT);
                continue;
            }
        };

        let one_way_ms = rtt.as_millis() as f64 / 2.0;
        ema = LATENCY_EMA_ALPHA * one_way_ms + (1.0 - LATENCY_EMA_ALPHA) * ema;
        let ema_rounded = ema.round() as u32;

        latency.store(ema_rounded, Ordering::Relaxed);

        debug!(
            rtt_ms = rtt.as_millis() as u64,
            one_way_ms = one_way_ms as u64,
            ema_ms = ema_rounded,
            "Latency measurement"
        );

        // Log at info level only when EMA shifts significantly
        if ema_rounded.abs_diff(last_logged_ema) >= LATENCY_LOG_THRESHOLD {
            info!(
                previous_ms = last_logged_ema,
                current_ms = ema_rounded,
                "Latency EMA changed"
            );
            last_logged_ema = ema_rounded;
        }
    }
}
