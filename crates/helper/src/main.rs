//! `mirrorscope-helper` — run as root via pkexec. Reads untrusted input from
//! stdin/args, validates it strictly and performs the privileged write.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode};

use mirrorscope_helper::{
    install_units, restore_backup, write_mirrorlist, HelperError, MAX_INPUT_BYTES,
};

const PACMAN_DIR: &str = "/etc/pacman.d";
const UNIT_DIR: &str = "/etc/systemd/system";

fn read_stdin() -> Result<String, HelperError> {
    let mut buf = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| HelperError::Invalid(format!("cannot read stdin: {e}")))?;
    String::from_utf8(buf).map_err(|_| HelperError::Invalid("input is not valid UTF-8".into()))
}

fn systemctl(args: &[&str]) -> Result<(), HelperError> {
    let st = Command::new("/usr/bin/systemctl")
        .args(args)
        .status()
        .map_err(|e| HelperError::Command(e.to_string()))?;
    if st.success() {
        Ok(())
    } else {
        Err(HelperError::Command(format!(
            "systemctl {} failed",
            args.join(" ")
        )))
    }
}

fn run(args: &[String]) -> Result<(), HelperError> {
    let dir = Path::new(PACMAN_DIR);
    match args.first().map(String::as_str) {
        Some("write") if args.len() == 1 => {
            let text = read_stdin()?;
            if let Some(b) = write_mirrorlist(dir, &text)? {
                println!("backup: {b}");
            }
            println!("mirrorlist written");
            Ok(())
        }
        Some("restore") if args.len() == 2 => {
            if let Some(b) = restore_backup(dir, &args[1])? {
                println!("previous mirrorlist saved as {b}");
            }
            println!("restored {}", args[1]);
            Ok(())
        }
        Some("install-timer") if args.len() == 3 => {
            install_units(Path::new(UNIT_DIR), &args[1], &args[2])?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "--now", mirrorscope_core::units::TIMER_NAME])?;
            println!("timer installed and enabled");
            Ok(())
        }
        Some("remove-timer") if args.len() == 1 => {
            let _ = systemctl(&["disable", "--now", mirrorscope_core::units::TIMER_NAME]);
            for n in [
                mirrorscope_core::units::TIMER_NAME,
                mirrorscope_core::units::SERVICE_NAME,
            ] {
                let _ = std::fs::remove_file(Path::new(UNIT_DIR).join(n));
            }
            systemctl(&["daemon-reload"])?;
            println!("timer removed");
            Ok(())
        }
        _ => Err(HelperError::Invalid(
            "usage: mirrorscope-helper write | restore <backup> | install-timer <preset> <config> | remove-timer".into(),
        )),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
