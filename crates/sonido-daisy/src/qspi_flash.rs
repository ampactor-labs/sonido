//! QSPI driver for the Daisy Seed's 8 MB flash (IS25LP064A on current Seeds;
//! the W25Q64 has the same command set): reads anywhere, erases and programs
//! only inside the rig bank at the top of flash.
//!
//! The firmware writes rigs it receives over USB and its own state sector;
//! reads happen when a rig is loaded. Single-line commands without DMA keep
//! it simple and sidestep the two things that make QSPI fiddly on the H7:
//!
//! - **No DMA** → no D-cache invalidation dance on the read buffer. The FIFO is
//!   drained by the CPU straight into the destination.
//! - **Single-line (1-1-1) Read Data (`0x03`)** → no quad-enable sequence, no
//!   dummy-cycle tuning. Slower, but a 4 KB patch sector is read once per patch
//!   switch, not in the audio path.
//!
//! Erase and program wait for the flash with `embassy_time` timers rather than
//! spinning, so the audio loop keeps running through a sector erase (tens to
//! hundreds of milliseconds).
//!
//! Under BOOT_SRAM the firmware runs from AXI SRAM, so the QUADSPI peripheral is
//! free for indirect-mode use (it is not driving execute-in-place).
//!
//! # Hardware validation
//!
//! This path cannot be unit-tested off-device. Validate it first with the
//! `qspi_test` example, which probes the JEDEC id (`9D 60 17` for the
//! IS25LP064A, `EF 40 17` for a W25Q64JV) before trusting sector reads.

use embassy_stm32 as hal;
use embassy_time::{Duration, Instant, Timer};
use hal::Peri;
use hal::mode::Blocking;
use hal::peripherals;
use hal::qspi::enums::{AddressSize, DummyCycles, MemorySize, QspiWidth};
use hal::qspi::{Config as QspiConfig, Qspi, TransferConfig};

/// Opcodes used here; the IS25LP064A and the W25Q64 share them.
const CMD_READ_DATA: u8 = 0x03;
const CMD_JEDEC_ID: u8 = 0x9F;
const CMD_RESET_ENABLE: u8 = 0x66;
const CMD_RESET: u8 = 0x99;
const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS: u8 = 0x05;
const CMD_PAGE_PROGRAM: u8 = 0x02;
const CMD_SECTOR_ERASE: u8 = 0x20;

/// Status register: write in progress.
const STATUS_WIP: u8 = 0x01;
/// Program granularity of the flash.
const PAGE_SIZE: usize = 256;
/// Total flash size.
const FLASH_SIZE: u32 = 8 * 1024 * 1024;

// ── Patch bank layout ────────────────────────────────────────────────────────
//
// The last 64 KB of the 8 MB flash holds the rig bank: 15 × 4 KB patch
// sectors, then the pedal's state sector at the very end (0x007F_F000). Over
// DFU, slot `n` is at `0x9000_0000 + PATCH_BANK_ADDR + n * 4096` (slot 0 =
// `0x907F_0000`); the firmware writes the same sectors itself.

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

/// The pedal's own state (active rig, autosaved jam), after the patch slots.
pub const STATE_SECTOR_ADDR: u32 = patch_slot_addr(PATCH_SLOT_COUNT);

/// Why a flash write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum FlashError {
    /// The address is outside the rig bank or not sector-aligned; the firmware
    /// region is never erased or programmed.
    OutsideBank,
    /// The flash stayed busy past its worst-case erase/program time.
    Timeout,
    /// Read-back after programming did not match.
    Verify,
}

/// JEDEC id of the ISSI IS25LP064A (8 MB), the flash on current Daisy Seeds.
pub const IS25LP064_JEDEC_ID: [u8; 3] = [0x9D, 0x60, 0x17];
/// JEDEC id of the Winbond W25Q64JV (8 MB), the same size and command set.
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
        let mut flash = Self { qspi };
        flash.reset_to_spi();
        flash
    }

    /// Software-reset the flash into single-line SPI mode.
    ///
    /// The Daisy bootloader (libDaisy) leaves the IS25LP064A in QPI mode, where
    /// it ignores single-line commands (reads come back `FF`), and only a power
    /// cycle or a reset command leaves it. A probe reset doesn't power-cycle the
    /// flash. So send Reset Enable + Reset over four lines (heard in QPI mode)
    /// and then over one (heard in SPI mode). The bootloader re-enters QPI on
    /// its own at the next boot.
    fn reset_to_spi(&mut self) {
        for width in [QspiWidth::QUAD, QspiWidth::SING] {
            for instruction in [CMD_RESET_ENABLE, CMD_RESET] {
                self.qspi.blocking_command(TransferConfig {
                    iwidth: width,
                    awidth: QspiWidth::NONE,
                    dwidth: QspiWidth::NONE,
                    instruction,
                    address: None,
                    dummy: DummyCycles::_0,
                });
            }
            // tRST: 100 µs before the next command (480 MHz core).
            cortex_m::asm::delay(48_000);
        }
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

    /// Whether the attached flash reports a supported 8 MB id.
    pub fn is_present(&mut self) -> bool {
        matches!(self.jedec_id(), IS25LP064_JEDEC_ID | W25Q64_JEDEC_ID)
    }

    /// Read the status register.
    pub fn status(&mut self) -> u8 {
        let mut status = [0u8; 1];
        self.qspi.blocking_read(
            &mut status,
            TransferConfig {
                iwidth: QspiWidth::SING,
                awidth: QspiWidth::NONE,
                dwidth: QspiWidth::SING,
                instruction: CMD_READ_STATUS,
                address: None,
                dummy: DummyCycles::_0,
            },
        );
        status[0]
    }

    fn command(&mut self, instruction: u8, address: Option<u32>) {
        self.qspi.blocking_command(TransferConfig {
            iwidth: QspiWidth::SING,
            awidth: if address.is_some() {
                QspiWidth::SING
            } else {
                QspiWidth::NONE
            },
            dwidth: QspiWidth::NONE,
            instruction,
            address,
            dummy: DummyCycles::_0,
        });
    }

    /// Wait for the write in progress to finish, yielding to other tasks.
    async fn wait_ready(&mut self, poll: Duration, limit: Duration) -> Result<(), FlashError> {
        let deadline = Instant::now() + limit;
        while self.status() & STATUS_WIP != 0 {
            if Instant::now() > deadline {
                return Err(FlashError::Timeout);
            }
            Timer::after(poll).await;
        }
        Ok(())
    }

    /// Erase the 4 KB sector at `address` and program `data` into it, then
    /// read it back.
    ///
    /// Only sectors inside the rig bank (`PATCH_BANK_ADDR..8 MB`) are accepted.
    ///
    /// # Errors
    ///
    /// [`FlashError::OutsideBank`] for any other address, [`FlashError::Timeout`]
    /// if the flash never finishes, [`FlashError::Verify`] on a read-back mismatch.
    pub async fn write_sector(
        &mut self,
        address: u32,
        data: &[u8; PATCH_SLOT_SIZE as usize],
    ) -> Result<(), FlashError> {
        if !(PATCH_BANK_ADDR..FLASH_SIZE).contains(&address)
            || !address.is_multiple_of(PATCH_SLOT_SIZE)
        {
            return Err(FlashError::OutsideBank);
        }
        // Sector erase: typically 70 ms, at most 300 ms.
        self.command(CMD_WRITE_ENABLE, None);
        self.command(CMD_SECTOR_ERASE, Some(address));
        self.wait_ready(Duration::from_millis(2), Duration::from_millis(1000))
            .await?;
        for (i, page) in data.chunks(PAGE_SIZE).enumerate() {
            // An erased page is already all 0xFF.
            if page.iter().all(|&b| b == 0xFF) {
                continue;
            }
            self.command(CMD_WRITE_ENABLE, None);
            self.qspi.blocking_write(
                page,
                TransferConfig {
                    iwidth: QspiWidth::SING,
                    awidth: QspiWidth::SING,
                    dwidth: QspiWidth::SING,
                    instruction: CMD_PAGE_PROGRAM,
                    address: Some(address + (i * PAGE_SIZE) as u32),
                    dummy: DummyCycles::_0,
                },
            );
            // Page program: typically 0.2 ms, at most 0.8 ms.
            self.wait_ready(Duration::from_micros(200), Duration::from_millis(10))
                .await?;
        }
        let mut page = [0u8; PAGE_SIZE];
        for (i, expected) in data.chunks(PAGE_SIZE).enumerate() {
            self.read(address + (i * PAGE_SIZE) as u32, &mut page).await;
            if page != expected {
                return Err(FlashError::Verify);
            }
        }
        Ok(())
    }

    /// Read `buf.len()` bytes starting at flash byte-`address` (1-1-1, `0x03`).
    ///
    /// Reads a page (about 80 µs) at a time and yields between pages. The
    /// transfer blocks the executor, and while a rig plays at 75% load the
    /// audio task misses its block if held off for more than about 0.18 ms:
    /// one 4 KB read (1.3 ms) used to drop audio on every state save.
    pub async fn read(&mut self, address: u32, buf: &mut [u8]) {
        for (i, chunk) in buf.chunks_mut(PAGE_SIZE).enumerate() {
            self.qspi.blocking_read(
                chunk,
                TransferConfig {
                    iwidth: QspiWidth::SING,
                    awidth: QspiWidth::SING,
                    dwidth: QspiWidth::SING,
                    instruction: CMD_READ_DATA,
                    address: Some(address + (i * PAGE_SIZE) as u32),
                    dummy: DummyCycles::_0,
                },
            );
            Timer::after_micros(1).await;
        }
    }
}
