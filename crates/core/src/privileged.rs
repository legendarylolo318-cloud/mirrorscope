//! Invoking `mirrorscope-helper`, via `pkexec` when not already root.

use std::io::Write;
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

/// Locate the helper: `$MIRRORSCOPE_HELPER`, a sibling of the running binary, or the installed path.
pub fn helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MIRRORSCOPE_HELPER") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("mirrorscope-helper");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from(INSTALLED_HELPER)
}

/// Run the helper with `args`, feeding `stdin`. Returns its stdout.
/// Blocking: call from `spawn_blocking` in async contexts.
pub fn run_helper(args: &[&str], stdin: &str) -> Result<String> {
    let helper = helper_path();
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
