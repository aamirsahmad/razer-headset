//! Talks to the BlackShark V2 HyperSpeed over hidraw using the frame format in
//! [`crate::protocol`].

use crate::protocol::{
    self, DOMAIN_DONGLE, DOMAIN_HEADSET, EQ_BANDS, REPORT_LEN, Response, eq_preset, reg,
};
use hidapi::{HidApi, HidDevice};
use std::error::Error;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

pub const RAZER_VID: u16 = 0x1532;
/// 2.4 GHz dongle and the headset connected by USB cable.
pub const PRODUCT_IDS: &[u16] = &[0x0565, 0x056E];
const CONTROL_USAGE_PAGE: u16 = 0xFF14;

const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
/// Pause between writing a custom EQ curve and re-selecting the slot so the
/// DSP picks up the new values.
const EQ_SETTLE: Duration = Duration::from_millis(40);

#[derive(Debug)]
pub enum DeviceError {
    NotFound,
    Permission(String),
    Timeout { cmd: u8 },
    HeadsetOff,
    BadReply(String),
    Hid(hidapi::HidError),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "Razer BlackShark V2 HyperSpeed (1532:0565 / 1532:056E) not found. Is the dongle plugged in?"
            ),
            Self::Permission(path) => write!(
                f,
                "permission denied opening {path}. Run `razer-headset setup-udev` to install udev rules, or run as root"
            ),
            Self::Timeout { cmd } => write!(
                f,
                "no reply to command 0x{cmd:02X} within {REPLY_TIMEOUT:?}"
            ),
            Self::HeadsetOff => write!(
                f,
                "dongle found but the headset is powered off (no RF link)"
            ),
            Self::BadReply(s) => write!(f, "unexpected reply: {s}"),
            Self::Hid(e) => write!(f, "hid error: {e}"),
        }
    }
}

impl Error for DeviceError {}

impl From<hidapi::HidError> for DeviceError {
    fn from(e: hidapi::HidError) -> Self {
        Self::Hid(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqPreset {
    Flat,
    Game,
    Music,
    Movie,
    Custom,
}

impl EqPreset {
    pub fn raw(self) -> u8 {
        match self {
            Self::Flat => eq_preset::FLAT,
            Self::Game => eq_preset::GAME,
            Self::Music => eq_preset::MUSIC,
            Self::Movie => eq_preset::MOVIE,
            Self::Custom => eq_preset::CUSTOM,
        }
    }

    pub fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            eq_preset::FLAT => Self::Flat,
            eq_preset::GAME => Self::Game,
            eq_preset::MUSIC => Self::Music,
            eq_preset::MOVIE => Self::Movie,
            eq_preset::CUSTOM => Self::Custom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Game => "game",
            Self::Music => "music",
            Self::Movie => "movie",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DongleLed {
    Off,
    Link,
    Battery,
    Warning,
}

impl DongleLed {
    pub fn raw(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Link => 1,
            Self::Battery => 2,
            Self::Warning => 3,
        }
    }

    pub fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            0 => Self::Off,
            1 => Self::Link,
            2 => Self::Battery,
            3 => Self::Warning,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Link => "link",
            Self::Battery => "battery",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sidetone {
    pub enabled: bool,
    pub level: u8,
}

pub struct Headset {
    dev: HidDevice,
    counter: u8,
}

impl Headset {
    /// Find and open the control interface of the dongle.
    pub fn open() -> Result<Self, DeviceError> {
        let api = HidApi::new()?;
        let mut candidates: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == RAZER_VID && PRODUCT_IDS.contains(&d.product_id()))
            .collect();
        if candidates.is_empty() {
            return Err(DeviceError::NotFound);
        }
        // Prefer the collection that carries the vendor control channel. On
        // Linux hidraw one node covers every collection, so this mostly
        // matters on other backends.
        candidates.sort_by_key(|d| {
            if d.usage_page() == CONTROL_USAGE_PAGE {
                0
            } else {
                1
            }
        });

        let mut last_err = None;
        for info in candidates {
            match info.open_device(&api) {
                Ok(dev) => return Ok(Self { dev, counter: 0 }),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Permission denied") {
                        return Err(DeviceError::Permission(
                            info.path().to_string_lossy().into_owned(),
                        ));
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .map(DeviceError::Hid)
            .unwrap_or(DeviceError::NotFound))
    }

    fn next_seq(&mut self) -> u8 {
        let seq = protocol::sequence_byte(self.counter);
        self.counter = self.counter.wrapping_add(1);
        seq
    }

    /// Send one request and wait for its matching reply, discarding frames
    /// that answer somebody else's request.
    pub fn request(&mut self, domain: u8, cmd: u8, params: &[u8]) -> Result<Response, DeviceError> {
        let seq = self.next_seq();
        let frame = protocol::encode_request(seq, domain, cmd, params);
        self.dev.write(&frame)?;

        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut buf = [0u8; REPORT_LEN];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(DeviceError::Timeout { cmd });
            }
            let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
            let n = self.dev.read_timeout(&mut buf, timeout_ms)?;
            if n == 0 {
                continue;
            }
            match protocol::decode_response(&buf[..n]) {
                Ok(resp) if resp.answers(seq, cmd) => return Ok(resp),
                Ok(_) => continue, // not ours
                Err(protocol::DecodeError::WrongReportId(_)) => continue, // other collection
                Err(e) => return Err(DeviceError::BadReply(e.to_string())),
            }
        }
    }

    fn get_byte(&mut self, domain: u8, cmd: u8) -> Result<u8, DeviceError> {
        let resp = self.request(domain, cmd, &[])?;
        resp.payload
            .first()
            .copied()
            .ok_or_else(|| DeviceError::BadReply(format!("empty payload for 0x{cmd:02X}")))
    }

    /// SET `cmd` (a GET register number) with the given parameters.
    fn set(&mut self, domain: u8, cmd: u8, params: &[u8]) -> Result<(), DeviceError> {
        let set_cmd = reg::set(cmd);
        let resp = self.request(domain, set_cmd, params)?;
        if resp.status != 0x01 {
            return Err(DeviceError::BadReply(format!(
                "status 0x{:02X} for SET 0x{set_cmd:02X}",
                resp.status
            )));
        }
        Ok(())
    }

    fn headset_get(&mut self, cmd: u8) -> Result<u8, DeviceError> {
        self.get_byte(DOMAIN_HEADSET, cmd)
    }

    fn headset_get_bool(&mut self, cmd: u8) -> Result<bool, DeviceError> {
        Ok(self.headset_get(cmd)? != 0)
    }

    fn headset_set(&mut self, cmd: u8, value: u8) -> Result<(), DeviceError> {
        self.set(DOMAIN_HEADSET, cmd, &[value])
    }

    // ----- link / identity ---------------------------------------------------

    /// Is the headset currently linked to the dongle over 2.4 GHz?
    pub fn link_up(&mut self) -> Result<bool, DeviceError> {
        Ok(self.get_byte(DOMAIN_DONGLE, reg::LINK_STATUS)? != 0)
    }

    /// Fail fast with a clear message when the headset is switched off.
    pub fn require_link(&mut self) -> Result<(), DeviceError> {
        if self.link_up()? {
            Ok(())
        } else {
            Err(DeviceError::HeadsetOff)
        }
    }

    pub fn firmware(&mut self) -> Result<String, DeviceError> {
        let resp = self.request(DOMAIN_HEADSET, reg::FIRMWARE, &[])?;
        protocol::format_firmware(&resp.payload)
            .ok_or_else(|| DeviceError::BadReply(format!("firmware payload {:02X?}", resp.payload)))
    }

    pub fn serial(&mut self) -> Result<String, DeviceError> {
        let resp = self.request(DOMAIN_HEADSET, reg::SERIAL, &[])?;
        Ok(resp
            .payload
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect())
    }

    // ----- power -------------------------------------------------------------

    pub fn battery(&mut self) -> Result<Battery, DeviceError> {
        let percent = self.headset_get(reg::BATTERY)?.min(100);
        let charging = self.headset_get_bool(reg::CHARGING)?;
        Ok(Battery { percent, charging })
    }

    /// Auto power-off delay in minutes; 0 means never.
    pub fn auto_power_off(&mut self) -> Result<u8, DeviceError> {
        self.headset_get(reg::AUTO_POWER_OFF)
    }

    /// Set the auto power-off delay in minutes (0 = never, max 255).
    pub fn set_auto_power_off(&mut self, minutes: u8) -> Result<(), DeviceError> {
        self.headset_set(reg::AUTO_POWER_OFF, minutes)
    }

    // ----- equalizer -----------------------------------------------------------

    /// Active preset, or the raw register value if it is not a known preset.
    pub fn eq_preset(&mut self) -> Result<Result<EqPreset, u8>, DeviceError> {
        let raw = self.headset_get(reg::EQ_PRESET)?;
        Ok(EqPreset::from_raw(raw).ok_or(raw))
    }

    /// Select a preset and make sure the EQ stage is enabled.
    pub fn set_eq_preset(&mut self, preset: EqPreset) -> Result<(), DeviceError> {
        self.headset_set(reg::EQ_PRESET, preset.raw())?;
        self.set_eq_enabled(true)
    }

    pub fn eq_enabled(&mut self) -> Result<bool, DeviceError> {
        self.headset_get_bool(reg::EQ_ENABLE)
    }

    pub fn set_eq_enabled(&mut self, on: bool) -> Result<(), DeviceError> {
        self.headset_set(reg::EQ_ENABLE, u8::from(on))
    }

    /// The curve stored in the Custom slot, in dB. Always the custom slot,
    /// whatever preset is active; the factory curves cannot be read.
    pub fn custom_eq(&mut self) -> Result<[i8; EQ_BANDS], DeviceError> {
        let resp = self.request(DOMAIN_HEADSET, reg::EQ_BANDS, &[])?;
        protocol::eq_from_payload(&resp.payload).ok_or_else(|| {
            DeviceError::BadReply(format!("EQ reply has {} bytes", resp.payload.len()))
        })
    }

    /// Write `bands` (dB) into the Custom slot and make it the live curve.
    ///
    /// Order matters: enable the EQ stage, select the slot, write the curve,
    /// then re-select the slot so the DSP picks up the new values.
    pub fn set_custom_eq(&mut self, bands: &[i8; EQ_BANDS]) -> Result<(), DeviceError> {
        let wire = protocol::eq_to_wire(bands);
        self.set_eq_enabled(true)?;
        self.headset_set(reg::EQ_PRESET, EqPreset::Custom.raw())?;
        self.set(DOMAIN_HEADSET, reg::EQ_BANDS, &wire)?;
        thread::sleep(EQ_SETTLE);
        self.headset_set(reg::EQ_PRESET, EqPreset::Custom.raw())
    }

    /// "Audio enhancement" (bass / spatial expander in the DSP).
    pub fn enhancement(&mut self) -> Result<bool, DeviceError> {
        self.headset_get_bool(reg::ENHANCEMENT)
    }

    pub fn set_enhancement(&mut self, on: bool) -> Result<(), DeviceError> {
        self.headset_set(reg::ENHANCEMENT, u8::from(on))
    }

    // ----- microphone ------------------------------------------------------------

    pub fn sidetone(&mut self) -> Result<Sidetone, DeviceError> {
        let enabled = self.headset_get_bool(reg::SIDETONE)?;
        let level = self.headset_get(reg::SIDETONE_LEVEL)?.min(15);
        Ok(Sidetone { enabled, level })
    }

    /// Set sidetone level 0..=15 and turn the stage on; `None` turns it off.
    pub fn set_sidetone(&mut self, level: Option<u8>) -> Result<(), DeviceError> {
        match level {
            Some(level) => {
                self.headset_set(reg::SIDETONE_LEVEL, level.min(15))?;
                self.headset_set(reg::SIDETONE, 1)
            }
            None => self.headset_set(reg::SIDETONE, 0),
        }
    }

    /// Position of the physical mic mute switch (read-only).
    pub fn mic_muted(&mut self) -> Result<bool, DeviceError> {
        self.headset_get_bool(reg::MIC_MUTE)
    }

    // ----- misc ----------------------------------------------------------------------

    /// Block incoming Bluetooth calls while connected to the dongle.
    pub fn dnd(&mut self) -> Result<bool, DeviceError> {
        self.headset_get_bool(reg::BT_DND)
    }

    pub fn set_dnd(&mut self, on: bool) -> Result<(), DeviceError> {
        self.headset_set(reg::BT_DND, u8::from(on))
    }

    pub fn dongle_led(&mut self) -> Result<Result<DongleLed, u8>, DeviceError> {
        let raw = self.get_byte(DOMAIN_DONGLE, reg::DONGLE_LED)?;
        Ok(DongleLed::from_raw(raw).ok_or(raw))
    }

    pub fn set_dongle_led(&mut self, mode: DongleLed) -> Result<(), DeviceError> {
        self.set(DOMAIN_DONGLE, reg::DONGLE_LED, &[mode.raw()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_preset_roundtrips() {
        for preset in [
            EqPreset::Flat,
            EqPreset::Game,
            EqPreset::Music,
            EqPreset::Movie,
            EqPreset::Custom,
        ] {
            assert_eq!(EqPreset::from_raw(preset.raw()), Some(preset));
            assert!(!preset.name().is_empty());
        }
        assert_eq!(EqPreset::from_raw(0x42), None);
    }

    #[test]
    fn dongle_led_roundtrips() {
        for mode in [
            DongleLed::Off,
            DongleLed::Link,
            DongleLed::Battery,
            DongleLed::Warning,
        ] {
            assert_eq!(DongleLed::from_raw(mode.raw()), Some(mode));
            assert!(!mode.name().is_empty());
        }
        assert_eq!(DongleLed::from_raw(4), None);
    }
}
