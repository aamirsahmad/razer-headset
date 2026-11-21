mod device;
#[cfg(target_os = "linux")]
mod media;
mod protocol;
#[cfg(target_os = "linux")]
mod udev;

use clap::{Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use device::DeviceError;
use device::{DongleLed, EqPreset, Headset};
#[cfg(target_os = "linux")]
use media::{Action, Media};
use protocol::{EQ_BAND_HZ, EQ_BANDS, EQ_MAX_DB, EQ_MIN_DB};
use std::error::Error;
use std::process::ExitCode;

/// Control a Razer BlackShark V2 HyperSpeed headset from the command line.
#[derive(Parser)]
#[command(name = "razer-headset", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl From<OnOff> for bool {
    fn from(v: OnOff) -> bool {
        matches!(v, OnOff::On)
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    Flat,
    Game,
    Music,
    Movie,
    Custom,
    /// Disable the EQ stage entirely
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum LedArg {
    Off,
    Link,
    Battery,
    Warning,
}

impl From<LedArg> for DongleLed {
    fn from(v: LedArg) -> Self {
        match v {
            LedArg::Off => DongleLed::Off,
            LedArg::Link => DongleLed::Link,
            LedArg::Battery => DongleLed::Battery,
            LedArg::Warning => DongleLed::Warning,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Toggle play/pause on the active media player
    #[cfg(target_os = "linux")]
    PlayPause,
    /// Start playback
    #[cfg(target_os = "linux")]
    Play,
    /// Pause playback
    #[cfg(target_os = "linux")]
    Pause,
    /// Skip to the next track
    #[cfg(target_os = "linux")]
    Next,
    /// Go back to the previous track
    #[cfg(target_os = "linux")]
    Prev,
    /// List media players and their playback status
    #[cfg(target_os = "linux")]
    Players,

    /// Show battery level and charging state
    Battery,
    /// Show or set the auto power-off delay in minutes (0 = never, max 255)
    PowerOff {
        /// New delay in minutes; omit to read the current value
        minutes: Option<u8>,
    },

    /// Show or select the EQ preset (flat, game, music, movie, custom, off)
    Eq {
        /// Preset to activate; omit to show the current state
        preset: Option<PresetArg>,
    },
    /// Show or set the custom EQ curve: 10 gains in dB from 31 Hz to 16 kHz (-9..=6)
    EqBands {
        /// Ten gains, e.g. `3 2 1 0 0 0 0 1 2 3`; omit to show the stored curve
        #[arg(
            num_args = 10,
            allow_negative_numbers = true,
            value_names = ["31Hz", "62Hz", "125Hz", "250Hz", "500Hz", "1kHz", "2kHz", "4kHz", "8kHz", "16kHz"],
            value_parser = clap::value_parser!(i8).range(EQ_MIN_DB as i64..=EQ_MAX_DB as i64)
        )]
        gains: Vec<i8>,
    },
    /// Show or toggle the audio enhancement (bass / spatial expander)
    Enhancement { state: Option<OnOff> },
    /// Show or set mic monitoring: a level 0-15, or `off`
    Sidetone {
        /// `off`, or a level from 0 (quiet) to 15 (loud)
        level: Option<String>,
    },
    /// Show the position of the mic mute switch
    Mic,
    /// Show or toggle Do Not Disturb (block Bluetooth calls while on the dongle)
    Dnd { state: Option<OnOff> },
    /// Show or set what the dongle LED indicates (off, link, battery, warning)
    Led { mode: Option<LedArg> },

    /// Show everything the headset reports
    Status,

    /// Install (or print) the Linux udev rules required for non-root access
    #[cfg(target_os = "linux")]
    SetupUdev {
        /// Print the udev rules to stdout instead of installing them
        #[arg(long)]
        print: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), Box<dyn Error>>;

fn run(cmd: Command) -> Res {
    match cmd {
        #[cfg(target_os = "linux")]
        Command::PlayPause => media(Action::PlayPause),
        #[cfg(target_os = "linux")]
        Command::Play => media(Action::Play),
        #[cfg(target_os = "linux")]
        Command::Pause => media(Action::Pause),
        #[cfg(target_os = "linux")]
        Command::Next => media(Action::Next),
        #[cfg(target_os = "linux")]
        Command::Prev => media(Action::Previous),
        #[cfg(target_os = "linux")]
        Command::Players => {
            let players = Media::connect()?.players()?;
            if players.is_empty() {
                println!("No MPRIS media players running.");
            }
            for p in players {
                println!("{:<24} {}", p.name(), p.status());
            }
            Ok(())
        }

        #[cfg(target_os = "linux")]
        Command::SetupUdev { print } => {
            if print {
                print!("{}", udev::RULES);
            } else {
                udev::install()?;
                println!("Installed {} and reloaded udev.", udev::RULES_PATH);
            }
            Ok(())
        }

        // Dongle-local setting: works even when the headset is off.
        Command::Led { mode } => {
            let mut hs = open_headset()?;
            if let Some(mode) = mode {
                hs.set_dongle_led(mode.into())?;
            }
            print_led(&mut hs)
        }

        Command::Status => {
            let mut hs = open_headset()?;
            let linked = hs.link_up()?;
            println!(
                "Link:            {}",
                if linked { "connected" } else { "headset off" }
            );
            print_led(&mut hs)?;
            if !linked {
                return Ok(());
            }
            print_battery(&mut hs)?;
            print_power_off(&mut hs)?;
            print_eq(&mut hs)?;
            print_eq_bands(&mut hs)?;
            print_enhancement(&mut hs)?;
            print_sidetone(&mut hs)?;
            print_mic(&mut hs)?;
            print_dnd(&mut hs)?;
            println!("Firmware:        {}", hs.firmware()?);
            println!("Serial:          {}", hs.serial()?);
            Ok(())
        }

        other => {
            let mut hs = open_headset()?;
            hs.require_link()?;
            headset_command(&mut hs, other)
        }
    }
}

/// Open the headset, offering to install missing udev rules on permission denied.
#[cfg(target_os = "linux")]
fn open_headset() -> Result<Headset, Box<dyn Error>> {
    match Headset::open() {
        Ok(hs) => Ok(hs),
        Err(DeviceError::Permission(path)) => {
            if udev::prompt_and_install(&path)? {
                return Ok(Headset::open()?);
            }
            Err(DeviceError::Permission(path).into())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn open_headset() -> Result<Headset, Box<dyn Error>> {
    Ok(Headset::open()?)
}

/// Commands that need the RF link up.
fn headset_command(hs: &mut Headset, cmd: Command) -> Res {
    match cmd {
        Command::Battery => print_battery(hs),

        Command::PowerOff { minutes } => {
            if let Some(m) = minutes {
                hs.set_auto_power_off(m)?;
                let now = hs.auto_power_off()?;
                if now != m {
                    return Err(format!("device reports {now} min after setting {m} min").into());
                }
            }
            print_power_off(hs)
        }

        Command::Eq { preset } => {
            match preset {
                Some(PresetArg::Off) => hs.set_eq_enabled(false)?,
                Some(p) => hs.set_eq_preset(match p {
                    PresetArg::Flat => EqPreset::Flat,
                    PresetArg::Game => EqPreset::Game,
                    PresetArg::Music => EqPreset::Music,
                    PresetArg::Movie => EqPreset::Movie,
                    PresetArg::Custom => EqPreset::Custom,
                    PresetArg::Off => unreachable!(),
                })?,
                None => {}
            }
            print_eq(hs)
        }

        Command::EqBands { gains } => {
            if !gains.is_empty() {
                let bands: [i8; EQ_BANDS] =
                    gains.try_into().map_err(|_| "expected exactly 10 gains")?;
                hs.set_custom_eq(&bands)?;
                let now = hs.custom_eq()?;
                if now != bands {
                    return Err(format!("device reports {now:?} after writing {bands:?}").into());
                }
                print_eq(hs)?;
            }
            print_eq_bands(hs)
        }

        Command::Enhancement { state } => {
            if let Some(s) = state {
                let want: bool = s.into();
                hs.set_enhancement(want)?;
                if hs.enhancement()? != want {
                    // Firmware 1.6.11 acknowledges the write but keeps the old value.
                    return Err(format!(
                        "device accepted the write but still reports enhancement {}; this firmware appears to treat the setting as read-only",
                        on_off(!want)
                    )
                    .into());
                }
            }
            print_enhancement(hs)
        }

        Command::Sidetone { level } => {
            if let Some(s) = level.as_deref() {
                hs.set_sidetone(parse_sidetone(s)?)?;
            }
            print_sidetone(hs)
        }

        Command::Mic => print_mic(hs),

        Command::Dnd { state } => {
            if let Some(s) = state {
                hs.set_dnd(s.into())?;
            }
            print_dnd(hs)
        }

        // Handled in `run`.
        #[cfg(target_os = "linux")]
        Command::PlayPause
        | Command::Play
        | Command::Pause
        | Command::Next
        | Command::Prev
        | Command::Players
        | Command::SetupUdev { .. } => unreachable!(),
        Command::Led { .. } | Command::Status => unreachable!(),
    }
}

#[cfg(target_os = "linux")]
fn media(action: Action) -> Res {
    let player = Media::connect()?.send(action)?;
    println!("{action:?} -> {}", player.name());
    Ok(())
}

fn parse_sidetone(s: &str) -> Result<Option<u8>, &'static str> {
    if s.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    s.parse::<u8>()
        .ok()
        .filter(|l| *l <= 15)
        .map(Some)
        .ok_or("sidetone level must be `off` or a number from 0 to 15")
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

fn fmt_minutes(m: u8) -> String {
    if m == 0 {
        "never".into()
    } else {
        format!("{m} min")
    }
}

fn print_battery(hs: &mut Headset) -> Res {
    let b = hs.battery()?;
    println!(
        "Battery:         {}%{}",
        b.percent,
        if b.charging { " (charging)" } else { "" }
    );
    Ok(())
}

fn print_power_off(hs: &mut Headset) -> Res {
    println!("Auto power-off:  {}", fmt_minutes(hs.auto_power_off()?));
    Ok(())
}

fn print_eq(hs: &mut Headset) -> Res {
    let preset = match hs.eq_preset()? {
        Ok(p) => p.name().to_string(),
        Err(raw) => format!("unknown (0x{raw:02X})"),
    };
    let enabled = hs.eq_enabled()?;
    println!(
        "EQ:              {preset}{}",
        if enabled { "" } else { " (EQ stage off)" }
    );
    Ok(())
}

fn print_eq_bands(hs: &mut Headset) -> Res {
    let bands = hs.custom_eq()?;
    let hz: Vec<String> = EQ_BAND_HZ
        .iter()
        .map(|&f| {
            if f >= 1000 {
                format!("{}k", f / 1000)
            } else {
                f.to_string()
            }
        })
        .collect();
    println!(
        "Custom EQ (Hz):  {}",
        hz.iter()
            .map(|s| format!("{s:>4}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "Custom EQ (dB):  {}",
        bands
            .iter()
            .map(|g| format!("{g:>+4}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(())
}

fn print_enhancement(hs: &mut Headset) -> Res {
    println!("Enhancement:     {}", on_off(hs.enhancement()?));
    Ok(())
}

fn print_sidetone(hs: &mut Headset) -> Res {
    let s = hs.sidetone()?;
    println!(
        "Sidetone:        {}",
        if s.enabled {
            format!("on, level {}/15", s.level)
        } else {
            format!("off (level {}/15)", s.level)
        }
    );
    Ok(())
}

fn print_mic(hs: &mut Headset) -> Res {
    println!(
        "Mic switch:      {}",
        if hs.mic_muted()? { "muted" } else { "live" }
    );
    Ok(())
}

fn print_dnd(hs: &mut Headset) -> Res {
    println!("Do Not Disturb:  {}", on_off(hs.dnd()?));
    Ok(())
}

fn print_led(hs: &mut Headset) -> Res {
    let led = match hs.dongle_led()? {
        Ok(m) => m.name().to_string(),
        Err(raw) => format!("unknown (0x{raw:02X})"),
    };
    println!("Dongle LED:      {led}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_eq_bands_with_negative_values() {
        let cli = Cli::try_parse_from([
            "razer-headset",
            "eq-bands",
            "2",
            "-5",
            "0",
            "-3",
            "-2",
            "-5",
            "-7",
            "-6",
            "-4",
            "6",
        ])
        .unwrap();
        match cli.command {
            Command::EqBands { gains } => {
                assert_eq!(gains, vec![2, -5, 0, -3, -2, -5, -7, -6, -4, 6]);
            }
            _ => panic!("expected EqBands"),
        }
    }

    #[test]
    fn cli_parses_eq_bands_without_args() {
        let cli = Cli::try_parse_from(["razer-headset", "eq-bands"]).unwrap();
        match cli.command {
            Command::EqBands { gains } => assert!(gains.is_empty()),
            _ => panic!("expected EqBands"),
        }
    }

    #[test]
    fn cli_rejects_invalid_eq_bands() {
        assert!(Cli::try_parse_from(["razer-headset", "eq-bands", "1", "2", "3"]).is_err());
        assert!(
            Cli::try_parse_from([
                "razer-headset",
                "eq-bands",
                "7",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "razer-headset",
                "eq-bands",
                "-10",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
            ])
            .is_err()
        );
    }

    #[test]
    fn cli_validates_power_off_range() {
        let cli = Cli::try_parse_from(["razer-headset", "power-off", "0"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::PowerOff { minutes: Some(0) }
        ));

        let cli = Cli::try_parse_from(["razer-headset", "power-off", "255"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::PowerOff { minutes: Some(255) }
        ));

        assert!(Cli::try_parse_from(["razer-headset", "power-off", "256"]).is_err());
        assert!(Cli::try_parse_from(["razer-headset", "power-off", "-1"]).is_err());
    }

    #[test]
    fn sidetone_parsing() {
        assert_eq!(parse_sidetone("off"), Ok(None));
        assert_eq!(parse_sidetone("OFF"), Ok(None));
        assert_eq!(parse_sidetone("0"), Ok(Some(0)));
        assert_eq!(parse_sidetone("15"), Ok(Some(15)));
        assert!(parse_sidetone("16").is_err());
        assert!(parse_sidetone("-1").is_err());
        assert!(parse_sidetone("loud").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cli_parses_setup_udev() {
        let cli = Cli::try_parse_from(["razer-headset", "setup-udev"]).unwrap();
        assert!(matches!(cli.command, Command::SetupUdev { print: false }));

        let cli = Cli::try_parse_from(["razer-headset", "setup-udev", "--print"]).unwrap();
        assert!(matches!(cli.command, Command::SetupUdev { print: true }));
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(on_off(true), "on");
        assert_eq!(on_off(false), "off");
        assert_eq!(fmt_minutes(0), "never");
        assert_eq!(fmt_minutes(30), "30 min");
    }
}
