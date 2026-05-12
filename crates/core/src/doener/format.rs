use crate::doener::types::{CityHit, GlobalStats};

const API_DOWN_MESSAGE: &str = "FeelsDankMan döner-index API down";

pub fn format_global(s: &GlobalStats) -> String {
    format!(
        "Döner-Index DE: {locations} Buden in {cities} Städten, ⌀ {avg:.2}€ ({min:.2}–{max:.2}€). {no_price}% ohne Preis.",
        locations = s.total_locations,
        cities = s.total_cities,
        avg = s.avg_price,
        min = s.min_price,
        max = s.max_price,
        no_price = format_pct(s.locations_no_price_pct),
    )
}

pub fn format_city(c: &CityHit) -> String {
    let bude = if c.location_count == 1 {
        "Bude"
    } else {
        "Buden"
    };
    match (c.avg_price, c.min_price, c.max_price) {
        (Some(avg), Some(min), Some(max)) => format!(
            "{name}: {count} {bude}, ⌀ {avg:.2}€ ({min:.2}–{max:.2}€).",
            name = c.city,
            count = c.location_count,
        ),
        _ => format!(
            "{name}: {count} {bude}, noch keine Preise.",
            name = c.city,
            count = c.location_count,
        ),
    }
}

pub fn format_did_you_mean(hits: &[CityHit]) -> String {
    let parts: Vec<String> = hits
        .iter()
        .take(3)
        .map(|h| format!("{} ({})", h.city, h.location_count))
        .collect();
    format!("Meintest du: {}?", parts.join(", "))
}

pub fn format_not_found(query: &str) -> String {
    format!("FeelsDankMan keine Stadt für '{query}' gefunden.")
}

pub fn api_down_message() -> &'static str {
    API_DOWN_MESSAGE
}

fn format_pct(p: f64) -> String {
    if (p.round() - p).abs() < f64::EPSILON {
        format!("{}", p.round() as i64)
    } else {
        format!("{p:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> GlobalStats {
        GlobalStats {
            total_locations: 6092,
            total_cities: 2202,
            min_price: 5.5,
            max_price: 9.0,
            avg_price: 6.1,
            locations_no_price_pct: 87.0,
        }
    }

    fn hit(city: &str, n: u32, prices: Option<(f64, f64, f64)>) -> CityHit {
        CityHit {
            city: city.into(),
            location_count: n,
            min_price: prices.map(|(min, _, _)| min),
            max_price: prices.map(|(_, _, max)| max),
            avg_price: prices.map(|(_, avg, _)| avg),
        }
    }

    #[test]
    fn global_matches_golden_string() {
        assert_eq!(
            format_global(&stats()),
            "Döner-Index DE: 6092 Buden in 2202 Städten, ⌀ 6.10€ (5.50–9.00€). 87% ohne Preis."
        );
    }

    #[test]
    fn global_keeps_decimal_in_no_price_pct_when_non_integer() {
        let mut s = stats();
        s.locations_no_price_pct = 87.1;
        assert!(format_global(&s).contains("87.1% ohne Preis"));
    }

    #[test]
    fn city_with_prices_uses_plural_buden() {
        let c = hit("Hannover", 51, Some((6.0, 6.0, 6.0)));
        assert_eq!(format_city(&c), "Hannover: 51 Buden, ⌀ 6.00€ (6.00–6.00€).");
    }

    #[test]
    fn city_with_one_location_uses_singular_bude() {
        let c = hit("Handewitt", 1, None);
        assert_eq!(format_city(&c), "Handewitt: 1 Bude, noch keine Preise.");
    }

    #[test]
    fn city_with_one_location_with_price_uses_singular_bude() {
        let c = hit("X", 1, Some((7.0, 7.0, 7.0)));
        assert_eq!(format_city(&c), "X: 1 Bude, ⌀ 7.00€ (7.00–7.00€).");
    }

    #[test]
    fn did_you_mean_lists_top_three() {
        let hits = vec![
            hit("Hannover", 51, None),
            hit("Hanau", 3, None),
            hit("Handewitt", 1, None),
            hit("Hannover-Land", 0, None),
        ];
        assert_eq!(
            format_did_you_mean(&hits),
            "Meintest du: Hannover (51), Hanau (3), Handewitt (1)?"
        );
    }

    #[test]
    fn not_found_quotes_query() {
        assert_eq!(
            format_not_found("xyz"),
            "FeelsDankMan keine Stadt für 'xyz' gefunden."
        );
    }
}
