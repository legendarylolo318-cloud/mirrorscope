use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Https,
    Rsync,
    #[serde(other)]
    Other,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Https => "https",
            Protocol::Rsync => "rsync",
            Protocol::Other => "other",
        }
    }
}

/// One mirror entry from the Arch mirror status JSON.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Mirror {
    pub url: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completion_pct: Option<f64>,
    /// Sync delay in seconds.
    #[serde(default)]
    pub delay: Option<i64>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub ipv4: bool,
    #[serde(default)]
    pub ipv6: bool,
}

impl Mirror {
    /// Age since last sync in hours, if known.
    pub fn age_hours(&self, now: DateTime<Utc>) -> Option<f64> {
        self.last_sync
            .map(|t| (now - t).num_seconds() as f64 / 3600.0)
    }

    /// URL guaranteed to end with a single trailing slash.
    pub fn base_url(&self) -> String {
        let mut u = self.url.trim().to_string();
        if !u.ends_with('/') {
            u.push('/');
        }
        u
    }
}

/// Top-level status document.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusDocument {
    #[serde(default)]
    pub last_check: Option<DateTime<Utc>>,
    #[serde(default)]
    pub version: Option<u32>,
    pub urls: Vec<Mirror>,
}

impl StatusDocument {
    pub fn parse(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/status.json");

    #[test]
    fn parses_fixture() {
        let doc = StatusDocument::parse(FIXTURE).unwrap();
        assert_eq!(doc.urls.len(), 9);
        assert_eq!(doc.version, Some(3));
        let m = &doc.urls[0];
        assert_eq!(m.country_code, "DE");
        assert_eq!(m.protocol, Protocol::Https);
        assert_eq!(m.delay, Some(612));
        assert!(m.last_sync.is_some());
    }

    #[test]
    fn tolerates_nulls_and_unknown_protocols() {
        let doc = StatusDocument::parse(FIXTURE).unwrap();
        let never = doc.urls.iter().find(|m| m.last_sync.is_none()).unwrap();
        assert!(never.score.is_none());
        assert!(doc.urls.iter().any(|m| m.protocol == Protocol::Other));
    }

    #[test]
    fn rejects_garbage() {
        assert!(StatusDocument::parse("not json").is_err());
        assert!(StatusDocument::parse("{}").is_err());
    }

    #[test]
    fn base_url_adds_slash() {
        let mut m = StatusDocument::parse(FIXTURE).unwrap().urls.remove(0);
        m.url = "https://x.example/arch".into();
        assert_eq!(m.base_url(), "https://x.example/arch/");
    }
}
