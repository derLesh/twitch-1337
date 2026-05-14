use crate::doener::atlas::AtlasPublicStats;
use crate::doener::types::CityHit;

const API_DOWN_MESSAGE: &str = "FeelsDankMan Döneratlas API down";

pub fn format_global(s: &AtlasPublicStats) -> String {
    let change = s
        .change_30d
        .map(|d| format!(" Veränderung 30 Tage: {d:+.1}%."))
        .unwrap_or_default();
    format!(
        "Döneratlas DE: Ø {avg:.2}€ (Modus {mode}€), {cities} Städte, {shops} Läden, {reports} Meldungen.{change}",
        avg = s.national_average,
        mode = s.mode_price,
        cities = s.total_cities,
        shops = s.total_shops,
        reports = s.total_reports,
        change = change,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> AtlasPublicStats {
        AtlasPublicStats {
            national_average: 8.36,
            total_cities: 1072,
            total_shops: 1897,
            total_reports: 3514,
            change_30d: Some(1.7),
            mode_price: 7,
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
    fn global_includes_headline_numbers() {
        let s = format_global(&stats());
        assert!(s.contains("Döneratlas DE:"));
        assert!(s.contains("8.36"));
        assert!(s.contains("1072 Städte"));
        assert!(s.contains("1897 Läden"));
        assert!(s.contains("1.7"));
    }

    #[test]
    fn global_omits_change_when_none() {
        let mut s = stats();
        s.change_30d = None;
        let out = format_global(&s);
        assert!(!out.contains("Veränderung"));
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
