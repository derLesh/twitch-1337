//! `!döner` / `!doener` — Umrechnung in „Anzahl Döner“ und Stadtpreise via [Döneratlas](https://doeneratlas.de/).

use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{error, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::APP_USER_AGENT;
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};

use super::{Command, CommandContext};

const DONERATLAS_BASE: &str = "https://doeneratlas.de";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(12);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a URL slug like on doeneratlas (`/stadt/muenchen`, `baden-baden`, …).
pub fn doeneratlas_city_slug(input: &str) -> String {
    let mut out = String::new();
    for ch in input.trim().to_lowercase().chars() {
        match ch {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c if (c.is_whitespace() || c == '-' || c == '_')
                && !out.is_empty()
                && !out.ends_with('-') =>
            {
                out.push('-');
            }
            c if c.is_whitespace() || c == '-' || c == '_' => {}
            _ => {}
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    while out.starts_with('-') {
        out.remove(0);
    }
    out
}

fn parse_euro_amount(raw: &str) -> Option<f64> {
    let mut t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t = t.strip_suffix('€').unwrap_or(t).trim();

    match (t.contains(','), t.contains('.')) {
        (true, false) => t.replace(',', ".").parse().ok(),
        (false, true) => t.parse().ok(),
        (true, true) => {
            let last_comma = t.rfind(',').unwrap_or(0);
            let last_dot = t.rfind('.').unwrap_or(0);
            if last_comma > last_dot {
                let no_dots: String = t.chars().filter(|&c| c != '.').collect();
                no_dots.replace(',', ".").parse().ok()
            } else {
                let no_commas: String = t.chars().filter(|&c| c != ',').collect();
                no_commas.parse().ok()
            }
        }
        (false, false) => t.parse().ok(),
    }
}

/// First hero price under `font-size:clamp(72px` — Deutschland-Live-Ø on the homepage.
fn parse_deutschland_average_eur(html: &str) -> Option<f64> {
    let key = r#"style="font-size:clamp(72px"#;
    let idx = html.find(key)?;
    let end = (idx + 800).min(html.len());
    let slice = html.get(idx..end)?;
    let gt = slice.find('>')?;
    let rest = slice.get(gt + 1..)?;
    let end = rest.find("</div>")?;
    let num = rest.get(..end)?.trim();
    parse_euro_amount(num)
}

fn parse_city_average_from_description(html: &str) -> Option<f64> {
    let needle = "Durchschnitt ";
    let idx = html.find(needle)?;
    let rest = html.get(idx + needle.len()..idx + needle.len() + 32)?;
    let end = rest.find('€')?;
    parse_euro_amount(&rest[..end])
}

fn price_after_marker(html: &str, marker: &str) -> Option<f64> {
    let idx = html.find(marker)?;
    let rest = html.get(idx + marker.len()..)?;
    let pat = r#"text-5xl">"#;
    let j = rest.find(pat)?;
    let rest = rest.get(j + pat.len()..)?;
    let end = rest.find('€')?;
    parse_euro_amount(&rest[..end])
}

/// Ø / min / max for a city page (`/stadt/...`).
fn parse_city_price_stats(html: &str) -> Option<(f64, f64, f64)> {
    let avg = parse_city_average_from_description(html)?;
    let min = price_after_marker(html, "§ günstig</span>")?;
    let max = price_after_marker(html, "§ teuer</span>")?;
    Some((avg, min, max))
}

fn format_eur_chat(n: f64) -> String {
    let rounded = (n * 100.0).round() / 100.0;
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        format!("{}€", rounded as i64)
    } else {
        let s = format!("{rounded:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        s.replace('.', ",") + "€"
    }
}

fn format_doener_count(n: f64) -> String {
    let rounded_tenth = (n * 10.0).round() / 10.0;
    if (rounded_tenth - rounded_tenth.round()).abs() < 0.051 {
        format!("{}", rounded_tenth.round() as i64)
    } else {
        format!("{:.1}", rounded_tenth).replace('.', ",")
    }
}

pub struct DoeneratlasCommand {
    http: reqwest::Client,
    cooldown: PerUserCooldown,
}

impl DoeneratlasCommand {
    pub fn new(cooldown: Duration) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("reqwest client for doeneratlas");
        Self {
            http,
            cooldown: PerUserCooldown::new(cooldown),
        }
    }

    async fn fetch_text(&self, url: &str) -> Result<String> {
        let resp = tokio::time::timeout(OPERATION_TIMEOUT, self.http.get(url).send()).await??;
        if !resp.status().is_success() {
            eyre::bail!("HTTP {}", resp.status());
        }
        Ok(tokio::time::timeout(OPERATION_TIMEOUT, resp.text())
            .await
            .map_err(|_| eyre::eyre!("read body timed out"))??)
    }
}

#[async_trait]
impl<T, L> Command<T, L> for DoeneratlasCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!döner"
    }

    fn matches(&self, word: &str) -> bool {
        let Some(stripped) = word.strip_prefix('!') else {
            return false;
        };
        let lowered = stripped.to_lowercase();
        lowered == "döner" || lowered == "doener"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        if let Some(rem) = self.cooldown.check(user).await {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    format!(
                        "Bitte warte noch {} Waiting",
                        format_cooldown_remaining(rem)
                    ),
                )
                .await
            {
                error!(error = ?e, "Failed to send !döner cooldown");
            }
            return Ok(());
        }

        if ctx.args.is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Nutze: !döner <Euro-Betrag> oder !döner check <Stadt> FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send !döner usage");
            }
            return Ok(());
        }

        let response = if ctx.args[0].eq_ignore_ascii_case("check") {
            if ctx.args.len() < 2 {
                "Nach !döner check brauche ich noch einen Stadtnamen FDM".to_string()
            } else {
                let label = ctx.args[1..].join(" ");
                let slug = doeneratlas_city_slug(&label);
                if slug.is_empty() {
                    "Stadtnamen verstehe ich so nicht FDM".to_string()
                } else {
                    let url = format!("{DONERATLAS_BASE}/stadt/{slug}");
                    match self.fetch_text(&url).await {
                        Ok(html) => match parse_city_price_stats(&html) {
                            Some((avg, min, max)) => {
                                format!(
                                    "{} Dönerpreis Ø {}, Min: {}, Max: {}",
                                    label,
                                    format_eur_chat(avg),
                                    format_eur_chat(min),
                                    format_eur_chat(max),
                                )
                            }
                            None => {
                                warn!(%url, "doeneratlas city parse failed");
                                format!(
                                    "Konnte Preise für «{label}» auf Döneratlas nicht lesen FDM"
                                )
                            }
                        },
                        Err(e) => {
                            warn!(error = ?e, %url, "doeneratlas city fetch failed");
                            format!("Döneratlas für «{label}» gerade nicht erreichbar FDM")
                        }
                    }
                }
            }
        } else {
            let amount_raw = ctx.args.join(" ");
            match parse_euro_amount(&amount_raw) {
                None => "Das ist kein gültiger Euro-Betrag FDM".to_string(),
                Some(amount_eur) if amount_eur <= 0.0 => {
                    "Der Betrag muss größer als 0 sein FDM".to_string()
                }
                Some(amount_eur) => {
                    let home_url = DONERATLAS_BASE.to_string();
                    match self.fetch_text(&home_url).await {
                        Ok(html) => match parse_deutschland_average_eur(&html) {
                            Some(avg) if avg > 0.0 => {
                                let n = amount_eur / avg;
                                format!(
                                    "Das wären {} Döner (Ø {} Deutschland, Döneratlas)",
                                    format_doener_count(n),
                                    format_eur_chat(avg),
                                )
                            }
                            _ => {
                                warn!("doeneratlas DE average parse failed");
                                "Konnte den Deutschland-Ø auf Döneratlas nicht lesen FDM"
                                    .to_string()
                            }
                        },
                        Err(e) => {
                            warn!(error = ?e, "doeneratlas home fetch failed");
                            "Döneratlas gerade nicht erreichbar FDM".to_string()
                        }
                    }
                }
            }
        };

        self.cooldown.record(user).await;

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send !döner response");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_koeln_muenchen() {
        assert_eq!(doeneratlas_city_slug("Köln"), "koeln");
        assert_eq!(doeneratlas_city_slug("München"), "muenchen");
        assert_eq!(
            doeneratlas_city_slug("Sasbach am Kaiserstuhl"),
            "sasbach-am-kaiserstuhl"
        );
        assert_eq!(doeneratlas_city_slug("  Baden-Baden  "), "baden-baden");
    }

    #[test]
    fn parse_amounts() {
        assert!((parse_euro_amount("25").unwrap() - 25.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("25,5").unwrap() - 25.5).abs() < 1e-9);
        assert!((parse_euro_amount("25.5").unwrap() - 25.5).abs() < f64::EPSILON);
        assert!((parse_euro_amount("1.234,56").unwrap() - 1234.56).abs() < 0.001);
    }

    #[test]
    fn parse_de_fixture() {
        let html =
            r#"<div class="tabular-nums" style="font-size:clamp(72px, 13vw, 200px)">8,36</div>"#;
        assert!((parse_deutschland_average_eur(html).unwrap() - 8.36).abs() < 0.001);
    }

    #[test]
    fn parse_city_fixture() {
        let html = concat!(
            r#"<meta name="description" content="Dönerpreise in Berlin vergleichen: aktueller Durchschnitt 5,40 €, viele Läden."/>"#,
            r#"<a><span>§ günstig</span><h3>x</h3><p>y</p><span class="x text-5xl">2,00 "#,
            "\u{00a0}",
            r#"€</span></a>"#,
            r#"<a><span>§ teuer</span><h3>x</h3><p>y</p><span class="whatever text-5xl">37,00 "#,
            "\u{00a0}",
            r#"€</span></a>"#,
        );
        let (a, b, c) = parse_city_price_stats(html).expect("parse");
        assert!((a - 5.4).abs() < 0.01);
        assert!((b - 2.0).abs() < 0.01);
        assert!((c - 37.0).abs() < 0.01);
    }
}
