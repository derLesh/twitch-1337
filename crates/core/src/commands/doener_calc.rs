//! `!döner` / `!doener` — Zahl in „Anzahl Döner“ umrechnen (Deutschland-Ø nach [Döneratlas](https://doeneratlas.de/)).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{error, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::doener::DoeneratlasClient;
use crate::settings::SettingsHandle;

use super::{Command, CommandContext};

fn parse_euro_amount(raw: &str) -> Option<f64> {
    let mut t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t = t.strip_suffix('€').unwrap_or(t).trim();
    let mut parser = AmountExprParser::new(t);
    let value = parser.parse_expr()?;
    parser.skip_ws();
    (parser.is_eof()).then_some(value)
}

fn parse_number_literal(t: &str) -> Option<f64> {
    let normalized = match (t.contains(','), t.contains('.')) {
        (true, false) => t.replace(',', "."),
        (false, true) | (false, false) => t.to_string(),
        (true, true) => {
            let last_comma = t.rfind(',').unwrap_or(0);
            let last_dot = t.rfind('.').unwrap_or(0);
            if last_comma > last_dot {
                let no_dots: String = t.chars().filter(|&c| c != '.').collect();
                no_dots.replace(',', ".")
            } else {
                t.chars().filter(|&c| c != ',').collect()
            }
        }
    };

    normalized.parse::<f64>().ok()
}

struct AmountExprParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> AmountExprParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn skip_ws(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.bump_char();
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn parse_expr(&mut self) -> Option<f64> {
        let mut lhs = self.parse_term()?;
        loop {
            self.skip_ws();
            match self.peek_char() {
                Some('+') => {
                    self.bump_char();
                    lhs += self.parse_term()?;
                }
                Some('-') => {
                    self.bump_char();
                    lhs -= self.parse_term()?;
                }
                _ => break,
            }
        }
        Some(lhs)
    }

    fn parse_term(&mut self) -> Option<f64> {
        let mut lhs = self.parse_factor()?;
        loop {
            self.skip_ws();
            match self.peek_char() {
                Some('*') => {
                    self.bump_char();
                    lhs *= self.parse_factor()?;
                }
                Some('/') => {
                    self.bump_char();
                    lhs /= self.parse_factor()?;
                }
                _ => break,
            }
        }
        Some(lhs)
    }

    fn parse_factor(&mut self) -> Option<f64> {
        self.skip_ws();
        let mut sign = 1.0;
        loop {
            match self.peek_char() {
                Some('+') => {
                    self.bump_char();
                    self.skip_ws();
                }
                Some('-') => {
                    self.bump_char();
                    self.skip_ws();
                    sign = -sign;
                }
                _ => break,
            }
        }

        let value = match self.peek_char()? {
            '(' => {
                self.bump_char();
                let inner = self.parse_expr()?;
                self.skip_ws();
                (self.bump_char()? == ')').then_some(inner)?
            }
            _ => self.parse_number()?,
        };

        Some(sign * value)
    }

    fn parse_number(&mut self) -> Option<f64> {
        self.skip_ws();
        let start = self.pos;
        let mut saw_digit = false;
        let mut saw_exp = false;
        let mut need_exp_digit = false;

        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                saw_digit = true;
                need_exp_digit = false;
                self.bump_char();
                continue;
            }
            if !saw_exp && matches!(ch, '.' | ',') {
                self.bump_char();
                continue;
            }
            if !saw_exp && matches!(ch, 'e' | 'E') && saw_digit {
                saw_exp = true;
                need_exp_digit = true;
                self.bump_char();
                if matches!(self.peek_char(), Some('+') | Some('-')) {
                    self.bump_char();
                }
                continue;
            }
            break;
        }

        if !saw_digit || need_exp_digit {
            return None;
        }

        parse_number_literal(&self.input[start..self.pos])
    }
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
    if !n.is_finite() {
        return if n.is_sign_negative() || n.is_nan() {
            "0".to_string()
        } else {
            "unendlich viele".to_string()
        };
    }

    let n = n.max(0.0);
    if n == 0.0 {
        return "0".to_string();
    }

    if !(0.1..1_000_000.0).contains(&n) {
        return format!("{n:.2e}").replace('.', ",");
    }

    let rounded_tenth = (n * 10.0).round() / 10.0;
    if rounded_tenth >= 1.0 && (rounded_tenth - rounded_tenth.round()).abs() < 0.051 {
        format!("{}", rounded_tenth.round() as i64)
    } else {
        format!("{:.1}", rounded_tenth).replace('.', ",")
    }
}

fn calculate_doener_count(amount_eur: f64, avg_eur: f64) -> f64 {
    if amount_eur.is_nan() || amount_eur <= 0.0 {
        return 0.0;
    }
    if amount_eur.is_infinite() {
        return f64::INFINITY;
    }

    let count = amount_eur / avg_eur;
    if count.is_nan() || count.is_sign_negative() {
        0.0
    } else {
        count
    }
}

fn matches_doener_trigger(word: &str) -> bool {
    let Some(stripped) = word.strip_prefix('!') else {
        return false;
    };
    let lowered = stripped.to_lowercase();
    lowered == "döner" || lowered == "doener"
}

pub struct DoenerCalcCommand {
    client: Arc<DoeneratlasClient>,
    cooldown: PerUserCooldown,
    settings: SettingsHandle,
}

impl DoenerCalcCommand {
    pub fn new(client: Arc<DoeneratlasClient>, settings: SettingsHandle) -> Self {
        Self {
            client,
            cooldown: PerUserCooldown::new(Duration::ZERO),
            settings,
        }
    }

    fn current_cooldown(&self) -> Duration {
        Duration::from_secs(self.settings.load().cooldowns.doener)
    }
}

#[async_trait]
impl<T, L> Command<T, L> for DoenerCalcCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!döner"
    }

    fn matches(&self, word: &str) -> bool {
        matches_doener_trigger(word)
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        if let Some(rem) = self
            .cooldown
            .check_with_duration(user, self.current_cooldown())
            .await
        {
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
                    "Nutze: !döner <Zahl> — Umrechnung nutzt den Döneratlas-Deutschland-Ø. Stadtpreise: !dpi <Suche> FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send !döner usage");
            }
            return Ok(());
        }

        let amount_raw = ctx.args.join(" ");
        let response = match parse_euro_amount(&amount_raw) {
            None => "Das ist keine gültige Zahl FDM".to_string(),
            Some(amount_eur) => match self.client.national_average_eur().await {
                Ok(avg) if avg > 0.0 => {
                    let n = calculate_doener_count(amount_eur, avg);
                    format!(
                        "Das wären {} Döner (Ø {} Deutschland)",
                        format_doener_count(n),
                        format_eur_chat(avg),
                    )
                }
                Ok(_) => {
                    warn!("doeneratlas returned non-positive national average");
                    "Konnte den Deutschland-Ø auf Döneratlas nicht lesen FDM".to_string()
                }
                Err(e) => {
                    warn!(error = ?e, "doeneratlas stats fetch failed");
                    "Döneratlas gerade nicht erreichbar FDM".to_string()
                }
            },
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
    use std::sync::Arc;

    use super::*;
    use crate::settings::Settings;

    #[test]
    fn parse_amounts() {
        assert!((parse_euro_amount("25").unwrap() - 25.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("25,5").unwrap() - 25.5).abs() < f64::EPSILON);
        assert!((parse_euro_amount("25.5").unwrap() - 25.5).abs() < f64::EPSILON);
        assert!((parse_euro_amount("1.234,56").unwrap() - 1234.56).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_amounts_accept_expressions() {
        assert!((parse_euro_amount("5+5").unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("1-2").unwrap() + 1.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("3*2").unwrap() - 6.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("4/2").unwrap() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_amounts_respect_precedence_and_parentheses() {
        assert!((parse_euro_amount("2+3*4").unwrap() - 14.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("(2+3)*4").unwrap() - 20.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("1,5 + 2,5").unwrap() - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn format_count_keeps_fractional_values() {
        assert_eq!(format_doener_count(0.3), "0,3");
    }

    #[test]
    fn format_count_uses_scientific_notation_for_tiny_values() {
        assert_eq!(format_doener_count(1.0e-33), "1,00e-33");
    }

    #[test]
    fn format_count_uses_scientific_notation_for_huge_values() {
        assert_eq!(format_doener_count(2.16e307), "2,16e307");
    }

    #[test]
    fn calculate_count_clamps_non_positive_amounts_to_zero() {
        assert_eq!(calculate_doener_count(-5.0, 8.32), 0.0);
        assert_eq!(calculate_doener_count(0.0, 8.32), 0.0);
    }

    #[test]
    fn matches_both_spellings_case_insensitive() {
        assert!(matches_doener_trigger("!döner"));
        assert!(matches_doener_trigger("!DÖNER"));
        assert!(matches_doener_trigger("!doener"));
    }

    #[test]
    fn reads_cooldown_from_handle_at_call_time() {
        crate::install_crypto_provider();
        let initial = Settings::compiled_defaults();
        let handle: crate::settings::SettingsHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(initial));
        let cmd = DoenerCalcCommand::new(
            Arc::new(crate::doener::DoeneratlasClient::with_base_url(
                reqwest::Client::new(),
                "http://127.0.0.1:1",
            )),
            handle.clone(),
        );

        let before = cmd.current_cooldown();
        let mut next = Settings::compiled_defaults();
        next.cooldowns.doener = 9;
        handle.store(Arc::new(next));
        let after = cmd.current_cooldown();

        assert_ne!(before, after);
        assert_eq!(after, Duration::from_secs(9));
    }
}
