//! Shared output formatters for aviation commands.

use super::types::AltBaro;

pub(super) fn format_altitude(alt: &Option<AltBaro>) -> String {
    match alt {
        Some(AltBaro::Feet(ft)) if *ft >= 1000 => format!("FL{}", ft / 100),
        Some(AltBaro::Feet(ft)) => format!("{ft}ft"),
        Some(AltBaro::Ground) => "GND".to_string(),
        None => "?".to_string(),
    }
}
