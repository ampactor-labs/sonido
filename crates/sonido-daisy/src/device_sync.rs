//! UART-based sync protocol between two Daisy Seeds.
//!
//! The master Seed transmits a [`SyncFrame`] at 100 Hz over UART.  The slave
//! receives and mirrors the state, enabling:
//!
//! - **Stereo rigs**: two independent effect chains locked to the same morph
//!   position and tempo.
//! - **Dual-mono**: different effects on L and R with perfect phase alignment.
//! - **Synchronized morph**: both boards follow the same morph sweep curve.
//!
//! # Wire Protocol
//!
//! Each frame is 12 bytes, transmitted at 115 200 baud (default).  The
//! receiver validates the XOR checksum before applying state.  Corrupted
//! frames are silently discarded.
//!
//! | Offset | Size | Field        | Description                          |
//! |--------|------|--------------|--------------------------------------|
//! | 0      | 1    | `header`     | Always `0xAA`                        |
//! | 1–4    | 4    | `morph_t`    | Morph position [0.0, 1.0] (f32 LE)  |
//! | 5–8    | 4    | `tempo_bpm`  | BPM (f32 LE)                         |
//! | 9      | 1    | `preset_idx` | Active preset index [0, 7]           |
//! | 10     | 1    | `flags`      | Bit 0: bypass; Bit 1: tap active     |
//! | 11     | 1    | `checksum`   | XOR of bytes [0..11]                 |
//!
//! # Status
//!
//! Frame layout and (de)serialization defined. UART-based device sync task
//! planned for hardware integration phase — see `audio.rs` for the pattern to follow.

/// Sync frame sent over UART at 100 Hz.
///
/// # Layout
///
/// `repr(C, packed)` ensures the on-wire layout matches the table in the
/// module-level doc exactly.  Do **not** add fields or reorder without bumping
/// the protocol version.
///
/// # Invariants
///
/// - `header` is always `0xAA`.
/// - `morph_t` is in [0.0, 1.0].
/// - `tempo_bpm` is in [40.0, 300.0].
/// - `preset_idx` is in [0, 7].
/// - `checksum` equals XOR of bytes [0..11] (all fields except checksum itself).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct SyncFrame {
    /// Start-of-frame marker.  Always `0xAA`.
    pub header: u8,
    /// Current morph knob position.
    ///
    /// Valid range: [0.0, 1.0].
    pub morph_t: f32,
    /// Current tempo in beats per minute.
    ///
    /// Valid range: [40.0, 300.0].
    pub tempo_bpm: f32,
    /// Active preset index.
    ///
    /// Valid range: [0, 7].
    pub preset_idx: u8,
    /// Status flags.
    ///
    /// - Bit 0: global bypass active.
    /// - Bit 1: tap-tempo event in progress.
    pub flags: u8,
    /// XOR checksum of bytes [0..11].
    pub checksum: u8,
}

impl SyncFrame {
    /// Construct and checksum a new [`SyncFrame`].
    ///
    /// # Arguments
    ///
    /// * `morph_t`    — Morph position [0.0, 1.0].
    /// * `tempo_bpm`  — BPM [40.0, 300.0].
    /// * `preset_idx` — Active preset [0, 7].
    /// * `flags`      — Status flags (bit 0: bypass; bit 1: tap active).
    pub fn new(morph_t: f32, tempo_bpm: f32, preset_idx: u8, flags: u8) -> Self {
        let mut frame = Self {
            header: 0xAA,
            morph_t,
            tempo_bpm,
            preset_idx,
            flags,
            checksum: 0,
        };
        let bytes = frame.to_bytes();
        // Checksum covers bytes 0..11 (everything before the checksum field).
        frame.checksum = bytes[..11].iter().fold(0u8, |acc, &b| acc ^ b);
        frame
    }

    /// Serialize the frame to a 12-byte array for UART transmission.
    ///
    /// Byte order is little-endian for the f32 fields, matching the
    /// STM32H7's native byte order.
    pub fn to_bytes(&self) -> [u8; 12] {
        let mt = self.morph_t.to_le_bytes();
        let tb = self.tempo_bpm.to_le_bytes();
        [
            self.header,
            mt[0],
            mt[1],
            mt[2],
            mt[3],
            tb[0],
            tb[1],
            tb[2],
            tb[3],
            self.preset_idx,
            self.flags,
            self.checksum,
        ]
    }

    /// Deserialize a 12-byte UART payload into a [`SyncFrame`].
    ///
    /// Returns `None` if the header byte is not `0xAA` or the checksum fails.
    pub fn from_bytes(data: &[u8; 12]) -> Option<Self> {
        if data[0] != 0xAA {
            return None;
        }
        let frame = Self {
            header: data[0],
            morph_t: f32::from_le_bytes([data[1], data[2], data[3], data[4]]),
            tempo_bpm: f32::from_le_bytes([data[5], data[6], data[7], data[8]]),
            preset_idx: data[9],
            flags: data[10],
            checksum: data[11],
        };
        if frame.verify_checksum() {
            Some(frame)
        } else {
            None
        }
    }

    /// Verify that the checksum field matches the XOR of bytes [0..11].
    ///
    /// Returns `true` if the frame is intact.
    pub fn verify_checksum(&self) -> bool {
        let bytes = self.to_bytes();
        let computed = bytes[..11].iter().fold(0u8, |acc, &b| acc ^ b);
        computed == self.checksum
    }
}
