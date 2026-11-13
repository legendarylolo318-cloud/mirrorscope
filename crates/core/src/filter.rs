use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{Mirror, Protocol, Result};

/// User-facing filter configuration (serialisable into presets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterConfig {
    /// Country names or ISO codes (case-insensitive). Empty = all countries.
    pub countries: Vec<String>,
    pub protocols: Vec<Protocol>,
    /// Maximum hours since last sync. `None` = no limit.
    pub max_age_hours: Option<f64>,
    /// Minimum completion in percent (0-100).
    pub min_completion_pct: f64,
    pub require_ipv4: bool,
    pub require_ipv6: bool,
    /// Only keep URLs matching this regex.
    pub include_regex: String,
    /// Drop URLs matching this regex.
    pub exclude_regex: String,
}

impl Default for FilterConfig {
    fn default() -> Self {
        FilterConfig {
            countries: Vec::new(),
            protocols: vec![Protocol::Https],
            max_age_hours: None,
            min_completion_pct: 100.0,
            require_ipv4: false,
            require_ipv6: false,
            include_regex: String::new(),
            exclude_regex: String::new(),
        }
    }
}

impl FilterConfig {
    /// Short human-readable description used in mirrorlist headers.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.countries.is_empty() {
            parts.push(format!("country={}", self.countries.join(",")));
        }
        let protos: Vec<_> = self.protocols.iter().map(|p| p.as_str()).collect();
        parts.push(format!("protocol={}", protos.join(",")));
        if let Some(a) = self.max_age_hours {
            parts.push(format!("age<={a}h"));
        }
        parts.push(format!("completion>={}%", self.min_completion_pct));
        if self.require_ipv4 {
            parts.push("ipv4".into());
        }
        if self.require_ipv6 {
            parts.push("ipv6".into());
        }
        if !self.include_regex.is_empty() {
            parts.push(format!("include=/{}/", self.include_regex));
        }
        if !self.exclude_regex.is_empty() {
            parts.push(format!("exclude=/{}/", self.exclude_regex));
        }
        parts.join(" ")
    }
}

/// A [`FilterConfig`] with regexes compiled.
pub struct CompiledFilter {
    cfg: FilterConfig,
    include: Option<Regex>,
    exclude: Option<Regex>,
    countries: Vec<String>,
}

impl CompiledFilter {
    pub fn new(cfg: &FilterConfig) -> Result<Self> {
        let include = if cfg.include_regex.trim().is_empty() {
            None
        } else {
            Some(Regex::new(cfg.include_regex.trim())?)
        };
        let exclude = if cfg.exclude_regex.trim().is_empty() {
            None
        } else {
            Some(Regex::new(cfg.exclude_regex.trim())?)
        };
        Ok(CompiledFilter {
            countries: cfg.countries.iter().map(|c| c.to_lowercase()).collect(),
            cfg: cfg.clone(),
            include,
            exclude,
        })
    }

    pub fn matches(&self, m: &Mirror, now: DateTime<Utc>) -> bool {
        if !m.active || m.url.is_empty() {
            return false;
        }
        if !self.cfg.protocols.contains(&m.protocol) {
            return false;
        }
        if !self.countries.is_empty() {
            let name = m.country.to_lowercase();
            let code = m.country_code.to_lowercase();
            if !self.countries.iter().any(|c| *c == name || *c == code) {
                return false;
            }
        }
        if let Some(max) = self.cfg.max_age_hours {
            match m.age_hours(now) {
                Some(age) if age <= max => {}
                _ => return false,
            }
        }
        // The JSON stores completion as a 0..1 fraction.
        let completion = m.completion_pct.unwrap_or(0.0) * 100.0;
        if completion + 1e-9 < self.cfg.min_completion_pct {
            return false;
        }
        if self.cfg.require_ipv4 && !m.ipv4 {
            return false;
        }
        if self.cfg.require_ipv6 && !m.ipv6 {
            return false;
        }
        if let Some(re) = &self.include {
            if !re.is_match(&m.url) {
                return false;
            }
        }
        if let Some(re) = &self.exclude {
            if re.is_match(&m.url) {
                return false;
            }
        }
        true
    }

    pub fn apply(&self, mirrors: &[Mirror], now: DateTime<Utc>) -> Vec<Mirror> {
        mirrors
            .iter()
            .filter(|m| self.matches(m, now))
            .cloned()
            .collect()
    }
}

/// Convenience: compile and apply in one go.
pub fn filter_mirrors(
    mirrors: &[Mirror],
    cfg: &FilterConfig,
    now: DateTime<Utc>,
) -> Result<Vec<Mirror>> {
    Ok(CompiledFilter::new(cfg)?.apply(mirrors, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusDocument;

    fn data() -> (Vec<Mirror>, DateTime<Utc>) {
        let doc = StatusDocument::parse(include_str!("../tests/fixtures/status.json")).unwrap();
        let now = "2026-11-12T10:00:00Z".parse().unwrap();
        (doc.urls, now)
    }

    fn urls(v: &[Mirror]) -> Vec<&str> {
        v.iter().map(|m| m.url.as_str()).collect()
    }

    #[test]
    fn default_is_https_full_completion_active() {
        let (m, now) = data();
        let r = filter_mirrors(&m, &FilterConfig::default(), now).unwrap();
        assert_eq!(
            urls(&r),
            vec![
                "https://de1.example.org/archlinux/",
                "https://fr1.example.org/arch/",
                "https://us2.example.org/archlinux/",
                "https://se1.example.org/archlinux/"
            ]
        );
    }

    #[test]
    fn protocols() {
        let (m, now) = data();
        let cfg = FilterConfig {
            protocols: vec![Protocol::Http, Protocol::Rsync],
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn country_by_name_or_code_case_insensitive() {
        let (m, now) = data();
        let cfg = FilterConfig {
            countries: vec!["germany".into(), "us".into()],
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert_eq!(
            urls(&r),
            vec![
                "https://de1.example.org/archlinux/",
                "https://us2.example.org/archlinux/"
            ]
        );
    }

    #[test]
    fn max_age_excludes_old_and_unknown() {
        let (m, now) = data();
        let cfg = FilterConfig {
            max_age_hours: Some(6.0),
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert!(!urls(&r).contains(&"https://fr1.example.org/arch/"));
        assert!(!urls(&r).contains(&"https://dead.example.org/archlinux/"));
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn completion_threshold() {
        let (m, now) = data();
        let cfg = FilterConfig {
            min_completion_pct: 90.0,
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert!(urls(&r).contains(&"https://fr2.example.org/arch/"));
    }

    #[test]
    fn ip_versions() {
        let (m, now) = data();
        let cfg = FilterConfig {
            require_ipv6: true,
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert_eq!(r.len(), 4); // de1, fr1, us2, se1 all have ipv6
        let cfg = FilterConfig {
            require_ipv4: true,
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert!(!urls(&r).contains(&"https://se1.example.org/archlinux/"));
    }

    #[test]
    fn regex_include_exclude() {
        let (m, now) = data();
        let cfg = FilterConfig {
            include_regex: r"example\.org/arch(linux)?/".into(),
            exclude_regex: r"^https://(fr|se)".into(),
            ..Default::default()
        };
        let r = filter_mirrors(&m, &cfg, now).unwrap();
        assert_eq!(
            urls(&r),
            vec![
                "https://de1.example.org/archlinux/",
                "https://us2.example.org/archlinux/"
            ]
        );
    }

    #[test]
    fn bad_regex_is_an_error() {
        let (m, now) = data();
        let cfg = FilterConfig {
            include_regex: "(".into(),
            ..Default::default()
        };
        assert!(filter_mirrors(&m, &cfg, now).is_err());
    }
}
