//! QSPI flash probe: the hardware check for the rig bank (`qspi_flash`).
//!
//! 1. Resets the flash out of the bootloader's QPI mode and reads its JEDEC id
//!    (`9D 60 17` for the IS25LP064A on current Seeds) and status register.
//! 2. Reads the vector table of the app the bootloader runs (QSPI `0x40000`).
//! 3. Confirms the write guard refuses the firmware region.
//! 4. Writes a pattern to the state sector, verifies it, and erases it again.
//! 5. Reports whether patch slot 0 holds a decodable rig.
//!
//! Run it from RAM with a probe attached (nothing but the state sector is
//! written, and that sector is left erased):
//!
//! ```sh
//! cargo run --release --example qspi_test --features alloc
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_stm32 as hal;
use embassy_time::{Duration, Instant, Timer};
use embedded_alloc::LlffHeap as Heap;
use panic_probe as _;

use sonido_daisy::led::UserLed;
use sonido_daisy::qspi_flash::{
    FlashError, PATCH_SLOT_SIZE, QspiFlash, STATE_SECTOR_ADDR, patch_slot_addr,
};
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

    defmt::info!("qspi_test: booting…");
    let mut flash = QspiFlash::new(p.QUADSPI, p.PF8, p.PF9, p.PF7, p.PF6, p.PF10, p.PG6);

    let id = flash.jedec_id();
    defmt::info!("JEDEC id: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
    if flash.is_present() {
        defmt::info!("  -> supported 8 MB flash present ✓");
    } else {
        defmt::warn!("  -> unexpected id (expected 9D 60 17 or EF 40 17)");
    }
    defmt::info!("status register: {:08b}", flash.status());

    let mut vectors = [0u8; 8];
    flash.read(0x0004_0000, &mut vectors).await;
    let sp = u32::from_le_bytes([vectors[0], vectors[1], vectors[2], vectors[3]]);
    let reset = u32::from_le_bytes([vectors[4], vectors[5], vectors[6], vectors[7]]);
    defmt::info!(
        "app at 0x90040000: SP {:08X}, reset vector {:08X}",
        sp,
        reset
    );

    let mut sector = [0u8; PATCH_SLOT_SIZE as usize];
    match flash.write_sector(0x0004_0000, &sector).await {
        Err(FlashError::OutsideBank) => defmt::info!("write guard refuses the app region ✓"),
        other => defmt::error!("write guard: expected OutsideBank, got {}", other),
    }

    for (i, b) in sector.iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    let t0 = Instant::now();
    match flash.write_sector(STATE_SECTOR_ADDR, &sector).await {
        Ok(()) => defmt::info!(
            "pattern written and verified in {} ms ✓",
            t0.elapsed().as_millis()
        ),
        Err(e) => defmt::error!("pattern write failed: {}", e),
    }
    let erased = [0xFFu8; PATCH_SLOT_SIZE as usize];
    match flash.write_sector(STATE_SECTOR_ADDR, &erased).await {
        Ok(()) => defmt::info!("state sector erased again ✓"),
        Err(e) => defmt::error!("erase failed: {}", e),
    }

    let mut buf = [0u8; sonido_patch::SECTOR_SIZE];
    flash.read(patch_slot_addr(0), &mut buf).await;
    match sonido_patch::decode(&buf) {
        Ok(patch) => defmt::info!(
            "slot 0: rig with {} node(s), {} edge(s), {} active macro(s)",
            patch.nodes.len(),
            patch.edges.len(),
            patch.active_macro_count()
        ),
        Err(_) => defmt::info!("slot 0: empty or not a rig"),
    }

    defmt::info!("qspi_test: done (heartbeat continues)");
    loop {
        Timer::after(Duration::from_secs(5)).await;
    }
}
