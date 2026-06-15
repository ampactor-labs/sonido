//! QSPI flash read probe — the first hardware-validation step for the patch
//! player.
//!
//! Confirms the on-board W25Q64 responds (JEDEC id `EF 40 17`), then reads patch
//! slot 0 and reports whether it decodes. Flash a patch there first with the GUI
//! or CLI, e.g.:
//!
//! ```sh
//! sonido patch export --from-dsl "distortion:drive=25 | reverb:mix=40" -o p0.bin
//! dfu-util -a 0 -s 0x907F0000:leave -D p0.bin
//! ```
//!
//! Build & flash:
//!
//! ```sh
//! cargo objcopy --example qspi_read_test --release --features alloc -- -O binary qspi_read_test.bin
//! dfu-util -a 0 -s 0x90040000:leave -D qspi_read_test.bin
//! ```
//!
//! Watch `defmt` output (e.g. `probe-rs`/RTT) for the id and decode result.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_stm32 as hal;
use embassy_time::{Duration, Timer};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_daisy::led::UserLed;
use sonido_daisy::qspi_flash::{QspiFlash, W25Q64_JEDEC_ID, patch_slot_addr};
use sonido_daisy::{ClockProfile, heartbeat};

#[global_allocator]
static HEAP: Heap = Heap::empty();

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    sonido_daisy::enable_d2_sram();

    #[allow(unsafe_code)]
    unsafe {
        HEAP.init(0x3000_8000, 256 * 1024);
    }

    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    defmt::info!("qspi_read_test: booting…");

    // Bank-1 QSPI on the Daisy's fixed pins (see QspiFlash::new).
    let mut flash = QspiFlash::new(p.QUADSPI, p.PF8, p.PF9, p.PF7, p.PF6, p.PF10, p.PG6);

    let id = flash.jedec_id();
    defmt::info!("JEDEC id: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
    if id == W25Q64_JEDEC_ID {
        defmt::info!("  -> W25Q64JV present ✓");
    } else {
        defmt::warn!("  -> unexpected id (expected EF 40 17); check QSPI wiring");
    }

    // Read patch slot 0 and attempt to decode it.
    let mut buf = [0u8; sonido_patch::SECTOR_SIZE];
    flash.read(patch_slot_addr(0), &mut buf);
    defmt::info!(
        "slot 0 first bytes: {:02X} {:02X} {:02X} {:02X}",
        buf[0],
        buf[1],
        buf[2],
        buf[3]
    );
    match sonido_patch::decode(&buf) {
        Ok(patch) => {
            defmt::info!(
                "decoded patch: {} node(s), {} edge(s), {} active macro(s)",
                patch.nodes.len(),
                patch.edges.len(),
                patch.active_macro_count()
            );
        }
        Err(_) => defmt::warn!("slot 0 did not decode — flash a patch there first"),
    }

    defmt::info!("qspi_read_test: done (heartbeat continues)");
    loop {
        Timer::after(Duration::from_secs(5)).await;
    }
}
