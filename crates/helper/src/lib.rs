//! Privileged helper logic: strict validation and atomic writes.
//!
//! Everything here is parameterised on directories so it can be tested
//! without root. The binary wires it to `/etc/pacman.d` and `/etc/systemd/system`.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use chrono::Utc;
use mirrorscope_core::backups::{valid_backup_name, BACKUP_PREFIX};
use thiserror::Error;

pub const MAX_INPUT_BYTES: usize = 256 * 1024;
pub use mirrorscope_core::mirrorlist::valid_server_url;

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

/// Open `path` for reading without following symlinks and read it, refusing
/// anything that is not a regular file of at most `MAX_INPUT_BYTES`.
/// The type/size check is done on the opened descriptor (no TOCTOU window).
fn read_regular_nofollow(path: &Path) -> Result<Option<Vec<u8>>, HelperError> {
    let mut f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(HelperError::Invalid(format!(
                "{} is a symlink; refusing to use it",
                path.display()
            )))
        }
        Err(e) => return Err(io_err(path, e)),
    };
    let md = f.metadata().map_err(|e| io_err(path, e))?;
    if !md.is_file() {
        return Err(HelperError::Invalid(format!(
            "{} is not a regular file; refusing to use it",
            path.display()
        )));
    }
    if md.len() > MAX_INPUT_BYTES as u64 {
        return Err(HelperError::Invalid(format!(
            "{} is too large",
            path.display()
        )));
    }
    let mut buf = Vec::with_capacity(md.len() as usize);
    (&mut f)
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| io_err(path, e))?;
    if buf.len() > MAX_INPUT_BYTES {
        return Err(HelperError::Invalid(format!(
            "{} is too large",
            path.display()
        )));
    }
    Ok(Some(buf))
}

/// Write `content` to `dir/name` atomically: temp file (O_EXCL|O_NOFOLLOW),
/// fsync, rename, fsync dir. Refuses to replace a destination that exists
/// but is not a regular file (symlink, directory, device, ...).
pub fn write_atomic(dir: &Path, name: &str, content: &[u8]) -> Result<(), HelperError> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(HelperError::Invalid(format!("invalid file name {name:?}")));
    }
    let dest = dir.join(name);
    match fs::symlink_metadata(&dest) {
        Ok(md) if !md.file_type().is_file() => {
            return Err(HelperError::Invalid(format!(
                "{} exists and is not a regular file; refusing to replace it",
                dest.display()
            )))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&dest, e)),
    }
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    // A stale temp file from a crashed run with a recycled PID would make
    // O_EXCL fail forever. remove_file never follows symlinks.
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o644)
            .open(&tmp)
            .map_err(|e| io_err(&tmp, e))?;
        // mode() is subject to the umask; force 0644 explicitly.
        f.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|e| io_err(&tmp, e))?;
        f.write_all(content).map_err(|e| io_err(&tmp, e))?;
        f.sync_all().map_err(|e| io_err(&tmp, e))?;
        drop(f);
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
    let Some(data) = read_regular_nofollow(&src)? else {
        return Ok(None);
    };
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let mut name = format!("{BACKUP_PREFIX}{stamp}");
    let mut n = 1;
    while fs::symlink_metadata(dir.join(&name)).is_ok() {
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
    let data = read_regular_nofollow(&path)?
        .ok_or_else(|| HelperError::Invalid(format!("backup {backup_name} not found")))?;
    let text = String::from_utf8(data)
        .map_err(|_| HelperError::Invalid("backup is not valid UTF-8".into()))?;
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

    // ---------------------------------------------------------------
    // Adversarial / fuzz-style tests
    // ---------------------------------------------------------------

    /// Independent, deliberately simple re-statement of what may reach
    /// /etc/pacman.d/mirrorlist. Anything the validator accepts must pass this.
    fn independently_safe(text: &str) -> bool {
        text.split('\n').all(|l| {
            if l.is_empty() {
                return true;
            }
            if l.starts_with('#') {
                return l.bytes().all(|b| b == b'\t' || (0x20..0x7f).contains(&b));
            }
            let Some(u) = l.strip_prefix("Server = ") else {
                return false;
            };
            (u.starts_with("https://") || u.starts_with("http://") || u.starts_with("rsync://"))
                && u.ends_with("/$repo/os/$arch")
                && !u.contains("..")
                && !u.contains('@')
                && u.matches("$repo").count() == 1
                && u.matches("$arch").count() == 1
                && u.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"./:_~+-$".contains(&b))
        })
    }

    /// Small deterministic PRNG so the test needs no extra crates.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    const NASTY: &[&str] = &[
        "\n",
        "\r",
        "\r\n",
        "\0",
        "\t",
        "\u{7f}",
        "\u{1b}",
        "\u{85}",
        "\u{2028}",
        "\u{2029}",
        "\u{feff}",
        "\u{200b}",
        "\u{202e}",
        " ",
        "/",
        "..",
        "../",
        "@",
        ":",
        "%",
        "%0a",
        "$",
        "$(id)",
        "`",
        ";",
        "&",
        "|",
        "'",
        "\"",
        "\\",
        "?",
        "#",
        "[",
        "]",
        "Include = /x",
        "\nInclude = /etc/shadow",
        "\nServer = file:///etc/",
        "é",
        "ⅰ",
        "\u{ff0f}",
    ];

    #[test]
    fn fuzz_mutations_never_accept_unsafe_lines() {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let base = GOOD.to_string();
        let mut accepted = 0;
        for _ in 0..20_000 {
            let mut t: Vec<char> = base.chars().collect();
            for _ in 0..1 + rng.below(3) {
                let pos = rng.below(t.len() + 1);
                match rng.below(3) {
                    0 => {
                        let ins: Vec<char> = NASTY[rng.below(NASTY.len())].chars().collect();
                        t.splice(pos..pos, ins);
                    }
                    1 if pos < t.len() => {
                        t.remove(pos);
                    }
                    _ if pos < t.len() => {
                        t[pos] = char::from_u32(rng.below(0x3000) as u32).unwrap_or('x');
                    }
                    _ => {}
                }
            }
            let text: String = t.into_iter().collect();
            if validate_mirrorlist(&text).is_ok() {
                accepted += 1;
                assert!(independently_safe(&text), "accepted unsafe input: {text:?}");
            }
        }
        // sanity: mutations inside comments etc. still pass sometimes
        assert!(accepted > 0);
    }

    #[test]
    fn nasty_insertions_at_every_position_of_a_server_line() {
        let line = "Server = https://m.example:8443/arch/linux/$repo/os/$arch";
        for i in 0..=line.len() {
            if !line.is_char_boundary(i) {
                continue;
            }
            for n in NASTY {
                let text = format!("{}{}{}\n", &line[..i], n, &line[i..]);
                if validate_mirrorlist(&text).is_ok() {
                    assert!(independently_safe(&text), "accepted {text:?}");
                }
            }
        }
    }

    #[test]
    fn unicode_and_line_separator_tricks_rejected() {
        let ok = "Server = https://ok.example/$repo/os/$arch";
        for text in [
            format!("{ok}\u{2028}Include = /x\n"),
            format!("{ok}\u{85}Include = /x\n"),
            format!("\u{feff}{ok}\n"),
            format!("#\u{feff}\n{ok}\n"),
            format!("# \u{202e}gnp.exe\n{ok}\n"),
            "Server = https://ok.example\u{ff0f}$repo/os/$arch\n".to_string(),
            "Server\u{a0}= https://ok.example/$repo/os/$arch\n".to_string(),
            format!("{ok}\n\0"),
            format!("{ok}\n\u{0c}"),
            format!("{ok}\n[core]\nInclude = /etc/pacman.d/evil\n"),
        ] {
            assert!(validate_mirrorlist(&text).is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn ports_are_range_checked() {
        assert!(valid_server_url("https://m.example:443/$repo/os/$arch"));
        assert!(valid_server_url("https://m.example:65535/$repo/os/$arch"));
        assert!(!valid_server_url("https://m.example:0/$repo/os/$arch"));
        assert!(!valid_server_url("https://m.example:65536/$repo/os/$arch"));
        assert!(!valid_server_url("https://m.example:1:2/$repo/os/$arch"));
    }

    #[test]
    fn size_limit_boundary() {
        let line = "Server = https://ok.example/$repo/os/$arch\n";
        let mut text = String::from(line);
        while text.len() + 2 <= MAX_INPUT_BYTES {
            text.push_str("#\n");
        }
        assert!(text.len() <= MAX_INPUT_BYTES);
        assert_eq!(validate_mirrorlist(&text), Ok(()));
        text.push_str("##");
        assert!(text.len() > MAX_INPUT_BYTES);
        assert_eq!(validate_mirrorlist(&text), Err(ValidationError::TooLarge));
    }

    #[test]
    fn generated_mirrorlist_from_fixture_passes_helper() {
        use mirrorscope_core::mirrorlist::{generate, HeaderInfo};
        use mirrorscope_core::{Mirror, StatusDocument};
        let doc =
            StatusDocument::parse(include_str!("../../core/tests/fixtures/status.json")).unwrap();
        let now = "2026-11-12T10:00:00Z".parse().unwrap();
        let cfg = mirrorscope_core::FilterConfig {
            protocols: vec![
                mirrorscope_core::Protocol::Https,
                mirrorscope_core::Protocol::Http,
                mirrorscope_core::Protocol::Rsync,
            ],
            min_completion_pct: 0.0,
            ..Default::default()
        };
        let list = mirrorscope_core::filter::filter_mirrors(&doc.urls, &cfg, now).unwrap();
        let refs: Vec<&Mirror> = list.iter().collect();
        let text = generate(
            &refs,
            &HeaderInfo {
                when: now,
                filters: "weird\nfilter\u{1b}[31m\u{e9}",
                sort: "score",
                source: "https://archlinux.org/mirrors/status/json/",
            },
        );
        assert_eq!(validate_mirrorlist(&text), Ok(()), "{text}");
        assert!(independently_safe(&text));
    }

    #[test]
    fn live_sample_with_non_ascii_countries_passes_helper() {
        use mirrorscope_core::mirrorlist::{generate, HeaderInfo};
        use mirrorscope_core::{FilterConfig, Mirror, StatusDocument};
        let doc = StatusDocument::parse(include_str!(
            "../../core/tests/fixtures/status-live-sample.json"
        ))
        .unwrap();
        let cfg = FilterConfig {
            countries: vec!["Réunion".into(), "Türkiye".into(), "de".into()],
            protocols: vec![
                mirrorscope_core::Protocol::Https,
                mirrorscope_core::Protocol::Http,
            ],
            min_completion_pct: 0.0,
            ..Default::default()
        };
        let now = chrono::Utc::now();
        let list = mirrorscope_core::filter::filter_mirrors(&doc.urls, &cfg, now).unwrap();
        assert!(!list.is_empty());
        let refs: Vec<&Mirror> = list.iter().collect();
        let text = generate(
            &refs,
            &HeaderInfo {
                when: now,
                filters: &cfg.describe(),
                sort: "score",
                source: "https://archlinux.org/mirrors/status/json/",
            },
        );
        assert!(text.contains("country=Reunion,Turkiye,de"));
        assert_eq!(validate_mirrorlist(&text), Ok(()), "{text}");
    }

    #[test]
    fn refuses_symlinked_mirrorlist() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("shadow");
        fs::write(&victim, "secret").unwrap();
        std::os::unix::fs::symlink(&victim, d.path().join("mirrorlist")).unwrap();
        assert!(write_mirrorlist(d.path(), GOOD).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "secret");
        // the symlink itself is left alone and no backup of the victim is made
        assert!(fs::symlink_metadata(d.path().join("mirrorlist"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn refuses_directory_or_fifo_mirrorlist() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("mirrorlist")).unwrap();
        assert!(write_mirrorlist(d.path(), GOOD).is_err());

        let d = tempfile::tempdir().unwrap();
        let fifo = d.path().join("mirrorlist.bak.1");
        let st = std::process::Command::new("mkfifo").arg(&fifo).status();
        if matches!(st, Ok(s) if s.success()) {
            // must not hang (O_NONBLOCK) and must be refused
            assert!(restore_backup(d.path(), "mirrorlist.bak.1").is_err());
        }
    }

    #[test]
    fn planted_temp_symlink_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let tmp = d
            .path()
            .join(format!(".mirrorlist.tmp.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        write_mirrorlist(d.path(), GOOD).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(d.path().join("mirrorlist")).unwrap(),
            GOOD
        );
        assert!(fs::symlink_metadata(&tmp).is_err());
    }

    #[test]
    fn written_file_mode_is_0644() {
        let d = tempfile::tempdir().unwrap();
        write_mirrorlist(d.path(), GOOD).unwrap();
        let mode = fs::metadata(d.path().join("mirrorlist"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn write_atomic_rejects_path_names() {
        let d = tempfile::tempdir().unwrap();
        for n in ["", "../x", "a/b", ".hidden", "/etc/passwd"] {
            assert!(write_atomic(d.path(), n, b"x").is_err(), "{n}");
        }
    }

    #[test]
    fn backup_preserves_exact_bytes() {
        let d = tempfile::tempdir().unwrap();
        let old = "# old\r\nweird but existing content \u{1b}\n";
        fs::write(d.path().join("mirrorlist"), old).unwrap();
        let b = write_mirrorlist(d.path(), GOOD).unwrap().unwrap();
        assert_eq!(fs::read_to_string(d.path().join(b)).unwrap(), old);
    }

    #[test]
    fn install_units_rejects_everything_suspicious() {
        let d = tempfile::tempdir().unwrap();
        for (preset, cfg) in [
            ("weekly", "/etc/shadow"),
            ("weekly", "relative.toml"),
            ("weekly", "/a/../b.toml"),
            ("weekly", "/a/b c.toml"),
            ("weekly", "/a/%n.toml"),
            ("weekly", "/a/b.toml\n[Service]\nExecStartPre=/bin/sh"),
            ("we ekly", "/a/b.toml"),
            ("weekly\n", "/a/b.toml"),
            ("$(id)", "/a/b.toml"),
        ] {
            assert!(
                install_units(d.path(), preset, cfg).is_err(),
                "{preset:?} {cfg:?}"
            );
        }
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }
}
