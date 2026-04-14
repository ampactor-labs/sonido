//! USB-MIDI event parser.
//!
//! Parses USB-MIDI 1.0 event packets (4 bytes each) into [`MidiEvent`]
//! structs. Supports Control Change (CC), Program Change, Note On/Off,
//! and MIDI Clock messages.
//!
//! The USB endpoint setup and async task are handled by the pedal
//! integration — this module only provides the stateless parser.
//!
//! # USB-MIDI Packet Format
//!
//! ```text
//! Byte 0: [Cable Number (4b)] [Code Index Number (4b)]
//! Byte 1: MIDI status byte
//! Byte 2: MIDI data byte 1
//! Byte 3: MIDI data byte 2
//! ```
//!
//! # Code Index Numbers
//!
//! | CIN | Event |
//! |-----|-------|
//! | 0x8 | Note Off |
//! | 0x9 | Note On |
//! | 0xB | Control Change |
//! | 0xC | Program Change |
//! | 0xF | Single Byte (Clock, Start, Stop) |

/// MIDI status bytes.
pub mod status {
    /// Note Off (0x80 | channel).
    pub const NOTE_OFF: u8 = 0x80;
    /// Note On (0x90 | channel).
    pub const NOTE_ON: u8 = 0x90;
    /// Control Change (0xB0 | channel).
    pub const CONTROL_CHANGE: u8 = 0xB0;
    /// Program Change (0xC0 | channel).
    pub const PROGRAM_CHANGE: u8 = 0xC0;
    /// MIDI Clock (system real-time).
    pub const CLOCK: u8 = 0xF8;
    /// MIDI Start (system real-time).
    pub const START: u8 = 0xFA;
    /// MIDI Stop (system real-time).
    pub const STOP: u8 = 0xFC;
}

/// A parsed MIDI event.
#[derive(Debug, Clone, Copy)]
pub struct MidiEvent {
    /// MIDI status byte (includes channel for channel messages).
    pub status: u8,
    /// First data byte (note number, CC number, program number).
    pub data1: u8,
    /// Second data byte (velocity, CC value). 0 for single-byte messages.
    pub data2: u8,
}

impl MidiEvent {
    /// Whether this is a Control Change message.
    #[inline]
    pub fn is_cc(&self) -> bool {
        self.status & 0xF0 == status::CONTROL_CHANGE
    }

    /// Whether this is a Program Change message.
    #[inline]
    pub fn is_program_change(&self) -> bool {
        self.status & 0xF0 == status::PROGRAM_CHANGE
    }

    /// Whether this is a MIDI Clock message.
    #[inline]
    pub fn is_clock(&self) -> bool {
        self.status == status::CLOCK
    }

    /// Whether this is a MIDI Start message.
    #[inline]
    pub fn is_start(&self) -> bool {
        self.status == status::START
    }

    /// Whether this is a MIDI Stop message.
    #[inline]
    pub fn is_stop(&self) -> bool {
        self.status == status::STOP
    }

    /// Whether this is a Note On message (velocity > 0).
    #[inline]
    pub fn is_note_on(&self) -> bool {
        self.status & 0xF0 == status::NOTE_ON && self.data2 > 0
    }

    /// Whether this is a Note Off message (or Note On with velocity 0).
    #[inline]
    pub fn is_note_off(&self) -> bool {
        self.status & 0xF0 == status::NOTE_OFF
            || (self.status & 0xF0 == status::NOTE_ON && self.data2 == 0)
    }

    /// MIDI channel (0–15) for channel messages.
    #[inline]
    pub fn channel(&self) -> u8 {
        self.status & 0x0F
    }

    /// CC number (for Control Change messages).
    #[inline]
    pub fn cc_number(&self) -> u8 {
        self.data1
    }

    /// CC value (for Control Change messages, 0–127).
    #[inline]
    pub fn cc_value(&self) -> u8 {
        self.data2
    }

    /// Program number (for Program Change messages, 0–127).
    #[inline]
    pub fn program_number(&self) -> u8 {
        self.data1
    }
}

/// Stateless USB-MIDI packet parser.
///
/// Parses 4-byte USB-MIDI event packets into [`MidiEvent`] structs.
/// Call [`parse_packet()`](Self::parse_packet) for each received packet.
pub struct MidiHandler;

impl MidiHandler {
    /// Creates a new MIDI handler.
    pub const fn new() -> Self {
        Self
    }

    /// Parse a 4-byte USB-MIDI event packet.
    ///
    /// Returns `Some(MidiEvent)` for recognized messages, `None` for
    /// unknown or malformed packets.
    ///
    /// # USB-MIDI Packet Format
    ///
    /// ```text
    /// packet[0]: Cable Number (high nibble) | Code Index Number (low nibble)
    /// packet[1]: MIDI status byte
    /// packet[2]: MIDI data byte 1
    /// packet[3]: MIDI data byte 2
    /// ```
    pub fn parse_packet(&self, packet: &[u8; 4]) -> Option<MidiEvent> {
        let cin = packet[0] & 0x0F;
        match cin {
            0x8 => Some(MidiEvent {
                status: packet[1],
                data1: packet[2],
                data2: packet[3],
            }),
            0x9 => Some(MidiEvent {
                status: packet[1],
                data1: packet[2],
                data2: packet[3],
            }),
            0xB => Some(MidiEvent {
                status: packet[1],
                data1: packet[2],
                data2: packet[3],
            }),
            0xC => Some(MidiEvent {
                status: packet[1],
                data1: packet[2],
                data2: 0,
            }),
            0xF => Some(MidiEvent {
                status: packet[1],
                data1: 0,
                data2: 0,
            }),
            _ => None,
        }
    }
}

impl Default for MidiHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> MidiHandler {
        MidiHandler::new()
    }

    #[test]
    fn note_off_parsed() {
        let h = handler();
        let ev = h.parse_packet(&[0x08, 0x80, 60, 0]).unwrap();
        assert_eq!(ev.status, 0x80);
        assert_eq!(ev.data1, 60);
        assert_eq!(ev.data2, 0);
        assert!(ev.is_note_off());
    }

    #[test]
    fn note_on_parsed() {
        let h = handler();
        let ev = h.parse_packet(&[0x09, 0x90, 64, 100]).unwrap();
        assert_eq!(ev.status, 0x90);
        assert_eq!(ev.data1, 64);
        assert_eq!(ev.data2, 100);
        assert!(ev.is_note_on());
    }

    #[test]
    fn cc_parsed() {
        let h = handler();
        let ev = h.parse_packet(&[0x0B, 0xB0, 7, 64]).unwrap();
        assert!(ev.is_cc());
        assert_eq!(ev.cc_number(), 7);
        assert_eq!(ev.cc_value(), 64);
    }

    #[test]
    fn program_change_parsed() {
        let h = handler();
        let ev = h.parse_packet(&[0x0C, 0xC0, 5, 0]).unwrap();
        assert!(ev.is_program_change());
        assert_eq!(ev.program_number(), 5);
        assert_eq!(ev.data2, 0);
    }

    #[test]
    fn clock_parsed() {
        let h = handler();
        let ev = h.parse_packet(&[0x0F, 0xF8, 0, 0]).unwrap();
        assert!(ev.is_clock());
        assert_eq!(ev.data1, 0);
        assert_eq!(ev.data2, 0);
    }

    #[test]
    fn unknown_cin_returns_none() {
        let h = handler();
        assert!(h.parse_packet(&[0x01, 0x00, 0x00, 0x00]).is_none());
        assert!(h.parse_packet(&[0x05, 0x00, 0x00, 0x00]).is_none());
    }

    #[test]
    fn cable_number_ignored() {
        let h = handler();
        // Cable number is high nibble — should be ignored for parsing
        let ev = h.parse_packet(&[0x1B, 0xB0, 10, 127]).unwrap(); // cable 1, CC
        assert!(ev.is_cc());
        assert_eq!(ev.cc_number(), 10);
        assert_eq!(ev.cc_value(), 127);
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        let h = handler();
        let ev = h.parse_packet(&[0x09, 0x90, 60, 0]).unwrap();
        assert!(ev.is_note_off());
        assert!(!ev.is_note_on());
    }

    #[test]
    fn channel_extracted_correctly() {
        let h = handler();
        let ev = h.parse_packet(&[0x0B, 0xB5, 1, 64]).unwrap(); // channel 5
        assert_eq!(ev.channel(), 5);
    }
}
