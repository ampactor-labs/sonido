//! Blocking QSPI **read** driver for the Daisy Seed's W25Q64 flash.
//!
//! The pedal flashes firmware and patches separately (the GUI writes patch
//! sectors over DFU), so the firmware never needs to *write* flash at
//! runtime — only read patch sectors back. A blocking, single-line read is
//! all that requires, and it sidesteps the two things that make QSPI fiddly
//! on the H7:
//!
//! - **No DMA** → no D-cache invalidation dance on the read buffer. The FIFO is
//!   drained by the CPU straight into the destination.
//! - **Single-line (1-1-1) Read Data (`0x03`)** → no quad-enable sequence, no
//!   dummy-cycle tuning. Slower, but a 2 KB patch sector is read once per patch
//!   switch, not in the audio path.
//!
//! Under BOOT_SRAM the firmware runs from AXI SRAM, so the QUADSPI peripheral is
//! free for indirect-mode use (it is not driving execute-in-place).
//!
//! # Hardware validation
//!
//! This path cannot be unit-tested off-device. Validate it first with the
//! `qspi_read_test` example, which probes the JEDEC id (expect `EF 40 17` for
//! the W25Q64JV) before trusting sector reads.

use embassy_stm32 as hal;
use hal::Peri;
use hal::mode::Blocking;
use hal::peripherals;
use hal::qspi::enums::{AddressSize, DummyCycles, MemorySize, QspiWidth};
use hal::qspi::{Config as QspiConfig, Qspi, TransferConfig};

/// W25Q64 opcodes used here.
const CMD_READ_DATA: u8 = 0x03;
const CMD_JEDEC_ID: u8 = 0x9F;

// ── Patch bank layout ────────────────────────────────────────────────────────
//
// The last 64 KB of the 8 MB flash holds the patch bank: 15 × 4 KB patch
// sectors, with the legacy preset sector kept at the very end (0x007F_F000)
// for one firmware generation. The GUI writes patch slot `n` over DFU at
// `0x9004_0000`-relative address `PATCH_BANK_ADDR + n * 4096`.

/// First byte of the patch bank, relative to flash base.
pub const PATCH_BANK_ADDR: u32 = 0x007F_0000;
/// Size of one patch sector (one [`sonido_patch::SECTOR_SIZE`]).
pub const PATCH_SLOT_SIZE: u32 = 4096;
/// Number of patch slots exposed in the bank.
pub const PATCH_SLOT_COUNT: usize = 15;

/// Flash byte-address of patch slot `slot` (0-based).
pub const fn patch_slot_addr(slot: usize) -> u32 {
    PATCH_BANK_ADDR + (slot as u32) * PATCH_SLOT_SIZE
}

/// Expected JEDEC id for the Daisy Seed's Winbond W25Q64JV (8 MB).
pub const W25Q64_JEDEC_ID: [u8; 3] = [0xEF, 0x40, 0x17];

/// Blocking reader for the on-board QSPI flash.
pub struct QspiFlash<'d> {
    qspi: Qspi<'d, peripherals::QUADSPI, Blocking>,
}

impl<'d> QspiFlash<'d> {
    /// Initialize the QSPI peripheral on the Daisy Seed's fixed Bank-1 pins.
    ///
    /// | Signal | Pin  |
    /// |--------|------|
    /// | IO0    | PF8  |
    /// | IO1    | PF9  |
    /// | IO2    | PF7  |
    /// | IO3    | PF6  |
    /// | CLK    | PF10 |
    /// | NCS    | PG6  |
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        qspi: Peri<'d, peripherals::QUADSPI>,
        io0: Peri<'d, peripherals::PF8>,
        io1: Peri<'d, peripherals::PF9>,
        io2: Peri<'d, peripherals::PF7>,
        io3: Peri<'d, peripherals::PF6>,
        clk: Peri<'d, peripherals::PF10>,
        ncs: Peri<'d, peripherals::PG6>,
    ) -> Self {
        // `Config` is #[non_exhaustive]; start from Default and set fields.
        let mut config = QspiConfig::default();
        config.memory_size = MemorySize::_8MiB; // 8 MB device → 2^23 bytes
        config.address_size = AddressSize::_24bit;
        config.prescaler = 8; // conservative; reads are occasional, off the audio path
        let qspi = Qspi::new_blocking_bank1(qspi, io0, io1, io2, io3, clk, ncs, config);
        Self { qspi }
    }

    /// Read the 3-byte JEDEC id. Use to confirm the flash is responding before
    /// trusting sector data.
    pub fn jedec_id(&mut self) -> [u8; 3] {
        let mut id = [0u8; 3];
        self.qspi.blocking_read(
            &mut id,
            TransferConfig {
                iwidth: QspiWidth::SING,
                awidth: QspiWidth::NONE,
                dwidth: QspiWidth::SING,
                instruction: CMD_JEDEC_ID,
                address: None,
                dummy: DummyCycles::_0,
            },
        );
        id
    }

    /// Whether the attached flash reports the expected W25Q64 id.
    pub fn is_present(&mut self) -> bool {
        self.jedec_id() == W25Q64_JEDEC_ID
    }

    /// Read `buf.len()` bytes starting at flash byte-`address` (1-1-1, `0x03`).
    pub fn read(&mut self, address: u32, buf: &mut [u8]) {
        self.qspi.blocking_read(
            buf,
            TransferConfig {
                iwidth: QspiWidth::SING,
                awidth: QspiWidth::SING,
                dwidth: QspiWidth::SING,
                instruction: CMD_READ_DATA,
                address: Some(address),
                dummy: DummyCycles::_0,
            },
        );
    }
}
