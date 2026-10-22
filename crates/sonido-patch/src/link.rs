//! The USB serial protocol between the pedal and the editor or CLI.
//!
//! The pedal enumerates as a USB serial port while it plays. The host sends one
//! request frame and reads one reply frame. Rigs travel as the same 4 KB sectors
//! the bank stores ([`crate::encode`]), so the pedal can check a rig (decode,
//! effects available, CPU budget) before writing it to flash.
//!
//! # Frame
//!
//! | Bytes | Field |
//! |-------|-------|
//! | 2 | sync `"SL"` |
//! | 1 | kind ([`Request`] or [`Reply`]) |
//! | 1 | argument (slot, rig, or error code) |
//! | 2 | payload length, little endian, at most [`MAX_PAYLOAD`] |
//! | n | payload |
//! | 4 | CRC32 of kind through payload, little endian |

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec, vec::Vec};

use crate::SECTOR_SIZE;
use crate::binary::crc32;
use crate::pedal::RigRef;

/// Frame sync bytes.
pub const SYNC: [u8; 2] = *b"SL";
/// Protocol version reported in [`Info`].
pub const PROTOCOL_VERSION: u8 = 1;
/// Largest payload: one rig sector.
pub const MAX_PAYLOAD: usize = SECTOR_SIZE;
/// Bytes around the payload (sync, kind, argument, length, CRC).
pub const FRAME_OVERHEAD: usize = 2 + 1 + 1 + 2 + 4;
/// Largest frame on the wire.
pub const MAX_FRAME: usize = FRAME_OVERHEAD + MAX_PAYLOAD;

/// Host → pedal requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Request {
    /// Reply [`Reply::Info`].
    Hello = 0x01,
    /// Read bank slot `arg`; reply [`Reply::Sector`].
    ReadSlot = 0x02,
    /// Check and write the payload (one sector) to bank slot `arg`, then play it.
    WriteSlot = 0x03,
    /// Erase bank slot `arg`.
    EraseSlot = 0x04,
    /// Play the rig whose [`RigRef`] byte is `arg`.
    Activate = 0x05,
    /// Reply the current jam as a rig sector.
    ReadJam = 0x06,
    /// Reply [`Reply::Status`].
    Status = 0x07,
}

impl Request {
    /// From the frame's kind byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => Self::Hello,
            0x02 => Self::ReadSlot,
            0x03 => Self::WriteSlot,
            0x04 => Self::EraseSlot,
            0x05 => Self::Activate,
            0x06 => Self::ReadJam,
            0x07 => Self::Status,
            _ => return None,
        })
    }
}

/// Pedal → host replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reply {
    /// Done.
    Ok = 0x80,
    /// Refused; `arg` is an [`ErrorCode`].
    Error = 0x81,
    /// Payload is an encoded [`Info`].
    Info = 0x82,
    /// Payload is one rig sector (erased slots reply all `0xFF`).
    Sector = 0x83,
    /// Payload is an encoded [`Status`].
    Status = 0x84,
}

impl Reply {
    /// From the frame's kind byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x80 => Self::Ok,
            0x81 => Self::Error,
            0x82 => Self::Info,
            0x83 => Self::Sector,
            0x84 => Self::Status,
            _ => return None,
        })
    }
}

/// Why the pedal refused a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    /// No such slot or rig.
    BadSlot = 1,
    /// The payload is not a rig sector.
    NotARig = 2,
    /// The rig uses an effect this firmware does not have.
    UnknownEffect = 3,
    /// The rig would not run in real time on the pedal.
    OverBudget = 4,
    /// Writing flash failed.
    Flash = 5,
    /// Unknown request.
    Unsupported = 6,
    /// The rig did not build (bad routing).
    BadRig = 7,
}

impl ErrorCode {
    /// From the frame's argument byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::BadSlot,
            2 => Self::NotARig,
            3 => Self::UnknownEffect,
            4 => Self::OverBudget,
            5 => Self::Flash,
            6 => Self::Unsupported,
            7 => Self::BadRig,
            _ => return None,
        })
    }

    /// Short explanation for people.
    pub fn describe(self) -> &'static str {
        match self {
            Self::BadSlot => "no such slot or rig",
            Self::NotARig => "not a rig",
            Self::UnknownEffect => "uses an effect this pedal firmware does not have",
            Self::OverBudget => "too heavy to run in real time on the pedal",
            Self::Flash => "the pedal could not write its flash",
            Self::Unsupported => "the pedal does not know this request (update its firmware)",
            Self::BadRig => "the rig's routing does not build",
        }
    }
}

/// A decoded frame, borrowing its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Kind byte ([`Request`] or [`Reply`]).
    pub kind: u8,
    /// Argument byte.
    pub arg: u8,
    /// Payload.
    pub payload: &'a [u8],
}

/// Write one frame into `out`, returning its length, or `None` if the payload
/// is too large or `out` too small.
pub fn encode_frame(kind: u8, arg: u8, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    let total = FRAME_OVERHEAD + payload.len();
    if payload.len() > MAX_PAYLOAD || out.len() < total {
        return None;
    }
    out[..2].copy_from_slice(&SYNC);
    out[2] = kind;
    out[3] = arg;
    out[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[6..6 + payload.len()].copy_from_slice(payload);
    let crc = crc32(&out[2..6 + payload.len()]);
    out[6 + payload.len()..total].copy_from_slice(&crc.to_le_bytes());
    Some(total)
}

/// Encode a frame into a new vector.
pub fn frame(kind: u8, arg: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0; FRAME_OVERHEAD + payload.len()];
    let n = encode_frame(kind, arg, payload, &mut out).unwrap_or(0);
    out.truncate(n);
    out
}

/// Incremental frame decoder: feed bytes as they arrive, in any chunking.
///
/// Bytes before a sync and frames with a bad length or CRC are dropped, and
/// decoding resumes at the next sync. Holds one [`MAX_FRAME`] buffer.
pub struct FrameDecoder {
    buf: [u8; MAX_FRAME],
    len: usize,
    dropped: u32,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// An empty decoder.
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_FRAME],
            len: 0,
            dropped: 0,
        }
    }

    /// Frames dropped so far (bad length or CRC).
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Feed `bytes`, calling `on_frame` for each complete, valid frame.
    pub fn feed(&mut self, bytes: &[u8], mut on_frame: impl FnMut(Frame<'_>)) {
        for &b in bytes {
            self.push(b, &mut on_frame);
        }
    }

    fn push(&mut self, b: u8, on_frame: &mut impl FnMut(Frame<'_>)) {
        // Hunt for the sync bytes.
        if self.len < 2 {
            if b == SYNC[self.len] {
                self.buf[self.len] = b;
                self.len += 1;
            } else {
                self.len = usize::from(b == SYNC[0]);
                if self.len == 1 {
                    self.buf[0] = b;
                }
            }
            return;
        }
        self.buf[self.len] = b;
        self.len += 1;
        if self.len < 6 {
            return;
        }
        let payload_len = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
        if payload_len > MAX_PAYLOAD {
            self.dropped += 1;
            self.len = 0;
            return;
        }
        let total = FRAME_OVERHEAD + payload_len;
        if self.len < total {
            return;
        }
        let crc_at = 6 + payload_len;
        let stored = u32::from_le_bytes([
            self.buf[crc_at],
            self.buf[crc_at + 1],
            self.buf[crc_at + 2],
            self.buf[crc_at + 3],
        ]);
        if stored == crc32(&self.buf[2..crc_at]) {
            on_frame(Frame {
                kind: self.buf[2],
                arg: self.buf[3],
                payload: &self.buf[6..crc_at],
            });
        } else {
            self.dropped += 1;
        }
        self.len = 0;
    }
}

/// What the pedal reports about itself ([`Reply::Info`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// [`PROTOCOL_VERSION`] of the firmware.
    pub protocol: u8,
    /// Bank slots.
    pub slot_count: u8,
    /// Built-in factory rigs.
    pub factory_count: u8,
    /// The rig playing now.
    pub active: RigRef,
    /// Effect UIDs this firmware can run.
    pub effects: Vec<u16>,
    /// Firmware name and version, for people.
    pub firmware: String,
}

impl Info {
    /// Encode as a [`Reply::Info`] payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![
            self.protocol,
            self.slot_count,
            self.factory_count,
            self.active.to_byte(),
            self.effects.len().min(255) as u8,
        ];
        for uid in self.effects.iter().take(255) {
            out.extend_from_slice(&uid.to_le_bytes());
        }
        let name = self.firmware.as_bytes();
        let name = &name[..name.len().min(255)];
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        out
    }

    /// Decode a [`Reply::Info`] payload.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (&protocol, rest) = payload.split_first()?;
        let (&slot_count, rest) = rest.split_first()?;
        let (&factory_count, rest) = rest.split_first()?;
        let (&active, rest) = rest.split_first()?;
        let (&n, rest) = rest.split_first()?;
        let (uids, rest) = rest.split_at_checked(n as usize * 2)?;
        let effects = uids
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        let (&name_len, rest) = rest.split_first()?;
        let name = rest.get(..name_len as usize)?;
        Some(Self {
            protocol,
            slot_count,
            factory_count,
            active: RigRef::from_byte(active),
            effects,
            firmware: String::from_utf8_lossy(name).into(),
        })
    }
}

/// What the pedal is doing ([`Reply::Status`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The rig playing now.
    pub active: RigRef,
    /// Whole-pedal bypass.
    pub bypassed: bool,
    /// A/B morph position, 0–255.
    pub morph: u8,
    /// Last second's mean CPU load, in tenths of a percent of the block budget.
    pub load_mean: u16,
    /// Last second's slowest callback, in tenths of a percent.
    pub load_peak: u16,
}

impl Status {
    /// Encode as a [`Reply::Status`] payload.
    pub fn encode(&self) -> [u8; 7] {
        let m = self.load_mean.to_le_bytes();
        let p = self.load_peak.to_le_bytes();
        [
            self.active.to_byte(),
            u8::from(self.bypassed),
            self.morph,
            m[0],
            m[1],
            p[0],
            p[1],
        ]
    }

    /// Decode a [`Reply::Status`] payload.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let b: &[u8; 7] = payload.get(..7)?.try_into().ok()?;
        Some(Self {
            active: RigRef::from_byte(b[0]),
            bypassed: b[1] != 0,
            morph: b[2],
            load_mean: u16::from_le_bytes([b[3], b[4]]),
            load_peak: u16::from_le_bytes([b[5], b[6]]),
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> (Vec<(u8, u8, Vec<u8>)>, u32) {
        let mut d = FrameDecoder::new();
        let mut frames = Vec::new();
        d.feed(bytes, |f| frames.push((f.kind, f.arg, f.payload.to_vec())));
        (frames, d.dropped())
    }

    #[test]
    fn frames_round_trip_in_any_chunking() {
        let payload: Vec<u8> = (0..MAX_PAYLOAD).map(|i| (i * 7) as u8).collect();
        let wire = frame(Request::WriteSlot as u8, 3, &payload);
        assert_eq!(wire.len(), MAX_FRAME);
        for chunk in [1, 7, 64, MAX_FRAME] {
            let mut d = FrameDecoder::new();
            let mut got = None;
            for piece in wire.chunks(chunk) {
                d.feed(piece, |f| got = Some((f.kind, f.arg, f.payload.to_vec())));
            }
            assert_eq!(got, Some((Request::WriteSlot as u8, 3, payload.clone())));
        }
    }

    #[test]
    fn garbage_and_bad_crc_are_skipped() {
        let mut wire = b"noise S".to_vec();
        let mut bad = frame(Request::Hello as u8, 0, b"x");
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        wire.extend_from_slice(&bad);
        wire.extend_from_slice(&frame(Request::Status as u8, 9, &[]));
        let (frames, dropped) = decode_all(&wire);
        assert_eq!(frames, vec![(Request::Status as u8, 9, vec![])]);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn oversize_length_resyncs() {
        let mut wire = vec![b'S', b'L', 1, 0, 0xFF, 0xFF];
        wire.extend_from_slice(&frame(Reply::Ok as u8, 0, &[]));
        let (frames, dropped) = decode_all(&wire);
        assert_eq!(frames, vec![(Reply::Ok as u8, 0, vec![])]);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn info_and_status_round_trip() {
        let info = Info {
            protocol: PROTOCOL_VERSION,
            slot_count: 15,
            factory_count: 3,
            active: RigRef::Bank(2),
            effects: vec![1, 6, 11],
            firmware: "sonido_pedal 0.2".into(),
        };
        assert_eq!(Info::decode(&info.encode()), Some(info));
        let status = Status {
            active: RigRef::Factory(1),
            bypassed: true,
            morph: 128,
            load_mean: 781,
            load_peak: 890,
        };
        assert_eq!(Status::decode(&status.encode()), Some(status));
    }

    #[test]
    fn kinds_and_codes_round_trip() {
        for r in [
            Request::Hello,
            Request::ReadSlot,
            Request::WriteSlot,
            Request::EraseSlot,
            Request::Activate,
            Request::ReadJam,
            Request::Status,
        ] {
            assert_eq!(Request::from_byte(r as u8), Some(r));
        }
        for c in 1..=7u8 {
            assert_eq!(ErrorCode::from_byte(c).map(|e| e as u8), Some(c));
        }
    }
}
