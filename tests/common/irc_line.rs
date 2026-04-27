//! Helpers for building IRCv3-tagged PRIVMSG lines to inject via
//! `fake_transport::install().inject`, and for parsing captured outgoing lines.

/// Build a plain-user PRIVMSG line (no special badges, minimal realistic tags).
pub fn privmsg(channel: &str, user: &str, text: &str) -> String {
    privmsg_with(channel, user, text, &[])
}

pub fn reply_privmsg(
    channel: &str,
    user: &str,
    text: &str,
    parent_user: &str,
    parent_text: &str,
) -> String {
    privmsg_with(
        channel,
        user,
        text,
        &[
            (
                "reply-parent-msg-id",
                "11111111-1111-1111-1111-111111111111",
            ),
            ("reply-parent-user-id", "22222"),
            ("reply-parent-user-login", parent_user),
            ("reply-parent-display-name", parent_user),
            ("reply-parent-msg-body", &escape_tag_value(parent_text)),
        ],
    )
}

fn escape_tag_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(' ', "\\s")
        .replace(';', "\\:")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

/// Build a PRIVMSG line with extra tags. Entries in `extra_tags` whose key
/// matches a default override the default value; new keys are appended.
pub fn privmsg_with(channel: &str, user: &str, text: &str, extra_tags: &[(&str, &str)]) -> String {
    let mut tags: Vec<(&str, String)> = vec![
        ("badge-info", String::new()),
        ("badges", String::new()),
        ("color", String::new()),
        ("display-name", user.to_owned()),
        ("emotes", String::new()),
        ("first-msg", "0".to_owned()),
        ("flags", String::new()),
        ("id", "00000000-0000-0000-0000-000000000000".to_owned()),
        ("mod", "0".to_owned()),
        ("returning-chatter", "0".to_owned()),
        ("room-id", "12345".to_owned()),
        ("subscriber", "0".to_owned()),
        ("tmi-sent-ts", "1700000000000".to_owned()),
        ("turbo", "0".to_owned()),
        ("user-id", "67890".to_owned()),
        ("user-type", String::new()),
    ];
    for (k, v) in extra_tags {
        if let Some(existing) = tags.iter_mut().find(|(name, _)| name == k) {
            existing.1 = (*v).to_owned();
        } else {
            tags.push((k, (*v).to_owned()));
        }
    }
    let tag_str = tags
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    format!("@{tag_str} :{user}!{user}@{user}.tmi.twitch.tv PRIVMSG #{channel} :{text}")
}

pub fn privmsg_as_broadcaster(channel: &str, user: &str, text: &str) -> String {
    privmsg_with(
        channel,
        user,
        text,
        &[("badges", "broadcaster/1"), ("mod", "0")],
    )
}

pub fn privmsg_as_mod(channel: &str, user: &str, text: &str) -> String {
    // Real Twitch sends both the `mod` tag and a `moderator/1` entry in
    // `badges`. Handlers (e.g. `!suspend`, `!p create`) gate on the badge
    // list, so set both for realism.
    privmsg_with(
        channel,
        user,
        text,
        &[("badges", "moderator/1"), ("mod", "1")],
    )
}

/// Build a PRIVMSG with a specific `tmi-sent-ts` (Unix milliseconds).
///
/// `server_timestamp` is parsed from this tag; use it when the handler filters
/// by clock time (e.g. the 13:37 monitor's hour/minute check).
pub fn privmsg_at(channel: &str, user: &str, text: &str, tmi_ts_ms: i64) -> String {
    privmsg_with(
        channel,
        user,
        text,
        &[("tmi-sent-ts", &tmi_ts_ms.to_string())],
    )
}

/// Extract message body from a captured outgoing IRC line
/// (`PRIVMSG #chan :body` or `@tags :prefix PRIVMSG #chan :body`).
///
/// Splits on the first ` :` pair. Works for the PRIVMSG outputs captured
/// from `TwitchIRCClient.say()`, which do not carry tags on outbound.
pub fn parse_privmsg_text(raw: &str) -> String {
    if let Some(idx) = raw.find(" :") {
        raw[idx + 2..].trim_end_matches(['\r', '\n']).to_owned()
    } else {
        raw.to_owned()
    }
}
