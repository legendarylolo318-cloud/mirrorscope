//! Privileged helper logic: strict validation and atomic writes.
//!
//! Everything here is parameterised on directories so it can be tested
//! without root. The binary wires it to `/etc/pacman.d` and `/etc/systemd/system`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chrono::Utc;
use mirrorscope_core::backups::{valid_backup_name, BACKUP_PREFIX};
use thiserror::Error;

pub const MAX_INPUT_BYTES: usize = 256 * 1024;
const SUFFIX: &str = "$repo/os/$arch";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("input is empty")]
    Empty,
    #[error("input is too large")]
    TooLarge,
    #[error("line {0}: contains forbidden control characters")]
    ControlChar(usize),
    #[error("line {0}: only comments and 'Server = <url>' lines are allowed")]
    BadLine(usize),
    #[error("line {0}: invalid mirror URL")]
    BadUrl(usize),
    #[error("no Server lines present")]
    NoServers,
}

#[derive(Debug, Error)]
pub enum HelperError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("{0}")]
    Invalid(String),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("command failed: {0}")]
    Command(String),
}

fn io_err(path: &Path, source: std::io::Error) -> HelperError {
    HelperError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn valid_host(host: &str) -> bool {
    // hostname or IPv4 with optional :port. No userinfo, no IPv6 literals.
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if let Some(p) = port {
        if p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    !name.is_empty()
        && name.len() <= 253
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-'])
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Validate the URL part of a `Server = ` line.
pub fn valid_server_url(url: &str) -> bool {
    let Some(rest) = ["https://", "http://", "rsync://"]
        .iter()
        .find_map(|p| url.strip_prefix(p))
    else {
        return false;
    };
    let Some(body) = rest.strip_suffix(SUFFIX) else {
        return false;
    };
    // body is "host[:port]/path/" and must end with '/'
    let Some(body) = body.strip_suffix('/') else {
        return false;
    };
    let (host, path) = match body.split_once('/') {
        Some((h, p)) => (h, p),
        None => (body, ""),
    };
    if !valid_host(host) {
        return false;
    }
    if path.is_empty() {
        return true;
    }
    path.split('/').all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '+'))
    })
}

/// Validate a complete mirrorlist. Only comments, blank lines and
/// `Server = http(s)|rsync://host/path/$repo/os/$arch` lines pass.
pub fn validate_mirrorlist(text: &str) -> Result<(), ValidationError> {
    if text.trim().is_empty() {
        return Err(ValidationError::Empty);
    }
    if text.len() > MAX_INPUT_BYTES {
        return Err(ValidationError::TooLarge);
    }
    let mut servers = 0;
    for (i, line) in text.split('\n').enumerate() {
        let n = i + 1;
        if line
            .chars()
            .any(|c| (c.is_control() && c != '\t') || c == '\u{7f}')
        {
            return Err(ValidationError::ControlChar(n));
        }
        if line.is_empty() || line.starts_with('#') {
            if !line.is_ascii() {
                return Err(ValidationError::BadLine(n));
            }
            continue;
        }
        let Some(url) = line.strip_prefix("Server = ") else {
            return Err(ValidationError::BadLine(n));
        };
        if !valid_server_url(url) {
            return Err(ValidationError::BadUrl(n));
        }
        servers += 1;
    }
    if servers == 0 {
        return Err(ValidationError::NoServers);
    }
    Ok(())
}

/// Write `content` to `dir/name` atomically: temp file, fsync, rename, fsync dir.
pub fn write_atomic(dir: &Path, name: &str, content: &[u8]) -> Result<(), HelperError> {
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)
            .map_err(|e| io_err(&tmp, e))?;
        f.write_all(content).map_err(|e| io_err(&tmp, e))?;
        f.sync_all().map_err(|e| io_err(&tmp, e))?;
        drop(f);
        let dest = dir.join(name);
        fs::rename(&tmp, &dest).map_err(|e| io_err(&dest, e))?;
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copy the current mirrorlist (if any) to `mirrorlist.bak.<timestamp>`; returns the backup name.
pub fn backup_current(dir: &Path) -> Result<Option<String>, HelperError> {
    let src = dir.join("mirrorlist");
    let md = match fs::symlink_metadata(&src) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(&src, e)),
    };
    if !md.is_file() {
        return Err(HelperError::Invalid(
            "current mirrorlist is not a regular file; refusing to back it up".into(),
        ));
    }
    let data = fs::read(&src).map_err(|e| io_err(&src, e))?;
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let mut name = format!("{BACKUP_PREFIX}{stamp}");
    let mut n = 1;
    while dir.join(&name).exists() {
        name = format!("{BACKUP_PREFIX}{stamp}-{n}");
        n += 1;
    }
    write_atomic(dir, &name, &data)?;
    Ok(Some(name))
}

/// Validate, back up, then atomically replace `dir/mirrorlist`.
pub fn write_mirrorlist(dir: &Path, text: &str) -> Result<Option<String>, HelperError> {
    validate_mirrorlist(text)?;
    let backup = backup_current(dir)?;
    write_atomic(dir, "mirrorlist", text.as_bytes())?;
    Ok(backup)
}

/// Restore `dir/<backup_name>` as the mirrorlist (current one is backed up first).
pub fn restore_backup(dir: &Path, backup_name: &str) -> Result<Option<String>, HelperError> {
    if !valid_backup_name(backup_name) {
        return Err(HelperError::Invalid("invalid backup name".into()));
    }
    let path = dir.join(backup_name);
    let md = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
    if !md.is_file() || md.len() as usize > MAX_INPUT_BYTES {
        return Err(HelperError::Invalid(
            "backup is not a regular, reasonably sized file".into(),
        ));
    }
    let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    write_mirrorlist(dir, &text)
}

/// Install the systemd units into `unit_dir` (atomically). Does not touch systemctl.
pub fn install_units(unit_dir: &Path, preset: &str, config_path: &str) -> Result<(), HelperError> {
    let u = mirrorscope_core::units::generate_units(preset, config_path)
        .map_err(|e| HelperError::Invalid(e.to_string()))?;
    write_atomic(
        unit_dir,
        mirrorscope_core::units::SERVICE_NAME,
        u.service.as_bytes(),
    )?;
    write_atomic(
        unit_dir,
        mirrorscope_core::units::TIMER_NAME,
        u.timer.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "## comment\n\nServer = https://mirror.example.org/archlinux/$repo/os/$arch\nServer = http://a.b-c.example:8080/$repo/os/$arch\nServer = rsync://r.example/arch/$repo/os/$arch\n";

    #[test]
    fn accepts_good() {
        assert_eq!(validate_mirrorlist(GOOD), Ok(()));
        assert_eq!(validate_mirrorlist(&GOOD.replace('\n', "\n\n")), Ok(()));
    }

    #[test]
    fn rejects_empty_and_no_servers() {
        assert_eq!(validate_mirrorlist(""), Err(ValidationError::Empty));
        assert_eq!(validate_mirrorlist("  \n"), Err(ValidationError::Empty));
        assert_eq!(
            validate_mirrorlist("# only comments\n"),
            Err(ValidationError::NoServers)
        );
    }

    #[test]
    fn rejects_non_server_lines() {
        for bad in [
            "Include = /etc/pacman.d/other\n",
            "[options]\n",
            "SigLevel = Never\n",
            "random text\n",
            " Server = https://m.example/$repo/os/$arch\n",
            "Server=https://m.example/$repo/os/$arch\n",
            "server = https://m.example/$repo/os/$arch\n",
            "Server = \n",
            "/etc/passwd\n",
        ] {
            let text = format!("Server = https://ok.example/$repo/os/$arch\n{bad}");
            assert!(validate_mirrorlist(&text).is_err(), "accepted: {bad:?}");
        }
    }

    #[test]
    fn rejects_bad_urls() {
        for bad in [
            "ftp://m.example/$repo/os/$arch",
            "file:///etc/passwd/$repo/os/$arch",
            "https://m.example/$repo/os/$arch/extra",
            "https://m.example/$repo/os",
            "https://m.example/arch",
            "https://m.example/../../etc/$repo/os/$arch",
            "https://m.example/a/../b/$repo/os/$arch",
            "https://m.example//a/$repo/os/$arch",
            "https://m.example/a/./b/$repo/os/$arch",
            "https://user:pw@m.example/$repo/os/$arch",
            "https://m.example/%2e%2e/$repo/os/$arch",
            "https://m.example/a b/$repo/os/$arch",
            "https://m.example/a;b/$repo/os/$arch",
            "https://m.example/a$(id)/$repo/os/$arch",
            "https://m.example/`id`/$repo/os/$arch",
            "https://m.example/?x=1/$repo/os/$arch",
            "https://m.example/#x/$repo/os/$arch",
            "https://m.example:/$repo/os/$arch",
            "https://m.example:99999x/$repo/os/$arch",
            "https://-bad.example/$repo/os/$arch",
            "https://[::1]/$repo/os/$arch",
            "https:///$repo/os/$arch",
            "https://m.example/arch$repo/os/$arch",
            "https://m\u{e9}.example/$repo/os/$arch",
            "https://m.example/\\evil/$repo/os/$arch",
        ] {
            assert!(!valid_server_url(bad), "accepted url: {bad}");
            let text = format!("Server = {bad}\n");
            assert!(validate_mirrorlist(&text).is_err());
        }
    }

    #[test]
    fn rejects_control_characters_and_injection() {
        let ok = "Server = https://ok.example/$repo/os/$arch";
        for text in [
            format!("{ok}\r\nInclude = /x\n"),
            format!("{ok}\rInclude = /x\n"),
            format!("{ok}\0\n"),
            format!("# c\u{1b}[31m\n{ok}\n"),
            format!("# caf\u{e9}\n{ok}\n"),
            format!("{ok}\u{0b}\n"),
        ] {
            assert!(validate_mirrorlist(&text).is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn rejects_oversize() {
        let text = format!(
            "#{}\nServer = https://ok.example/$repo/os/$arch\n",
            "a".repeat(MAX_INPUT_BYTES)
        );
        assert_eq!(validate_mirrorlist(&text), Err(ValidationError::TooLarge));
    }

    #[test]
    fn write_creates_backup_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("mirrorlist"),
            "# old\nServer = https://old.example/$repo/os/$arch\n",
        )
        .unwrap();
        let backup = write_mirrorlist(d.path(), GOOD).unwrap().unwrap();
        assert!(valid_backup_name(&backup));
        assert!(fs::read_to_string(d.path().join(&backup))
            .unwrap()
            .contains("old.example"));
        assert_eq!(
            fs::read_to_string(d.path().join("mirrorlist")).unwrap(),
            GOOD
        );
        // no temp files left behind
        let leftovers: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn invalid_input_leaves_files_untouched() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("mirrorlist"), "orig").unwrap();
        assert!(write_mirrorlist(d.path(), "Include = /evil\n").is_err());
        assert_eq!(
            fs::read_to_string(d.path().join("mirrorlist")).unwrap(),
            "orig"
        );
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_without_existing_mirrorlist() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(write_mirrorlist(d.path(), GOOD).unwrap(), None);
    }

    #[test]
    fn two_quick_writes_get_distinct_backups() {
        let d = tempfile::tempdir().unwrap();
        write_mirrorlist(d.path(), GOOD).unwrap();
        let a = write_mirrorlist(d.path(), GOOD).unwrap().unwrap();
        let b = write_mirrorlist(d.path(), GOOD).unwrap().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn restore_validates_name_and_content() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("mirrorlist"), GOOD).unwrap();
        for bad in [
            "../mirrorlist",
            "mirrorlist",
            "mirrorlist.bak.../x",
            "/etc/passwd",
            "mirrorlist.bak.1/../2",
        ] {
            assert!(restore_backup(d.path(), bad).is_err(), "{bad}");
        }
        // backup with illegal content is refused
        fs::write(d.path().join("mirrorlist.bak.1"), "Include = /x\n").unwrap();
        assert!(restore_backup(d.path(), "mirrorlist.bak.1").is_err());
        // good backup restores
        let other = "Server = https://restored.example/$repo/os/$arch\n";
        fs::write(d.path().join("mirrorlist.bak.2"), other).unwrap();
        restore_backup(d.path(), "mirrorlist.bak.2").unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("mirrorlist")).unwrap(),
            other
        );
    }

    #[test]
    fn restore_refuses_symlink_backup() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("target"), GOOD).unwrap();
        std::os::unix::fs::symlink(d.path().join("target"), d.path().join("mirrorlist.bak.9"))
            .unwrap();
        assert!(restore_backup(d.path(), "mirrorlist.bak.9").is_err());
    }

    #[test]
    fn install_units_validates() {
        let d = tempfile::tempdir().unwrap();
        install_units(
            d.path(),
            "weekly",
            "/home/u/.config/mirrorscope/config.toml",
        )
        .unwrap();
        assert!(d.path().join("mirrorscope-update.timer").is_file());
        assert!(install_units(d.path(), "bad name", "/x/config.toml").is_err());
        assert!(install_units(d.path(), "ok", "/x/y\nExecStart=/bin/sh").is_err());
    }
}
