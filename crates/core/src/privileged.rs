//! Invoking `mirrorscope-helper`, via `pkexec` when not already root.

use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::{Error, Result};

const INSTALLED_HELPER: &str = "/usr/lib/mirrorscope/mirrorscope-helper";

/// True if the effective uid is 0 (parsed from /proc, no unsafe code).
pub fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|v| v == "0"))
        })
        .unwrap_or(false)
}

/// Where `--install-helper` puts the polkit policy.
pub const POLICY_PATH: &str = "/usr/share/polkit-1/actions/io.github.mirrorscope.policy";
/// Where `--install-helper` puts a root-owned copy of the main binary (used by the timer).
pub const INSTALLED_EXE: &str = crate::units::EXE_PATH;

/// True when running from inside an AppImage (the runtime exports `$APPIMAGE`).
pub fn in_appimage() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}

/// Locate the helper: `$MIRRORSCOPE_HELPER`, then (outside an AppImage) a
/// sibling of the running binary, then the installed path.
///
/// Inside an AppImage the bundled helper is never used: pkexec runs it as root,
/// and root cannot read the user's FUSE mount. The installed copy is required.
pub fn helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MIRRORSCOPE_HELPER") {
        return PathBuf::from(p);
    }
    if !in_appimage() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let cand = dir.join("mirrorscope-helper");
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    PathBuf::from(INSTALLED_HELPER)
}

/// Files bundled next to the running binary (AppDir layout: `usr/bin/mirrorscope`,
/// `usr/lib/mirrorscope/mirrorscope-helper`, `usr/share/polkit-1/actions/*.policy`).
pub struct BundledFiles {
    pub exe: PathBuf,
    pub helper: PathBuf,
    pub policy: PathBuf,
}

pub fn bundled_files() -> Result<BundledFiles> {
    let exe = std::env::current_exe().map_err(|e| Error::Other(format!("current_exe: {e}")))?;
    let usr = exe
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| Error::Other("cannot locate bundled files".into()))?;
    let f = BundledFiles {
        helper: usr.join("lib/mirrorscope/mirrorscope-helper"),
        policy: usr.join("share/polkit-1/actions/io.github.mirrorscope.policy"),
        exe,
    };
    for p in [&f.helper, &f.policy] {
        if !p.is_file() {
            return Err(Error::Other(format!(
                "bundled file {} not found (run --install-helper from the AppImage)",
                p.display()
            )));
        }
    }
    Ok(f)
}

/// Install the helper, a copy of the main binary and the polkit policy
/// system-wide, via one `pkexec` prompt (or directly when root).
///
/// Root cannot read the AppImage FUSE mount, so the files are first copied
/// (as the user) into a private 0700 temp directory.
pub fn install_system_files() -> Result<String> {
    let src = bundled_files()?;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let stage = base.join(format!("mirrorscope-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage)
        .map_err(|e| Error::io(&stage, e))?;
    let result = (|| {
        let staged = [
            (src.helper.clone(), stage.join("mirrorscope-helper")),
            (src.exe.clone(), stage.join("mirrorscope")),
            (
                src.policy.clone(),
                stage.join("io.github.mirrorscope.policy"),
            ),
        ];
        for (from, to) in &staged {
            std::fs::copy(from, to).map_err(|e| Error::io(from, e))?;
        }
        // Positional arguments avoid any shell quoting of paths.
        let script = "set -e; \
            install -Dm755 \"$1\" /usr/lib/mirrorscope/mirrorscope-helper; \
            install -Dm755 \"$2\" /usr/lib/mirrorscope/mirrorscope; \
            install -Dm644 \"$3\" /usr/share/polkit-1/actions/io.github.mirrorscope.policy";
        let mut cmd = if is_root() {
            Command::new("/bin/sh")
        } else {
            let mut c = Command::new("pkexec");
            c.arg("/bin/sh");
            c
        };
        cmd.arg("-c").arg(script).arg("mirrorscope-install");
        for (_, to) in &staged {
            cmd.arg(to);
        }
        // Inherit stdio so a text-mode polkit agent can prompt on the terminal.
        let status = cmd
            .status()
            .map_err(|e| Error::Helper(format!("cannot start pkexec: {e}")))?;
        if status.success() {
            Ok(format!(
                "installed {INSTALLED_HELPER}, {INSTALLED_EXE} and {POLICY_PATH}"
            ))
        } else {
            Err(Error::Helper(match status.code() {
                Some(126) => "authorization dismissed or denied".into(),
                Some(127) => "polkit could not authorize the installation".into(),
                c => format!("installation failed (exit status {c:?})"),
            }))
        }
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Run the helper with `args`, feeding `stdin`. Returns its stdout.
/// Blocking: call from `spawn_blocking` in async contexts.
pub fn run_helper(args: &[&str], stdin: &str) -> Result<String> {
    let helper = helper_path();
    if !helper.is_file() {
        return Err(Error::Helper(format!(
            "{} is not installed. Run `mirrorscope --install-helper` once \
             (installs the helper and its polkit policy system-wide).",
            helper.display()
        )));
    }
    let mut cmd = if is_root() {
        Command::new(&helper)
    } else {
        let mut c = Command::new("pkexec");
        c.arg(&helper);
        c
    };
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Helper(format!("cannot start helper ({}): {e}", helper.display())))?;
    if let Some(mut si) = child.stdin.take() {
        // A helper that rejects input may exit before reading everything.
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::Helper(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let code = out.status.code().unwrap_or(-1);
        Err(Error::Helper(match code {
            126 => "authorization dismissed or denied".to_string(),
            127 => "polkit could not authorize the helper".to_string(),
            _ if err.is_empty() => format!("helper exited with status {code}"),
            _ => err,
        }))
    }
}
