//! Helpers for building IRCv3-tagged PRIVMSG lines to inject via
//! `fake_transport::install().inject`, and for parsing captured outgoing lines.

/// Build a plain-user PRIVMSG line (no special badges, minimal realistic tags).
pub fn privmsg(channel: &str, user: &str, text: &str) -> String {
    privmsg_with(channel, user, text, &[])
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
    privmsg_with(channel, user, text, &[("mod", "1")])
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
