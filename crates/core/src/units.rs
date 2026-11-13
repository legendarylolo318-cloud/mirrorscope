//! Generation of the systemd service + timer that refreshes the mirrorlist.

use crate::config::valid_preset_name;
use crate::{Error, Result};

pub const SERVICE_NAME: &str = "mirrorscope-update.service";
pub const TIMER_NAME: &str = "mirrorscope-update.timer";
/// Fixed binary path used inside generated units (also enforced by the helper).
pub const EXE_PATH: &str = "/usr/bin/mirrorscope";

pub struct UnitFiles {
    pub service: String,
    pub timer: String,
}

/// Characters allowed in a config path embedded into a unit file.
pub fn valid_config_path(p: &str) -> bool {
    p.starts_with('/')
        && p.len() <= 200
        && !p.split('/').any(|seg| seg == "..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
}

pub fn generate_units(preset: &str, config_path: &str) -> Result<UnitFiles> {
    if !valid_preset_name(preset) {
        return Err(Error::Other(format!("invalid preset name: {preset:?}")));
    }
    if !valid_config_path(config_path) {
        return Err(Error::Other(format!(
            "invalid config path: {config_path:?}"
        )));
    }
    let service = format!(
        "[Unit]\n\
Description=Refresh Arch Linux mirrorlist with mirrorscope (preset {preset})\n\
Wants=network-online.target\n\
After=network-online.target\n\
\n\
[Service]\n\
Type=oneshot\n\
ExecStart={EXE_PATH} --cli --config {config_path} --preset {preset} --save /etc/pacman.d/mirrorlist\n\
Nice=19\n\
IOSchedulingClass=idle\n"
    );
    let timer = format!(
        "[Unit]\n\
Description=Weekly Arch Linux mirrorlist refresh (preset {preset})\n\
\n\
[Timer]\n\
OnCalendar=weekly\n\
Persistent=true\n\
RandomizedDelaySec=1h\n\
Unit={SERVICE_NAME}\n\
\n\
[Install]\n\
WantedBy=timers.target\n"
    );
    Ok(UnitFiles { service, timer })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates() {
        let u = generate_units("weekly", "/home/u/.config/mirrorscope/config.toml").unwrap();
        assert!(u
            .service
            .contains("--cli --config /home/u/.config/mirrorscope/config.toml --preset weekly"));
        assert!(u.timer.contains("OnCalendar=weekly"));
        assert!(u.timer.contains(SERVICE_NAME));
    }

    #[test]
    fn rejects_injection() {
        assert!(generate_units("a\nExecStart=/bin/sh", "/x/config.toml").is_err());
        assert!(generate_units("ok", "/x/config.toml\nExecStartPre=/bin/sh").is_err());
        assert!(generate_units("ok", "relative/config.toml").is_err());
        assert!(generate_units("ok", "/x/../etc/shadow").is_err());
        assert!(generate_units("ok", "/x/a b").is_err());
    }
}
