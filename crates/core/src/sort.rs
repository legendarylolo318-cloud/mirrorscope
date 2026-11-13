use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Mirror;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Score,
    Delay,
    /// Time since last sync (youngest first when ascending).
    Age,
    Country,
    /// Measured download rate (fastest first by default).
    Rate,
    Url,
    Completion,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "score" => SortKey::Score,
            "delay" => SortKey::Delay,
            "age" => SortKey::Age,
            "country" => SortKey::Country,
            "rate" => SortKey::Rate,
            "url" => SortKey::Url,
            "completion" => SortKey::Completion,
            _ => return None,
        })
    }

    /// The natural "best first" order for this key.
    pub fn default_order(self) -> SortOrder {
        match self {
            SortKey::Rate | SortKey::Completion => SortOrder::Descending,
            _ => SortOrder::Ascending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    #[default]
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn flip(self) -> Self {
        match self {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        }
    }
}

/// Compare optional values; missing values always sort last regardless of order.
fn cmp_opt<T: PartialOrd>(a: Option<T>, b: Option<T>, order: SortOrder) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
            match order {
                SortOrder::Ascending => o,
                SortOrder::Descending => o.reverse(),
            }
        }
    }
}

/// Sort `mirrors` in place. `rates` maps URL to measured bytes/second.
/// The sort is stable and ties fall back to URL for determinism.
pub fn sort_mirrors(
    mirrors: &mut [Mirror],
    key: SortKey,
    order: SortOrder,
    rates: &HashMap<String, f64>,
    now: DateTime<Utc>,
) {
    mirrors.sort_by(|a, b| {
        let primary = match key {
            SortKey::Score => cmp_opt(a.score, b.score, order),
            SortKey::Delay => cmp_opt(a.delay, b.delay, order),
            SortKey::Age => cmp_opt(a.age_hours(now), b.age_hours(now), order),
            SortKey::Completion => cmp_opt(a.completion_pct, b.completion_pct, order),
            SortKey::Rate => cmp_opt(
                rates.get(&a.url).copied(),
                rates.get(&b.url).copied(),
                order,
            ),
            SortKey::Country => {
                let o = a.country.to_lowercase().cmp(&b.country.to_lowercase());
                match order {
                    SortOrder::Ascending => o,
                    SortOrder::Descending => o.reverse(),
                }
            }
            SortKey::Url => {
                let o = a.url.cmp(&b.url);
                match order {
                    SortOrder::Ascending => o,
                    SortOrder::Descending => o.reverse(),
                }
            }
        };
        primary.then_with(|| a.url.cmp(&b.url))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusDocument;

    fn data() -> (Vec<Mirror>, DateTime<Utc>) {
        let doc = StatusDocument::parse(include_str!("../tests/fixtures/status.json")).unwrap();
        (doc.urls, "2026-11-12T10:00:00Z".parse().unwrap())
    }

    fn first_urls(v: &[Mirror], n: usize) -> Vec<String> {
        v.iter().take(n).map(|m| m.url.clone()).collect()
    }

    #[test]
    fn by_score_ascending_missing_last() {
        let (mut m, now) = data();
        sort_mirrors(
            &mut m,
            SortKey::Score,
            SortOrder::Ascending,
            &HashMap::new(),
            now,
        );
        assert_eq!(m[0].url, "https://us2.example.org/archlinux/");
        assert!(m.last().unwrap().score.is_none());
    }

    #[test]
    fn by_score_descending_still_missing_last() {
        let (mut m, now) = data();
        sort_mirrors(
            &mut m,
            SortKey::Score,
            SortOrder::Descending,
            &HashMap::new(),
            now,
        );
        assert_eq!(m[0].url, "https://fr1.example.org/arch/");
        assert!(m.last().unwrap().score.is_none());
    }

    #[test]
    fn by_delay() {
        let (mut m, now) = data();
        sort_mirrors(
            &mut m,
            SortKey::Delay,
            SortOrder::Ascending,
            &HashMap::new(),
            now,
        );
        assert_eq!(m[0].delay, Some(100));
    }

    #[test]
    fn by_age_youngest_first() {
        let (mut m, now) = data();
        sort_mirrors(
            &mut m,
            SortKey::Age,
            SortOrder::Ascending,
            &HashMap::new(),
            now,
        );
        assert_eq!(m[0].url, "https://se1.example.org/archlinux/");
    }

    #[test]
    fn by_country() {
        let (mut m, now) = data();
        sort_mirrors(
            &mut m,
            SortKey::Country,
            SortOrder::Ascending,
            &HashMap::new(),
            now,
        );
        assert_eq!(m[0].country, "");
        assert_eq!(m.last().unwrap().country, "United States");
    }

    #[test]
    fn by_rate_fastest_first_unmeasured_last() {
        let (mut m, now) = data();
        let mut rates = HashMap::new();
        rates.insert("https://de1.example.org/archlinux/".to_string(), 1.0e6);
        rates.insert("https://us2.example.org/archlinux/".to_string(), 5.0e6);
        rates.insert("https://fr1.example.org/arch/".to_string(), 2.0e6);
        sort_mirrors(
            &mut m,
            SortKey::Rate,
            SortKey::Rate.default_order(),
            &rates,
            now,
        );
        assert_eq!(
            first_urls(&m, 3),
            vec![
                "https://us2.example.org/archlinux/",
                "https://fr1.example.org/arch/",
                "https://de1.example.org/archlinux/"
            ]
        );
    }
}
