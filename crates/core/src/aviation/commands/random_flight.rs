use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr};
use tracing::{error, instrument, warn};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::PrivmsgMessage, transport::Transport,
};

use crate::commands::{Command, CommandContext};
use crate::util::parse_flight_duration;

pub struct RandomFlightCommand;

#[async_trait]
impl<T, L> Command<T, L> for RandomFlightCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!fl"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        flight_command(
            ctx.privmsg,
            ctx.client,
            ctx.args.first().copied(),
            ctx.args.get(1).copied(),
        )
        .await
    }
}

#[instrument(skip(privmsg, client), fields(user = %privmsg.sender.login))]
pub(crate) async fn flight_command<T, L>(
    privmsg: &PrivmsgMessage,
    client: &Arc<TwitchIRCClient<T, L>>,
    aircraft_code: Option<&str>,
    duration_str: Option<&str>,
) -> Result<()>
where
    T: Transport,
    L: LoginCredentials,
{
    const USAGE_MSG: &str = "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM";

    let (Some(aircraft_code), Some(duration_str)) = (aircraft_code, duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from(USAGE_MSG))
            .await
        {
            error!(error = ?e, "Failed to send flight usage message");
        }
        return Ok(());
    };

    let Some(aircraft) = random_flight::aircraft_by_icao_type(aircraft_code) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das Flugzeug kenn ich nich FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'unknown aircraft' error message");
        }
        return Ok(());
    };

    let Some(duration) = parse_flight_duration(duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from(USAGE_MSG))
            .await
        {
            error!(error = ?e, "Failed to send flight duration usage message");
        }
        return Ok(());
    };

    // Can take many retries internally
    let result = tokio::task::spawn_blocking(move || {
        random_flight::generate_flight_plan(aircraft, duration, None)
    })
    .await
    .wrap_err("Flight plan generation task panicked")?;

    let fp = match result {
        Ok(fp) => fp,
        Err(e) => {
            warn!(error = ?e, "Flight plan generation failed");
            if let Err(e) = client
                .say_in_reply_to(
                    privmsg,
                    String::from("Hab keine Route gefunden, versuch mal ne andere Zeit FDM"),
                )
                .await
            {
                error!(error = ?e, "Failed to send 'no route found' error message");
            }
            return Ok(());
        }
    };

    let time_str = crate::cooldown::format_duration_hm(fp.block_time);

    let response = format!(
        "{} → {} | {:.0} nm | {} | FL{} | {}",
        fp.departure.icao,
        fp.arrival.icao,
        fp.distance_nm,
        time_str,
        fp.cruise_altitude_ft / 100,
        fp.simbrief_url(),
    );

    client.say_in_reply_to(privmsg, response).await?;

    Ok(())
}
