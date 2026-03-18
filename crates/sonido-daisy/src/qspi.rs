//! QSPI NOR flash preset storage for the Daisy Seed.
//!
//! Provides read/write access to the W25Q64JV 8 MB QSPI flash for
//! persistent preset storage. Uses the last 4 KB sector to avoid the
//! firmware region (BOOT_SRAM mode loads firmware from the start of flash).
//!
//! # Flash Layout
//!
//! ```text
//! 0x0000_0000 ─ 0x007F_EFFF : Firmware + free space (~8 MB - 4 KB)
//! 0x007F_F000 ─ 0x007F_FFFF : Preset sector (4 KB)
//!   ├── PresetHeader (8 bytes)
//!   ├── PresetSlot[0] (400 bytes)
//!   ├── PresetSlot[1] (400 bytes)
//!   ├── ...
//!   └── PresetSlot[7] (400 bytes)
//! ```
//!
//! # W25Q64JV Commands
//!
//! | Command | Opcode | Description |
//! |---------|--------|-------------|
//! | Read JEDEC ID | 0x9F | Verify chip presence |
//! | Read Data | 0x03 | Read bytes (any length) |
//! | Write Enable | 0x06 | Required before program/erase |
//! | Page Program | 0x02 | Write up to 256 bytes |
//! | Sector Erase | 0x20 | Erase 4 KB sector |
//! | Read Status | 0x05 | Poll busy flag (bit 0) |

/// Preset storage sector address (last 4 KB of 8 MB flash).
pub const PRESET_SECTOR_ADDR: u32 = 0x007F_F000;

/// Size of the preset sector in bytes.
pub const PRESET_SECTOR_SIZE: usize = 4096;

/// Maximum number of user preset slots.
pub const MAX_USER_PRESETS: usize = 8;

/// Magic number identifying a valid preset header ("SOND").
pub const PRESET_MAGIC: u32 = 0x534F_4E44;

/// Current preset format version.
pub const PRESET_VERSION: u8 = 1;

/// Maximum parameters per effect slot.
pub const MAX_SLOT_PARAMS: usize = 16;

/// Preset sector header — identifies valid preset data.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresetHeader {
    /// Magic number ([`PRESET_MAGIC`]).
    pub magic: u32,
    /// Format version ([`PRESET_VERSION`]).
    pub version: u8,
    /// Number of valid presets in the sector.
    pub count: u8,
    /// Reserved for future use.
    pub _pad: [u8; 2],
}

/// A single effect slot within a preset.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EffectSlotData {
    /// Index into the effect list (0–14 for current 15 curated effects).
    pub effect_idx: u8,
    /// Number of valid parameters.
    pub param_count: u8,
    /// Reserved.
    pub _pad: [u8; 2],
    /// A-snapshot parameter values.
    pub params_a: [f32; MAX_SLOT_PARAMS],
    /// B-snapshot parameter values.
    pub params_b: [f32; MAX_SLOT_PARAMS],
}

/// A complete preset (topology + up to 3 effect slots with A/B snapshots).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresetSlot {
    /// 0xFF = empty, 0x01 = valid.
    pub valid: u8,
    /// Topology mode: 0=Linear, 1=Parallel, 2=Fan.
    pub topology: u8,
    /// Number of active effect slots (1–3).
    pub num_slots: u8,
    /// Reserved.
    pub _pad: u8,
    /// Effect slot data (up to 3 slots).
    pub effects: [EffectSlotData; 3],
}

impl Default for PresetHeader {
    fn default() -> Self {
        Self {
            magic: PRESET_MAGIC,
            version: PRESET_VERSION,
            count: 0,
            _pad: [0; 2],
        }
    }
}

impl Default for EffectSlotData {
    fn default() -> Self {
        Self {
            effect_idx: 0,
            param_count: 0,
            _pad: [0; 2],
            params_a: [0.0; MAX_SLOT_PARAMS],
            params_b: [0.0; MAX_SLOT_PARAMS],
        }
    }
}

impl Default for PresetSlot {
    fn default() -> Self {
        Self {
            valid: 0xFF, // 0xFF = empty (erased flash state)
            topology: 0,
            num_slots: 0,
            _pad: 0,
            effects: [EffectSlotData::default(); 3],
        }
    }
}

impl PresetHeader {
    /// Returns `true` if the header contains the expected magic number and version.
    pub fn is_valid(&self) -> bool {
        self.magic == PRESET_MAGIC && self.version == PRESET_VERSION
    }
}

impl PresetSlot {
    /// Serialize this preset to a fixed-size byte array.
    ///
    /// Uses `repr(C)` layout — safe because all fields are `Copy` primitives
    /// with no padding surprises at this alignment. The caller stores this
    /// directly into flash / the RAM buffer.
    pub fn to_bytes(&self) -> [u8; core::mem::size_of::<PresetSlot>()] {
        // SAFETY: PresetSlot is repr(C), all fields are initialized, Copy primitives.
        unsafe { core::mem::transmute_copy(self) }
    }

    /// Deserialize from a byte slice.
    ///
    /// Returns `None` if `bytes` is shorter than `size_of::<PresetSlot>()`.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < core::mem::size_of::<PresetSlot>() {
            return None;
        }
        // SAFETY: repr(C) struct, bytes slice is at least as large as the struct.
        // We copy into a properly aligned local first.
        let mut slot = PresetSlot::default();
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                &raw mut slot as *mut u8,
                core::mem::size_of::<PresetSlot>(),
            )
        };
        dst.copy_from_slice(&bytes[..core::mem::size_of::<PresetSlot>()]);
        Some(slot)
    }
}

/// Preset store backed by a mutable byte buffer (RAM or QSPI-mapped memory).
///
/// This struct is hardware-agnostic — pass it any `&mut [u8]` of at least
/// [`PRESET_SECTOR_SIZE`] bytes. The embedded pedal wires this to QSPI flash
/// reads/writes; tests use a stack/heap buffer.
///
/// # Layout
///
/// ```text
/// bytes[0..8]    : PresetHeader
/// bytes[8..408]  : PresetSlot[0]  (400 bytes)
/// bytes[408..808]: PresetSlot[1]
/// ...
/// ```
pub struct PresetStore<'a> {
    buffer: &'a mut [u8],
}

/// Byte offset where the first preset slot starts (after the header).
const SLOT_OFFSET: usize = core::mem::size_of::<PresetHeader>();

/// Byte size of one preset slot.
const SLOT_SIZE: usize = core::mem::size_of::<PresetSlot>();

impl<'a> PresetStore<'a> {
    /// Create a new store wrapping the given byte buffer.
    ///
    /// The buffer must be at least [`PRESET_SECTOR_SIZE`] bytes.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer }
    }

    /// Write an empty-but-valid header and zero-fill all preset slots.
    ///
    /// Call once when the sector is blank (new device) or after erase.
    pub fn init_empty(&mut self) {
        // Zero the entire buffer first
        for b in self.buffer.iter_mut() {
            *b = 0xFF;
        }
        // Write default header
        let header = PresetHeader::default();
        // SAFETY: repr(C), all fields are Copy primitives.
        let header_bytes: [u8; core::mem::size_of::<PresetHeader>()] =
            unsafe { core::mem::transmute_copy(&header) };
        self.buffer[..SLOT_OFFSET].copy_from_slice(&header_bytes);
    }

    /// Read all preset slots from the buffer.
    ///
    /// Returns `None` for empty slots (`valid == 0xFF`).
    pub fn load_all(&self) -> [Option<PresetSlot>; MAX_USER_PRESETS] {
        let mut result = [None; MAX_USER_PRESETS];
        for i in 0..MAX_USER_PRESETS {
            let start = SLOT_OFFSET + i * SLOT_SIZE;
            let end = start + SLOT_SIZE;
            if end > self.buffer.len() {
                break;
            }
            if let Some(slot) = PresetSlot::from_bytes(&self.buffer[start..end]) {
                if slot.valid == 0x01 {
                    result[i] = Some(slot);
                }
            }
        }
        result
    }

    /// Write a preset into the given slot index.
    ///
    /// Marks the slot as valid (`valid = 0x01`) before writing.
    /// Out-of-range slot indices are silently ignored.
    pub fn save(&mut self, slot: usize, preset: &PresetSlot) {
        if slot >= MAX_USER_PRESETS {
            return;
        }
        let start = SLOT_OFFSET + slot * SLOT_SIZE;
        let end = start + SLOT_SIZE;
        if end > self.buffer.len() {
            return;
        }
        let mut to_write = *preset;
        to_write.valid = 0x01;
        let bytes = to_write.to_bytes();
        self.buffer[start..end].copy_from_slice(&bytes);

        // Update header count
        let count = self.load_all().iter().filter(|s| s.is_some()).count() as u8;
        self.buffer[5] = count; // PresetHeader.count is at byte offset 5 (magic=4, version=1)
    }

    /// Read the current header from the buffer.
    pub fn header(&self) -> PresetHeader {
        // SAFETY: repr(C), bytes are aligned Copy primitives.
        let mut h = PresetHeader::default();
        let src = &self.buffer[..SLOT_OFFSET];
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                &raw mut h as *mut u8,
                core::mem::size_of::<PresetHeader>(),
            )
        };
        dst.copy_from_slice(src);
        h
    }
}

// ---------------------------------------------------------------------------
// Planned: QspiPresetStore — awaiting Embassy QSPI driver for safe async flash access.
// ---------------------------------------------------------------------------
//
// When the pedal-agent wires up Embassy QSPI, implement:
//
// ```rust,ignore
// use embassy_stm32::qspi::{Qspi, TransferConfig};
//
// pub struct QspiPresetStore<'d> {
//     qspi: Qspi<'d, peripherals::QUADSPI, DMA1_CH5>,
// }
//
// impl<'d> QspiPresetStore<'d> {
//     pub fn new(mut qspi: Qspi<'d, ...>) -> Self {
//         // Verify JEDEC ID: send 0x9F, read 3 bytes, check = [0xEF, 0x40, 0x17]
//         Self { qspi }
//     }
//
//     pub fn load_all(&mut self) -> [Option<PresetSlot>; MAX_USER_PRESETS] {
//         // 1. Send opcode 0x03 (Read Data) + 3-byte address PRESET_SECTOR_ADDR
//         // 2. Read PRESET_SECTOR_SIZE bytes into local buffer
//         // 3. Deserialize header, validate magic+version
//         // 4. Deserialize each PresetSlot via PresetSlot::from_bytes()
//     }
//
//     pub fn save(&mut self, slot_idx: usize, preset: &PresetSlot) {
//         // 1. Read entire sector into RAM buffer (load_all into temp)
//         // 2. Patch the target slot in the RAM buffer
//         // 3. Send 0x06 (Write Enable)
//         // 4. Send 0x20 (Sector Erase) + address, poll status 0x05 until busy=0
//         // 5. Write back in 256-byte pages via 0x02 (Page Program)
//         //    - For each page: 0x06 (WE), 0x02 + addr + data, poll busy
//     }
//
//     pub fn erase_all(&mut self) {
//         // 0x06 (Write Enable) + 0x20 (Sector Erase) at PRESET_SECTOR_ADDR
//         // Poll 0x05 status register bit 0 (BUSY) until clear
//     }
// }
// ```

#[cfg(test)]
mod tests {
    use super::*;

    fn make_buffer() -> [u8; PRESET_SECTOR_SIZE] {
        [0xFF; PRESET_SECTOR_SIZE]
    }

    #[test]
    fn header_is_valid_after_init() {
        let mut buf = make_buffer();
        let mut store = PresetStore::new(&mut buf);
        store.init_empty();
        let h = store.header();
        assert!(h.is_valid(), "header should be valid after init_empty");
    }

    #[test]
    fn empty_store_returns_all_none() {
        let mut buf = make_buffer();
        let mut store = PresetStore::new(&mut buf);
        store.init_empty();
        let slots = store.load_all();
        for (i, slot) in slots.iter().enumerate() {
            assert!(slot.is_none(), "slot {i} should be None in empty store");
        }
    }

    #[test]
    fn round_trip_save_and_load() {
        let mut buf = make_buffer();
        let mut store = PresetStore::new(&mut buf);
        store.init_empty();

        let mut preset = PresetSlot::default();
        preset.topology = 1;
        preset.num_slots = 2;
        preset.effects[0].effect_idx = 3;
        preset.effects[0].param_count = 4;
        preset.effects[0].params_a[0] = 0.5;
        preset.effects[0].params_a[1] = 0.75;
        preset.effects[1].effect_idx = 7;
        preset.effects[1].params_b[3] = 0.333;

        store.save(0, &preset);

        let slots = store.load_all();
        let loaded = slots[0].expect("slot 0 should be present after save");

        assert_eq!(loaded.topology, 1);
        assert_eq!(loaded.num_slots, 2);
        assert_eq!(loaded.effects[0].effect_idx, 3);
        assert_eq!(loaded.effects[0].param_count, 4);
        assert!((loaded.effects[0].params_a[0] - 0.5).abs() < f32::EPSILON);
        assert!((loaded.effects[0].params_a[1] - 0.75).abs() < f32::EPSILON);
        assert_eq!(loaded.effects[1].effect_idx, 7);
        assert!((loaded.effects[1].params_b[3] - 0.333).abs() < f32::EPSILON);
    }

    #[test]
    fn save_multiple_slots() {
        let mut buf = make_buffer();
        let mut store = PresetStore::new(&mut buf);
        store.init_empty();

        for i in 0..MAX_USER_PRESETS {
            let mut preset = PresetSlot::default();
            preset.topology = i as u8;
            store.save(i, &preset);
        }

        let slots = store.load_all();
        for (i, slot) in slots.iter().enumerate() {
            let loaded = slot.expect("slot should be present");
            assert_eq!(loaded.topology, i as u8);
        }
    }

    #[test]
    fn out_of_range_slot_is_ignored() {
        let mut buf = make_buffer();
        let mut store = PresetStore::new(&mut buf);
        store.init_empty();
        let preset = PresetSlot::default();
        // Should not panic
        store.save(MAX_USER_PRESETS, &preset);
        store.save(99, &preset);
    }

    #[test]
    fn preset_slot_serialization_round_trip() {
        let mut slot = PresetSlot::default();
        slot.valid = 0x01;
        slot.topology = 2;
        slot.num_slots = 3;
        slot.effects[2].params_a[15] = 1.234_567;

        let bytes = slot.to_bytes();
        let restored = PresetSlot::from_bytes(&bytes).expect("from_bytes should succeed");

        assert_eq!(restored.valid, slot.valid);
        assert_eq!(restored.topology, slot.topology);
        assert_eq!(restored.num_slots, slot.num_slots);
        assert!((restored.effects[2].params_a[15] - 1.234_567).abs() < 1e-5);
    }

    #[test]
    fn from_bytes_too_short_returns_none() {
        assert!(PresetSlot::from_bytes(&[0u8; 4]).is_none());
    }

    #[test]
    fn invalid_header_fails_validation() {
        let h = PresetHeader {
            magic: 0xDEAD_BEEF,
            version: PRESET_VERSION,
            count: 0,
            _pad: [0; 2],
        };
        assert!(!h.is_valid());
    }
}
