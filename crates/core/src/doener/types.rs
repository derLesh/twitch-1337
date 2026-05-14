use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CityHit {
    pub city: String,
    /// Match key für `GET /app-api/public/cities?slug=…`.
    #[serde(default)]
    pub slug: String,
    pub location_count: u32,
    /// Shops whose `current_price` appeared in the search response for this slug (often << [`CityHit::location_count`]).
    #[serde(default)]
    pub priced_shop_sample: u32,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub min_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub max_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub avg_price: Option<f64>,
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
    fn city_hit_parses_string_prices() {
        let raw = r#"{"city":"Hannover","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert_eq!(c.city, "Hannover");
        assert_eq!(c.slug, "");
        assert_eq!(c.priced_shop_sample, 0);
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
