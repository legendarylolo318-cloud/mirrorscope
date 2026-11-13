use std::path::{Path, PathBuf};

use similar::{ChangeTag, TextDiff};

use crate::{Error, Result};

pub const BACKUP_PREFIX: &str = "mirrorlist.bak.";

#[derive(Debug, Clone)]
pub struct BackupInfo {
    /// File name, e.g. `mirrorlist.bak.20261112-100000`.
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// Whether `name` is an acceptable backup file name (no path components).
pub fn valid_backup_name(name: &str) -> bool {
    match name.strip_prefix(BACKUP_PREFIX) {
        Some(rest) => {
            !rest.is_empty()
                && rest.len() <= 40
                && rest
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
        }
        None => false,
    }
}

/// List backups in `dir`, newest first.
pub fn list_backups(dir: &Path) -> Result<Vec<BackupInfo>> {
    let rd = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    let mut v = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_backup_name(&name) {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        if md.is_file() {
            v.push(BackupInfo {
                name,
                path: entry.path(),
                size: md.len(),
            });
        }
    }
    v.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(v)
}

/// Unified-style diff (`-` old, `+` new, space unchanged).
pub fn diff_text(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for ch in diff.iter_all_changes() {
        let sign = match ch.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(sign);
        out.push_str(ch.value());
        if !ch.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_backup_name("mirrorlist.bak.20261112-100000"));
        assert!(!valid_backup_name("mirrorlist"));
        assert!(!valid_backup_name("mirrorlist.bak."));
        assert!(!valid_backup_name("mirrorlist.bak.../../etc/passwd"));
        assert!(!valid_backup_name("mirrorlist.bak.1/2"));
        assert!(!valid_backup_name("../mirrorlist.bak.1"));
    }

    #[test]
    fn lists_newest_first_ignoring_others() {
        let d = tempfile::tempdir().unwrap();
        for n in [
            "mirrorlist.bak.20240101-000000",
            "mirrorlist.bak.20250101-000000",
            "mirrorlist",
            "other",
        ] {
            std::fs::write(d.path().join(n), "x").unwrap();
        }
        let l = list_backups(d.path()).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].name, "mirrorlist.bak.20250101-000000");
    }

    #[test]
    fn diff_marks_changes() {
        let d = diff_text("a\nb\n", "a\nc\n");
        assert_eq!(d, " a\n-b\n+c\n");
    }
}
