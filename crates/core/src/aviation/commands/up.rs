//! `!up <PLZ/ICAO/IATA/Ort>` — list aircraft visible above the resolved location.

use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, WrapErr as _};
use tracing::{debug, error, warn};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::PrivmsgMessage, transport::Transport,
};

use crate::cooldown::format_cooldown_remaining;
use crate::util::{MAX_RESPONSE_LENGTH, truncate_response};

use super::super::client::AviationClient;
use super::super::formatting::format_altitude;
use super::super::location::{ResolveResult, ResolvedLocation, resolve_location};
use super::super::types::{AltBaro, NearbyAircraft};

const UP_SEARCH_RADIUS_NM: u16 = 15;
const UP_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const UP_ADSB_TIMEOUT: Duration = Duration::from_secs(10);
const UP_ADSBDB_TIMEOUT: Duration = Duration::from_secs(5);
const UP_MAX_CANDIDATES: usize = 10;
const UP_MAX_RESULTS: usize = 5;
const UP_CONE_REFERENCE_ALT_FT: f64 = 35_000.0;

fn cone_distance_nm(ac: &NearbyAircraft, center_lat: f64, center_lon: f64) -> Option<f64> {
    let (Some(ac_lat), Some(ac_lon), Some(alt)) = (ac.lat, ac.lon, &ac.alt_baro) else {
        return None;
    };
    let alt_ft = match alt {
        AltBaro::Feet(ft) if *ft > 0 => *ft as f64,
        _ => return None,
    };
    let distance =
        random_flight::geo::haversine_distance_nm(center_lat, center_lon, ac_lat, ac_lon);
    let max_distance = alt_ft * f64::from(UP_SEARCH_RADIUS_NM) / UP_CONE_REFERENCE_ALT_FT;
    if distance <= max_distance {
        Some(distance)
    } else {
        None
    }
}

pub async fn up_command<T, L>(
    privmsg: &PrivmsgMessage,
    client: &Arc<TwitchIRCClient<T, L>>,
    aviation_client: &AviationClient,
    input: &str,
    cooldown: &crate::cooldown::PerUserCooldown,
) -> Result<()>
where
    T: Transport,
    L: LoginCredentials,
{
    let user = &privmsg.sender.login;
    let input = input.trim();

    // Empty input
    if input.is_empty() {
        if let Err(e) = client
            .say_in_reply_to(
                privmsg,
                "Benutzung: !up <PLZ/ICAO/IATA/Ort> FDM".to_string(),
            )
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    }

    // Check cooldown
    if let Some(remaining) = cooldown.check(user).await {
        debug!(user = %user, remaining_secs = remaining.as_secs(), "!up on cooldown");
        if let Err(e) = client
            .say_in_reply_to(
                privmsg,
                format!(
                    "Bitte warte noch {} Waiting",
                    format_cooldown_remaining(remaining)
                ),
            )
            .await
        {
            error!(error = ?e, "Failed to send cooldown message");
        }
        return Ok(());
    }

    cooldown.record(user).await;

    // Resolve location
    let location = match resolve_location(input, aviation_client).await {
        Ok(ResolveResult::Found(loc)) => loc,
        Ok(ResolveResult::PlzNotFound) => {
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Kenne ich nicht die PLZ FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send unknown PLZ message");
            }
            return Ok(());
        }
        Ok(ResolveResult::NotFound) => {
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Kenne ich nicht FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send not-found message");
            }
            return Ok(());
        }
        Err(e) => {
            error!(error = ?e, input = %input, "Location resolution failed");
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Da ist was schiefgelaufen FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send error message");
            }
            return Ok(());
        }
    };

    let ResolvedLocation {
        lat,
        lon,
        display_name,
    } = &location;
    debug!(input = %input, lat = %lat, lon = %lon, display = %display_name, "Looking up aircraft");

    // Wrap entire API flow in overall timeout
    let result = tokio::time::timeout(UP_COMMAND_TIMEOUT, async {
        // Fetch nearby aircraft
        let aircraft = tokio::time::timeout(
            UP_ADSB_TIMEOUT,
            aviation_client.get_aircraft_nearby(*lat, *lon, UP_SEARCH_RADIUS_NM),
        )
        .await
        .map_err(|_| eyre::eyre!("ADS-B request timed out"))?
        .wrap_err("ADS-B request failed")?;

        // Filter by cone visibility, then by identifier (callsign → registration → hex)
        let candidates: Vec<_> = aircraft
            .iter()
            .filter_map(|ac| {
                let distance_nm = cone_distance_nm(ac, *lat, *lon)?;
                let callsign = ac
                    .flight
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let registration =
                    ac.r.as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                let hex = ac
                    .hex
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let id = callsign
                    .clone()
                    .or_else(|| registration.clone())
                    .or_else(|| hex.clone())?;
                Some((id, callsign, ac, distance_nm))
            })
            .take(UP_MAX_CANDIDATES)
            .collect();

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch routes concurrently
        let mut join_set = tokio::task::JoinSet::new();
        for (id, callsign, ac, distance_nm) in &candidates {
            let av_client = aviation_client.clone();
            let id_owned = id.clone();
            let callsign_owned = callsign.clone();
            let icao_type = ac.t.clone();
            let alt = ac.alt_baro.clone();
            let dist = *distance_nm;
            let (ac_lat, ac_lon) = (
                ac.lat.expect("lat guaranteed by cone_distance_nm"),
                ac.lon.expect("lon guaranteed by cone_distance_nm"),
            );
            let bearing = random_flight::geo::initial_bearing(*lat, *lon, ac_lat, ac_lon);
            let direction = match random_flight::geo::cardinal_direction(bearing) {
                "N" => "↑",
                "NE" => "↗",
                "E" => "→",
                "SE" => "↘",
                "S" => "↓",
                "SW" => "↙",
                "W" => "←",
                "NW" => "↖",
                _ => "?",
            };
            join_set.spawn(async move {
                let route = match callsign_owned {
                    Some(cs) => {
                        let res = tokio::time::timeout(
                            UP_ADSBDB_TIMEOUT,
                            av_client.get_flight_route(&cs),
                        )
                        .await;
                        match res {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => {
                                warn!(callsign = %cs, error = ?e, "adsbdb lookup failed");
                                None
                            }
                            Err(_) => {
                                warn!(callsign = %cs, "adsbdb lookup timed out");
                                None
                            }
                        }
                    }
                    None => None,
                };
                (id_owned, icao_type, alt, route, dist, direction)
            });
        }

        let mut results = Vec::new();
        while let Some(res) = join_set.join_next().await {
            if let Ok(entry) = res {
                results.push(entry);
            }
        }

        results.sort_by(|a, b| a.4.partial_cmp(&b.4).unwrap_or(std::cmp::Ordering::Equal));

        Ok::<_, eyre::Report>(results)
    })
    .await;

    let response = match result {
        Ok(Ok(entries)) if entries.is_empty() => {
            format!("Nix los über {display_name}")
        }
        Ok(Ok(entries)) => {
            let total = entries.len();
            let parts: Vec<String> = entries
                .iter()
                .take(UP_MAX_RESULTS)
                .map(|(id, icao_type, alt, route, dist, direction)| {
                    let typ = icao_type.as_deref().unwrap_or("?");
                    let alt_str = format_altitude(alt);
                    match route {
                        Some(r) => format!(
                            "{id} ({typ}) {origin}→{dest} {alt_str} {dist:.1}nm {direction}",
                            origin = r.origin.iata_code,
                            dest = r.destination.iata_code,
                        ),
                        None => format!("{id} ({typ}) {alt_str} {dist:.1}nm {direction}"),
                    }
                })
                .collect();
            let joined = parts.join(" | ");
            let msg = format!("✈ {total} Flieger über {display_name}: {joined}");
            truncate_response(&msg, MAX_RESPONSE_LENGTH)
        }
        Ok(Err(e)) => {
            error!(error = ?e, input = %input, "!up command failed");
            "Da ist was schiefgelaufen FDM".to_string()
        }
        Err(_) => {
            error!(input = %input, "!up command timed out");
            "Da ist was schiefgelaufen FDM".to_string()
        }
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send !up response");
    }

    Ok(())
}
