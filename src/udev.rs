//! Detection and interactive installation of the Linux udev rules required to
//! talk to the headset's hidraw node without root privileges.

use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub const RULES_PATH: &str = "/etc/udev/rules.d/70-razer-blackshark.rules";
pub const RULES: &str = include_str!("../udev/70-razer-blackshark.rules");

pub fn rules_installed() -> bool {
    rules_installed_at(Path::new(RULES_PATH))
}

fn rules_installed_at(path: &Path) -> bool {
    path.exists()
}

fn is_yes(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// If running interactively and the udev rules file is not yet installed, offer
/// to install it with `sudo` and reload udev. Returns `Ok(true)` if the rules
/// were installed and the caller should retry opening the device.
pub fn prompt_and_install(device_path: &str) -> Result<bool, Box<dyn Error>> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(false);
    }

    if rules_installed() {
        eprintln!(
            "Permission denied opening {device_path}, even though {RULES_PATH} already exists."
        );
        eprintln!(
            "Try unplugging and replugging the dongle, or run `razer-headset setup-udev` to reload udev."
        );
        eprintln!(
            "(Over headless SSH, TAG+=\"uaccess\" only grants access to a local desktop session.)"
        );
        return Ok(false);
    }

    eprintln!("Permission denied opening {device_path}.");
    eprintln!("udev rules ({RULES_PATH}) are not installed yet.\n");
    eprintln!("This will install the following rules and reload udev using sudo:");
    for line in RULES
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
    {
        eprintln!("  {line}");
    }
    eprint!("\nInstall udev rules now? [y/N] ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !is_yes(&answer) {
        return Ok(false);
    }

    install()?;
    eprintln!("Installed {RULES_PATH} and reloaded udev.");
    Ok(true)
}

/// Write the udev rules file and reload/trigger udev via `sudo`.
pub fn install() -> Result<(), Box<dyn Error>> {
    // Authenticate on the interactive TTY before piping rules to `sudo tee`.
    run_checked(Command::new("sudo").arg("-v"), "sudo authentication")?;

    let mut child = Command::new("sudo")
        .args(["tee", RULES_PATH])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to run `sudo tee`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(RULES.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("`sudo tee {RULES_PATH}` exited with {status}").into());
    }

    run_checked(
        Command::new("sudo").args(["udevadm", "control", "--reload"]),
        "udevadm control --reload",
    )?;
    run_checked(
        Command::new("sudo").args([
            "udevadm",
            "trigger",
            "--subsystem-match=hidraw",
            "--subsystem-match=input",
        ]),
        "udevadm trigger",
    )?;
    run_checked(
        Command::new("sudo").args(["udevadm", "settle"]),
        "udevadm settle",
    )?;

    Ok(())
}

fn run_checked(cmd: &mut Command, label: &str) -> Result<(), Box<dyn Error>> {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run `{label}`: {e}"))?;
    if !status.success() {
        return Err(format!("`{label}` exited with {status}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rules_cover_dongle_and_wired() {
        assert!(RULES.contains("1532"));
        assert!(RULES.contains("0565"));
        assert!(RULES.contains("056e"));
        assert!(RULES.contains("TAG+=\"uaccess\""));
    }

    #[test]
    fn yes_prompt_parsing() {
        assert!(is_yes("y"));
        assert!(is_yes("Y\n"));
        assert!(is_yes("yes"));
        assert!(is_yes("  YES \r\n"));
        assert!(!is_yes(""));
        assert!(!is_yes("n"));
        assert!(!is_yes("no"));
        assert!(!is_yes("maybe"));
    }

    #[test]
    fn rules_installed_check() {
        assert!(rules_installed_at(Path::new(
            "udev/70-razer-blackshark.rules"
        )));
        assert!(!rules_installed_at(Path::new(
            "/nonexistent/70-razer-blackshark.rules"
        )));
    }
}
