use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GlobalStats {
    pub total_locations: u32,
    pub total_cities: u32,
    pub min_price: f64,
    pub max_price: f64,
    pub avg_price: f64,
    pub locations_no_price_pct: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CityHit {
    pub city: String,
    pub location_count: u32,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub min_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub max_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub avg_price: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CitiesResponse {
    pub cities: Vec<CityHit>,
}

fn deserialize_optional_price<'de, D>(de: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(f64),
        Text(String),
        Null,
    }
    Ok(match Option::<Raw>::deserialize(de)? {
        None | Some(Raw::Null) => None,
        Some(Raw::Number(n)) => Some(n),
        Some(Raw::Text(s)) => s.trim().parse::<f64>().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_stats_parses_canonical_payload() {
        let raw = r#"{"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#;
        let s: GlobalStats = serde_json::from_str(raw).unwrap();
        assert_eq!(s.total_locations, 6092);
        assert_eq!(s.total_cities, 2202);
        assert!((s.avg_price - 6.1).abs() < 1e-9);
        assert!((s.locations_no_price_pct - 87.1).abs() < 1e-9);
    }

    #[test]
    fn city_hit_parses_string_prices() {
        let raw = r#"{"city":"Hannover","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert_eq!(c.city, "Hannover");
        assert_eq!(c.location_count, 51);
        assert_eq!(c.avg_price, Some(6.0));
    }

    #[test]
    fn city_hit_treats_null_prices_as_none() {
        let raw = r#"{"city":"Handewitt","location_count":1,"min_price":null,"max_price":null,"avg_price":null}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert!(c.min_price.is_none());
        assert!(c.max_price.is_none());
        assert!(c.avg_price.is_none());
    }

    #[test]
    fn city_hit_accepts_numeric_prices() {
        let raw = r#"{"city":"Berlin","location_count":324,"min_price":6.0,"max_price":7.5,"avg_price":6.2}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert_eq!(c.min_price, Some(6.0));
        assert_eq!(c.max_price, Some(7.5));
    }

    #[test]
    fn city_hit_unparseable_price_string_becomes_none() {
        let raw = r#"{"city":"X","location_count":0,"min_price":"abc","max_price":null,"avg_price":null}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert!(c.min_price.is_none());
    }
}
