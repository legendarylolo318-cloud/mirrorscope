use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, FilterConfig, Result, SortKey, SortOrder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// A complete set of user-adjustable settings; also used as a preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub filter: FilterConfig,
    pub sort: SortKey,
    pub order: SortOrder,
    pub number: usize,
    pub concurrency: usize,
    pub timeout_secs: u64,
    /// Restrict to the N most recently synced mirrors before sorting (0 = off).
    pub latest: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            filter: FilterConfig::default(),
            sort: SortKey::Score,
            order: SortOrder::Ascending,
            number: 10,
            concurrency: 8,
            timeout_secs: 5,
            latest: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Theme,
    pub last: Settings,
    pub presets: BTreeMap<String, Settings>,
}

/// `$XDG_CONFIG_HOME/mirrorscope/config.toml` or `~/.config/mirrorscope/config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("mirrorscope").join("config.toml"))
}

impl Config {
    /// Load; a missing file yields defaults, a malformed one is an error.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                toml::from_str(&s).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }
}

/// Preset names are restricted so they are safe in unit files and CLI args.
pub fn valid_preset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Protocol;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/config.toml");
        let mut c = Config::default();
        let mut s = Settings::default();
        s.filter.countries = vec!["Germany".into()];
        s.filter.protocols = vec![Protocol::Https, Protocol::Http];
        s.number = 5;
        c.presets.insert("weekly".into(), s.clone());
        c.theme = Theme::Dark;
        c.save(&p).unwrap();
        let back = Config::load(&p).unwrap();
        assert_eq!(back.presets["weekly"], s);
        assert_eq!(back.theme, Theme::Dark);
    }

    #[test]
    fn missing_file_gives_defaults_and_partial_files_work() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        assert_eq!(Config::load(&p).unwrap().last.number, 10);
        std::fs::write(&p, "[last]\nnumber = 3\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.last.number, 3);
        assert_eq!(c.last.concurrency, 8);
    }

    #[test]
    fn malformed_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[[[").unwrap();
        assert!(Config::load(&p).is_err());
    }

    #[test]
    fn preset_names() {
        assert!(valid_preset_name("weekly-1_a"));
        assert!(!valid_preset_name(""));
        assert!(!valid_preset_name("a b"));
        assert!(!valid_preset_name("a;rm"));
        assert!(!valid_preset_name(&"x".repeat(33)));
    }
}
