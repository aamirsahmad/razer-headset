//! Frame encoding/decoding for the MediaTek/Airoha control channel used by the
//! Razer BlackShark V2 HyperSpeed dongle (1532:0565).
//!
//! This is *not* the classic 90-byte Razer feature-report protocol. Every
//! exchange is a 64-byte report with report ID 0x02 on HID usage page 0xFF14,
//! written and read with plain write(2)/read(2) on the hidraw node.
//!
//! Layout (offsets include the report ID):
//!
//! ```text
//! [0]    0x02        report ID
//! [1]    0x00        request (reply: 0x02)
//! [2]    0x60|seq    client mask + 5-bit rolling sequence, echoed in the reply
//! [6]    len         4 + number of parameter bytes
//! [8]    dir         0x00 request, 0x80 reply
//! [9]    domain      0x80 relay to headset over RF, 0x00 dongle itself
//! [10]   cmd         register; SET register = GET register | 0x80
//! [11]   status      0x00 request, 0x01 reply
//! [12]   count       parameter / payload byte count
//! [13..] params / payload
//! [62]   checksum    request: XOR of [0..62); reply: XOR of [1..62)
//! [63]   0x00
//! ```
//!
//! Register map from the reverse-engineering work at
//! <https://github.com/justik13/razer-blackshark-v2-hyperspeed-webhid>, as
//! verified by <https://github.com/bbesli/Synapse-for-Linux>.

use std::fmt;

pub const REPORT_ID: u8 = 0x02;
pub const REPORT_LEN: usize = 64;
const MAX_PARAMS: usize = 48;

const SEQ_CLIENT_MASK: u8 = 0x60;
const SEQ_MASK: u8 = 0x1F;

pub const DOMAIN_HEADSET: u8 = 0x80;
pub const DOMAIN_DONGLE: u8 = 0x00;

const OFF_KIND: usize = 1;
const OFF_SEQ: usize = 2;
const OFF_LEN: usize = 6;
const OFF_DIR: usize = 8;
const OFF_DOMAIN: usize = 9;
const OFF_CMD: usize = 10;
const OFF_STATUS: usize = 11;
const OFF_COUNT: usize = 12;
const OFF_PARAMS: usize = 13;
const OFF_CHECKSUM: usize = 62;

/// GET register numbers. The SET variant of a register is `GET | 0x80`.
pub mod reg {
    pub const SERIAL: u8 = 0x00;
    pub const FIRMWARE: u8 = 0x02;
    pub const EQ_PRESET: u8 = 0x13;
    pub const EQ_BANDS: u8 = 0x15; // custom slot, 10 x int8 dB
    pub const SIDETONE: u8 = 0x18;
    pub const SIDETONE_LEVEL: u8 = 0x19; // 0..=15
    pub const ENHANCEMENT: u8 = 0x1D; // bass / spatial expander
    pub const EQ_ENABLE: u8 = 0x1E;
    pub const LINK_STATUS: u8 = 0x20; // dongle domain
    pub const BATTERY: u8 = 0x21;
    pub const BT_DND: u8 = 0x27; // block Bluetooth calls while on 2.4 GHz
    pub const CHARGING: u8 = 0x2A;
    pub const AUTO_POWER_OFF: u8 = 0x2C;
    pub const MIC_MUTE: u8 = 0x55; // read-only
    pub const DONGLE_LED: u8 = 0x66; // dongle domain

    pub const fn set(get: u8) -> u8 {
        get | 0x80
    }
}

/// Raw values of the EQ preset register.
pub mod eq_preset {
    pub const FLAT: u8 = 0x00;
    pub const GAME: u8 = 0x07;
    pub const MUSIC: u8 = 0x08;
    pub const MOVIE: u8 = 0x09;
    pub const CUSTOM: u8 = 0xFF;
}

pub const EQ_BANDS: usize = 10;
pub const EQ_BAND_HZ: [u16; EQ_BANDS] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
pub const EQ_MIN_DB: i8 = -9;
pub const EQ_MAX_DB: i8 = 6;
/// The DSP stores `wire - 5`, so gains are written biased by +5.
const EQ_WIRE_OFFSET: i8 = 5;

/// Convert gains in dB to the parameter bytes of SET 0x95.
pub fn eq_to_wire(bands: &[i8; EQ_BANDS]) -> [u8; EQ_BANDS] {
    bands.map(|db| (db.clamp(EQ_MIN_DB, EQ_MAX_DB) + EQ_WIRE_OFFSET) as u8)
}

/// Convert the payload of GET 0x15 (already in dB) to gains.
pub fn eq_from_payload(payload: &[u8]) -> Option<[i8; EQ_BANDS]> {
    let bytes: &[u8; EQ_BANDS] = payload.get(..EQ_BANDS)?.try_into().ok()?;
    Some(bytes.map(|b| (b as i8).clamp(EQ_MIN_DB, EQ_MAX_DB)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub seq: u8,
    pub domain: u8,
    pub cmd: u8,
    pub status: u8,
    pub payload: Vec<u8>,
}

impl Response {
    /// Does this frame answer a request sent with this sequence byte and command?
    pub fn answers(&self, seq: u8, cmd: u8) -> bool {
        self.cmd == cmd && (self.seq & SEQ_MASK) == (seq & SEQ_MASK)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort(usize),
    WrongReportId(u8),
    NotAReply,
    BadCount(u8),
    Checksum { expected: u8, found: u8 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "frame too short ({n} bytes)"),
            Self::WrongReportId(id) => write!(f, "unexpected report ID 0x{id:02X}"),
            Self::NotAReply => write!(f, "frame is not a reply"),
            Self::BadCount(c) => write!(f, "payload count {c} exceeds frame"),
            Self::Checksum { expected, found } => {
                write!(
                    f,
                    "checksum mismatch (expected 0x{expected:02X}, found 0x{found:02X})"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, b| acc ^ b)
}

/// Turn a rolling counter into the sequence byte sent on the wire.
pub fn sequence_byte(counter: u8) -> u8 {
    SEQ_CLIENT_MASK | (counter & SEQ_MASK)
}

/// Build a host -> device frame.
pub fn encode_request(seq: u8, domain: u8, cmd: u8, params: &[u8]) -> [u8; REPORT_LEN] {
    assert!(params.len() <= MAX_PARAMS, "too many parameters");
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[OFF_KIND] = 0x00;
    buf[OFF_SEQ] = seq;
    buf[OFF_LEN] = (4 + params.len()) as u8;
    buf[OFF_DIR] = 0x00;
    buf[OFF_DOMAIN] = domain;
    buf[OFF_CMD] = cmd;
    buf[OFF_STATUS] = 0x00;
    buf[OFF_COUNT] = params.len() as u8;
    buf[OFF_PARAMS..OFF_PARAMS + params.len()].copy_from_slice(params);
    buf[OFF_CHECKSUM] = xor(&buf[..OFF_CHECKSUM]);
    buf
}

/// Parse a device -> host frame.
pub fn decode_response(report: &[u8]) -> Result<Response, DecodeError> {
    if report.len() < REPORT_LEN {
        return Err(DecodeError::TooShort(report.len()));
    }
    if report[0] != REPORT_ID {
        return Err(DecodeError::WrongReportId(report[0]));
    }
    if report[OFF_DIR] != 0x80 {
        return Err(DecodeError::NotAReply);
    }
    let count = report[OFF_COUNT] as usize;
    if OFF_PARAMS + count > OFF_CHECKSUM {
        return Err(DecodeError::BadCount(report[OFF_COUNT]));
    }
    // Replies are checksummed without the report ID; accept the request-style
    // variant too in case a firmware revision includes it.
    let found = report[OFF_CHECKSUM];
    let expected = xor(&report[1..OFF_CHECKSUM]);
    if found != expected && found != xor(&report[..OFF_CHECKSUM]) {
        return Err(DecodeError::Checksum { expected, found });
    }
    Ok(Response {
        seq: report[OFF_SEQ],
        domain: report[OFF_DOMAIN],
        cmd: report[OFF_CMD],
        status: report[OFF_STATUS],
        payload: report[OFF_PARAMS..OFF_PARAMS + count].to_vec(),
    })
}

/// Format the 4-byte firmware payload as `major.minor.build.rev`.
pub fn format_firmware(payload: &[u8]) -> Option<String> {
    match payload {
        [a, b, c, d, ..] => Some(format!("{a}.{b}.{c}.{d}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_response(
        seq: u8,
        domain: u8,
        cmd: u8,
        status: u8,
        payload: &[u8],
    ) -> [u8; REPORT_LEN] {
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[OFF_KIND] = 0x02;
        buf[OFF_SEQ] = seq;
        buf[OFF_LEN] = (4 + payload.len()) as u8;
        buf[OFF_DIR] = 0x80;
        buf[OFF_DOMAIN] = domain;
        buf[OFF_CMD] = cmd;
        buf[OFF_STATUS] = status;
        buf[OFF_COUNT] = payload.len() as u8;
        buf[OFF_PARAMS..OFF_PARAMS + payload.len()].copy_from_slice(payload);
        buf[OFF_CHECKSUM] = xor(&buf[1..OFF_CHECKSUM]);
        buf
    }

    #[test]
    fn battery_query_layout() {
        let frame = encode_request(sequence_byte(0), DOMAIN_HEADSET, reg::BATTERY, &[]);
        let mut expected = [0u8; REPORT_LEN];
        expected[0] = 0x02;
        expected[2] = 0x60;
        expected[6] = 0x04;
        expected[9] = 0x80;
        expected[10] = 0x21;
        expected[62] = 0xC7;
        assert_eq!(frame, expected);
    }

    #[test]
    fn set_auto_power_off_layout() {
        let frame = encode_request(
            sequence_byte(0),
            DOMAIN_HEADSET,
            reg::set(reg::AUTO_POWER_OFF),
            &[30],
        );
        assert_eq!(frame[6], 0x05);
        assert_eq!(frame[10], 0xAC);
        assert_eq!(frame[12], 1);
        assert_eq!(frame[13], 30);
        assert_eq!(frame[62], 0x54);
    }

    #[test]
    fn checksum_matches_reference_formula() {
        // Reference implementation checksums the 63-byte payload (without
        // report ID) as 0x02 ^ p[0..61].
        let frame = encode_request(sequence_byte(5), DOMAIN_HEADSET, reg::BATTERY, &[]);
        let payload = &frame[1..];
        let reference = payload[..61].iter().fold(0x02u8, |a, b| a ^ b);
        assert_eq!(payload[61], reference);
        assert_eq!(frame[62], 0x02 ^ 0x65 ^ 0x04 ^ 0x80 ^ 0x21);
    }

    #[test]
    fn sequence_wraps_at_five_bits() {
        assert_eq!(sequence_byte(0), 0x60);
        assert_eq!(sequence_byte(31), 0x7F);
        assert_eq!(sequence_byte(32), 0x60);
    }

    #[test]
    fn response_roundtrip() {
        let frame = encode_response(0x6A, DOMAIN_HEADSET, reg::BATTERY, 1, &[85]);
        let resp = decode_response(&frame).unwrap();
        assert_eq!(resp.payload, vec![85]);
        assert_eq!(resp.status, 1);
        assert!(resp.answers(sequence_byte(10), reg::BATTERY));
        assert!(!resp.answers(sequence_byte(11), reg::BATTERY));
        assert!(!resp.answers(sequence_byte(10), reg::CHARGING));
    }

    #[test]
    fn response_accepts_request_style_checksum() {
        let mut frame = encode_response(0x60, DOMAIN_HEADSET, reg::AUTO_POWER_OFF, 1, &[15]);
        frame[OFF_CHECKSUM] = xor(&frame[..OFF_CHECKSUM]);
        assert_eq!(decode_response(&frame).unwrap().payload, vec![15]);
    }

    #[test]
    fn rejects_malformed_frames() {
        let good = encode_response(0x60, DOMAIN_HEADSET, reg::BATTERY, 1, &[50]);

        let mut bad_crc = good;
        bad_crc[13] ^= 1;
        assert!(matches!(
            decode_response(&bad_crc),
            Err(DecodeError::Checksum { .. })
        ));

        let mut not_reply = good;
        not_reply[OFF_DIR] = 0x00;
        assert_eq!(decode_response(&not_reply), Err(DecodeError::NotAReply));

        let mut wrong_id = good;
        wrong_id[0] = 0x07;
        assert_eq!(
            decode_response(&wrong_id),
            Err(DecodeError::WrongReportId(0x07))
        );

        assert_eq!(decode_response(&good[..10]), Err(DecodeError::TooShort(10)));
    }

    #[test]
    fn eq_wire_encoding_matches_hardware_capture() {
        // Verified on hardware: writing these gains sends 06 07 08 09 0a 0b 04 03 02 fc
        // and reads back 01 02 03 04 05 06 ff fe fd f7.
        let gains = [1, 2, 3, 4, 5, 6, -1, -2, -3, -9];
        assert_eq!(
            eq_to_wire(&gains),
            [0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x04, 0x03, 0x02, 0xfc]
        );
        let readback = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0xff, 0xfe, 0xfd, 0xf7];
        assert_eq!(eq_from_payload(&readback), Some(gains));
        assert_eq!(eq_from_payload(&readback[..9]), None);
    }

    #[test]
    fn eq_gains_are_clamped() {
        assert_eq!(eq_to_wire(&[20; EQ_BANDS])[0], (EQ_MAX_DB + 5) as u8);
        assert_eq!(eq_to_wire(&[-20; EQ_BANDS])[0], (EQ_MIN_DB + 5) as u8);
        // A Synapse-written "flat" curve of raw zeros reads as -5 dB; still in range.
        assert_eq!(eq_from_payload(&[0xFB; EQ_BANDS]), Some([-5; EQ_BANDS]));
    }

    #[test]
    fn firmware_formatting() {
        assert_eq!(format_firmware(&[1, 7, 0, 0]).as_deref(), Some("1.7.0.0"));
        assert_eq!(format_firmware(&[1, 7]), None);
    }

    #[test]
    fn rejects_payload_count_overflow() {
        let mut frame = encode_response(0x60, DOMAIN_HEADSET, reg::BATTERY, 1, &[50]);
        frame[OFF_COUNT] = 50; // 13 + 50 = 63 > OFF_CHECKSUM (62)
        frame[OFF_CHECKSUM] = xor(&frame[1..OFF_CHECKSUM]);
        assert_eq!(decode_response(&frame), Err(DecodeError::BadCount(50)));
    }

    #[test]
    fn encode_request_handles_max_params() {
        let params = [0xAAu8; MAX_PARAMS];
        let frame = encode_request(sequence_byte(1), DOMAIN_HEADSET, reg::EQ_BANDS, &params);
        assert_eq!(frame[OFF_COUNT] as usize, MAX_PARAMS);
        assert_eq!(frame[OFF_LEN] as usize, 4 + MAX_PARAMS);
        assert_eq!(&frame[OFF_PARAMS..OFF_PARAMS + MAX_PARAMS], &params);
        assert_eq!(frame[OFF_CHECKSUM], xor(&frame[..OFF_CHECKSUM]));
    }

    #[test]
    #[should_panic(expected = "too many parameters")]
    fn encode_request_panics_above_max_params() {
        let params = [0u8; MAX_PARAMS + 1];
        let _ = encode_request(sequence_byte(0), DOMAIN_HEADSET, reg::EQ_BANDS, &params);
    }
}
