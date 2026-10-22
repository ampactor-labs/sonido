# Embedded Guide

Deploying Sonido on the Electrosmith Daisy Seed (STM32H750 Cortex-M7) and the
Cleveland Music Co. Hothouse DIY pedal platform.

> **Current hardware:** Daisy Seed 65 MB (Rev 7 / PCM3060) + Hothouse DIY pedal
> platform (working firmware, hardware tuning ongoing). Phases 1-2 require bare Seed + USB;
> Phases 3-4 require the Hothouse for audio I/O and controls.

---

## Daisy Seed

| Spec | Value |
|------|-------|
| MCU | STM32H750IBK6 (ARM Cortex-M7, single core) |
| Clock | 480 MHz (libDaisy defaults to 400 MHz for thermal headroom) |
| FPU | Single-precision hardware FPU (no double, no SIMD) |
| SDRAM | 64 MB (Alliance AS4C16M32MSA-6) — "65 MB" variant |
| QSPI Flash | 8 MB (ISSI IS25LP064A on current Seeds, JEDEC id `9D 60 17`; the Winbond W25Q64JV, `EF 40 17`, shares its command set) |
| Audio Codec | PCM3060 (TI) — Rev 7, current production |
| Audio | 24-bit stereo, up to 96 kHz |
| GPIO | 31 configurable pins (12x 16-bit ADC, 2x 12-bit DAC) |
| USB | Micro-USB (power, flashing, debug, serial) |

> **Rev 7 noise floor:** ~15 dB worse than Rev 4 (225 uVrms vs 40 uVrms).
> Contributing factors: higher analog voltage (3.6 Vpp vs 2.1 Vpp) and PCB
> ground plane design. No official fix from Electrosmith.
> sonido-daisy has its own codec driver (`src/audio.rs`) and pin macro
> (`codec_pins!`) for this Seed 1.2 / PCM3060 board; it does not depend on daisy-embassy.

### Memory Map

| Region | Address | Size | Wait States | Use |
|--------|---------|------|:-----------:|-----|
| ITCM | `0x0000_0000` | 64 KB | 0 (instruction only) | Unused by `memory.x` |
| DTCM | `0x2000_0000` | 128 KB | 0 (data only) | `RAM`: stack, `.bss`, `.data` |
| AXI SRAM | `0x2400_0000` | 512 KB (480 KB usable under BOOT_SRAM) | 0–1 | `FLASH`: firmware code + rodata (copied here by the bootloader) |
| D2 SRAM1–3 | `0x3000_0000` | 288 KB | 1–2 | `RAM_D2_DMA` (first 32 KB): SAI DMA buffers (`.sram1_bss`); `RAM_D2` (256 KB at `0x3000_8000`): general purpose |
| D3 SRAM4 | `0x3800_0000` | 64 KB | 1–2 | Low-power domain |
| SDRAM | `0xC000_0000` | 64 MB | 4–8 | Heap (all `Vec`/`Box` allocations: delay lines, reverb, looper) |

Region names in the last column are the ones in `crates/sonido-daisy/memory.x`.
Under BOOT_SRAM the AXI SRAM holds the program, so effect memory comes from the
SDRAM heap, not from internal SRAM.

### Memory Budget

Per-effect heap for the 8 jam effects (`sonido_registry::JAM_EFFECT_IDS`), as
recorded in `sonido_platform::cycle_table::EFFECT_SDRAM`: the bytes each effect
allocates at 48 kHz as `EffectRegistry::create` builds it (kernel plus smoothed
adapter, the form the pedal plays), measured on the host.

| Effect | Heap (bytes) | Notes |
|--------|-------------------:|-------|
| distortion | 576 | |
| bitcrusher | 216 | |
| filter | 304 | |
| ringmod | 200 | |
| chorus | 23,704 | 4 modulated `InterpolatedDelay` lines |
| phaser | 752 | Allpass stages and LFOs held inline |
| delay | 776,432 | 2 s stereo delay lines plus diffusion allpasses |
| reverb | 118,276 | 8 FDN delay lines, modulated allpasses, early-reflection tap delay, stereo predelay |

The heaviest of the other effects a rig can hold: looper 23,040,312 (60 s
stereo), time_stretch 1,049,032, texture 524,912, glitch 524,608, spring_reverb
114,780, plate_reverb 106,188, harmonic_habitat 100,208.

The jam runs up to 3 effects at once; its heaviest combination is
delay + reverb + chorus = **918,412 bytes (~0.9 MB)**. That exceeds the AXI SRAM
(which holds the code anyway), so the pedal puts its heap on the 64 MB SDRAM,
where any combination fits with room to spare; a rig is refused only if its
effects' `EFFECT_SDRAM` sum passes `SDRAM_BUDGET_BYTES` (64 MB). See
[Memory Placement](#memory-placement-boot_sram) below.

### Audio Path

```
Codec ADC → SAI RX → DMA → SRAM buffer (ping)
                              │
                     Process buffer (pong) ← CPU
                              │
         SAI TX ← DMA ← SRAM buffer (ping) → Codec DAC
```

- **DMA double-buffer** — CPU processes one half while DMA fills/drains the other
- **Block size** — 32 samples (0.67 ms at 48 kHz), matching libDaisy C++ default — see `BLOCK_SIZE` in `sonido-daisy/src/lib.rs`
- **Format** — 24-bit I2S, processed as `f32` internally
- **Known limitation** — Embassy uses a circular-buffer workaround, not hardware
  M0AR/M1AR double-buffer registers
  ([embassy#702](https://github.com/embassy-rs/embassy/issues/702))

---

## sonido-daisy Crate

Firmware crate with tiered examples at `crates/sonido-daisy/`.
It is excluded from the Cargo workspace (`exclude` in the root `Cargo.toml`), so
build it from its own directory, where `.cargo/config.toml` sets the
`thumbv7em-none-eabihf` target and `target-cpu=cortex-m7`. `build.rs` passes the
linker scripts (`link.x`, `defmt.x`), so a `RUSTFLAGS` variable (which replaces
config rustflags) can't drop them; names the build (`SONIDO_FIRMWARE_VERSION`, from
`git describe`, which `sonido pedal info` reports); and embeds the factory rigs. CI's
`firmware` job builds and size-checks `sonido_pedal.bin`, and releases attach it.

```bash
# Cross-compile check (no flash)
cd crates/sonido-daisy
cargo check --examples --features alloc,platform
```

### Tier System

| Tier | Example | What It Validates | Hardware Needed |
|:----:|---------|-------------------|-----------------|
| 1 | `blinky_bare.rs` | Toolchain, flash, BOOT_SRAM path | Seed + USB |
| 1 | `blinky.rs` | Embassy runtime + clock init | Seed + USB |
| 1 | `heap_test.rs` | D2 SRAM clock enable + heap allocation (heap at `0x3000_8000`) | Seed + USB |
| 2 | `bench_mini.rs` | One kernel (PreampKernel) runs without crashing — fast LED blink = OK | Seed + USB |
| 2 | `bench_kernels.rs` | DWT cycle counts for all 36 kernels, mean and worst block (USB serial, defmt RTT) | Seed + USB, or Seed + SWD probe |
| 3 | `silence.rs` | Codec/SAI/DMA init — digital silence output | Hothouse |
| 3 | `tone_out.rs` | DAC signal path health (440 Hz sine) | Hothouse |
| 3 | `square_out.rs` | DAC health check (1 kHz full-scale square) | Hothouse |
| 3 | `passthrough.rs` | Codec, DMA, audio passthrough | Seed + audio I/O |
| 3 | `passthrough_blink.rs` | Audio passthrough + LED heartbeat task | Hothouse |
| 3 | `hothouse_diag.rs` | All Hothouse hardware (input level, knobs, toggles, footswitches) | Hothouse |
| 3.5 | `convert_passthrough.rs` | u24↔f32 conversion round trip, no DSP | Hothouse |
| 3.75 | `simple_gain.rs` | Float math + `ControlBuffer` reads (K1 = gain) | Hothouse |
| — | `graph_audio_test.rs` | `ProcessingGraph` + SDRAM heap coexisting with SAI DMA | Hothouse |
| — | `qspi_test.rs` | QSPI flash: JEDEC id, the app's vector table, the write guard, a write/verify/erase of the state sector, decode of bank slot 0 (defmt output) | Seed + SWD probe |
| 4 | `single_effect.rs` | One kernel via `EffectSlot` + biased knob mapping, noon = default (distortion by default) | Hothouse |
| 4 | `single_delay.rs`, `single_reverb.rs` | One kernel driven directly via `from_knobs()` | Hothouse |
| 5 | `sonido_pedal.rs` | The pedal: a rig player (`PatchPlayer`) over the jam, the flash rig bank and the factory rigs, USB rig link, state saved to flash | Hothouse |

Examples that need features (`required-features` in `Cargo.toml`): `sonido_pedal`
and `single_effect` need `--features alloc,platform`; `graph_audio_test`,
`qspi_test`, `single_delay` and `single_reverb` need `--features alloc`.

### Modern Rust on Daisy Seed

**sonido-daisy** owns the full platform layer (clock, audio, ADC, LED). Key patterns:

- **Clock**: `sonido_daisy::rcc_config(ClockProfile::Performance)` or `::Efficient` — 480 MHz / 400 MHz with proper PLL2_P ADC clock fix.
- **Audio**: `AudioPeripherals::prepare_interface()` → `start_interface()` → `start_callback()` — async loop, one callback per DMA transfer (~0.667 ms at 48 kHz, 32-sample blocks). SAI clocks start inside `start_callback()`.
- **LED / UI**: Use `sonido_daisy::heartbeat` — the shared lub-dub blink task. Every binary spawns it: `spawner.spawn(heartbeat(UserLed::new(p.PC7))).unwrap();`.
- **USB / Serial**: Same spawned-task pattern. See `hothouse_diag.rs`.
- **Audio callback is real-time** — NEVER block in the audio callback. No ADC reads, no USB, no allocation. Only pure DSP math and lock-free ControlBuffer reads.

Reference implementation: `crates/sonido-daisy/examples/single_effect.rs`

### Control / Audio Separation

All 6 knobs use uniform `blocking_read()` polling in a 100 Hz Embassy task (`hothouse_control_task`, `POLL_INTERVAL_MS` = 10 in `hothouse.rs`). At ~8 µs per channel (CYCLES387_5 sample time), 6 reads cost ~48 µs per 10 ms cycle (~0.5% CPU). This matches libDaisy's own `AnalogControl` polling approach — no DMA channel, DMA buffer, or D2 SRAM allocation needed for the control path. GPIO reads (toggles, footswitches) are instant register accesses. Shared state flows through a lock-free `ControlBuffer` using `Relaxed` atomics. Knob values are not IIR-smoothed (an IIR added 460 ms of lag): `ControlBuffer::write_knob` snaps readings within 0.005 of either end to 0.0/1.0, holds the stored value until the reading leaves a hysteresis band of 0.15% of travel (`KNOB_HYSTERESIS`) and then follows it at the band's edge, and raises a change flag when a value moves more than 3e-4. An untouched knob therefore reads exactly the same every poll, so the parameters it drives settle and their smoothing stops costing CPU.

**Audio-side control decimation:** The audio callback checks controls every `CONTROL_POLL_EVERY` (15) blocks, not every block. At 48 kHz / 32-sample blocks = 1 500 blocks/s, this yields ~100 Hz control rate, matching the control task so each update is seen once. This constant is coupled to `BLOCK_SIZE` and `SAMPLE_RATE` — changing either requires recalculating. Defined in `effect_slot.rs`.

```
┌─────────────────────┐              ┌──────────────────────┐
│ hothouse_control_task│  ControlBuffer  │   Audio Callback     │
│ (100 Hz, async task)│───────────────→│ (1500 Hz, real-time) │
│                     │  (lock-free)    │                      │
│ • ADC blocking_read │              │ • read_knob()         │
│ • GPIO toggle reads │              │ • read_toggle()       │
│ • GPIO footswitches │              │ • read_footswitch()   │
│ • edge snap + flags │  ←────────────│ • write_led()         │
│ • LED GPIO output   │  LED bridge   │                      │
└─────────────────────┘              └──────────────────────┘
```

**Library modules:**

| Module | Purpose | Dependencies |
|--------|---------|-------------|
| `controls.rs` | `ControlBuffer<KNOBS,TOGGLES,FS,LEDS>` — lock-free shared state with change detection, LED bridge; `HothouseBuffer` alias | `core` only |
| `hothouse.rs` | `HothouseControls` (knobs array + GPIO), `hothouse_control_task` (uniform polling), `hothouse_pins!` macro, `decode_toggle`; `HothousePlatform` (`PlatformController`, feature `platform`) | `controls.rs` + Embassy |
| `lib.rs` | Constants, DWT helpers, `heartbeat`/`usb_task` tasks, sample conversion, `codec_pins!`; re-exports `Adapter`/`DirectPolicy` (`sonido-core/src/kernel/adapter.rs`, feature `alloc`) | `sonido-core` |
| `audio.rs` | SAI1 + DMA codec driver: `AudioPeripherals`, `Interface<Idle/Running>` | Embassy |
| `rcc.rs` | `ClockProfile`, `rcc_config()`, `cycles_per_block()` | Embassy |
| `sdram.rs` | `init_sdram!`, `configure_mpu()`, `enable_dcache()`, `SDRAM_SIZE` | `stm32-fmc` |
| `adc.rs`, `led.rs` | CPU temperature sensor enable; `UserLed` (PC7) | Embassy |
| `qspi_flash.rs` | `QspiFlash` (IS25LP064A / W25Q64): reads anywhere, a page (256 B) at a time with a yield between pages; `write_sector()` erases, programs and verifies a 4 KB sector, only inside the rig bank (`PATCH_BANK_ADDR`, `PATCH_SLOT_COUNT` = 15, `patch_slot_addr()`, `STATE_SECTOR_ADDR`) | Embassy |
| `patch_loader.rs` | `make_effect()` (through `EffectRegistry::create`), `load_patch()` (read + decode a bank slot), `check_rig()` (the pedal's CPU/memory/structure rule), `error_blink_code()` (feature `platform`) | `sonido-patch`, `sonido-platform` (cycle_table) |
| `rigs.rs` | `FACTORY_RIG_COUNT`, `factory_rig()` — the factory rigs `build.rs` encodes from `sonido_rigs::factory_rigs()` (feature `platform`) | `sonido-patch` |
| `effect_slot.rs` | `EffectSlot` — click-free bypass ramp, NaN/Inf sanitization, knob mapping bridge; also exports `BypassCrossfade`, `sanitize_stereo`, `CONTROL_POLL_EVERY` (feature `platform`) | `sonido-core` (SmoothedParam), `sonido-platform` (knob_mapping) |
| `tap_tempo.rs` | `TapTempo` — tap-tempo BPM (used by `sonido_pedal`) | `core` |
| `expression.rs`, `midi.rs`, `device_sync.rs` | Expression-pedal smoothing, USB-MIDI packet parser, two-Seed sync frame format — library code not yet used by any example | `core` |

`build.rs` also generates `$OUT_DIR/factory_rigs.rs` (each factory rig as its
encoded 4 KB-sector bytes), so the image carries the rigs without the code that
builds them. `sonido-rigs` is a build-dependency only (beside a `std` build of
`sonido-patch` for the build script).

The ADC→parameter conversions live in `sonido-platform`, not in this crate:
`crates/sonido-platform/src/param_map.rs` (`adc_to_param`, `adc_to_param_biased`)
and `knob_mapping.rs` (`knob_map`, `knob_to_param`).

### Biased Knob Mapping (Noon = Default)

The Hothouse 6-knob pedal uses a "noon = sweet spot" convention: knobs at 12 o'clock
should produce a musically useful tone. The sweet spot is the parameter's
**descriptor default**: every effect's defaults are its voiced sweet spot, level-matched
by the level contract (`sonido_core::level`), so a pedal with every knob at noon and a
freshly opened plugin start identical. There is no separate noon table. This is
achieved by biasing the ADC-to-parameter mapping curve, **not** by narrowing descriptor
ranges (see ADR-030).

`sonido_platform::adc_to_param_biased(desc, noon, normalized)` splits the knob travel at center:
- `[0.0, 0.5]` maps `desc.min` → `noon`
- `[0.5, 1.0]` maps `noon` → `desc.max`

Both halves respect the descriptor's `ParamScale` (Linear, Logarithmic, Power).
STEPPED parameters use the same split, rounded to whole steps, so the noon option is
what noon selects: the options below it share the left half of the travel and those
above it the right half (a default on the first option keeps the whole left half
there). Noon selects the default even at an end of the range: a parameter voiced at its
maximum (a distortion's 100% mix) sweeps its whole range in the left half and holds
there in the right.

Firmware calls the wrapper `sonido_platform::knob_to_param`, which uses the descriptor
default as noon and applies per-effect knob floors that tighten the descriptor's
minimum for hardware travel only (`knob_min_override`): delay time starts at 30 ms
(below that echoes read as comb filtering), filter cutoff at 150 Hz (below that a
resonant low-pass on guitar is silence) and bitcrusher bit depth at 5 (below that a
guitar at instrument level never crosses the first quantization step):

```rust
use sonido_platform::knob_mapping::{knob_map, knob_to_param, NULL_KNOB};

// In the control poll:
let map = knob_map(effect_id).unwrap();
for k in 0..6 {
    let idx = map[k];
    if idx == NULL_KNOB { continue; }
    let desc = effect.effect_param_info(idx as usize).unwrap();
    let value = knob_to_param(effect_id, idx as usize, &desc, CONTROLS.read_knob(k));
    effect.effect_set_param(idx as usize, value);
}
```

`EffectSlot::apply_knobs()` does exactly this.

To change what noon sounds like, change the effect's default (descriptor and
`Params::default` together; `ParamDescriptor::with_default` sets it on a factory
descriptor), then re-run the level calibration
(`cargo run -p sonido-effects --example calibrate --release`) so the new default
stays level-matched. The jam's setup page biases its own three knobs the same way
(`adc_to_param_biased`, noon at the default): input −12..+12 dB and master −60..+12 dB
with 0 dB at noon, morph time 0.2–5 s (logarithmic) with 1 s at noon. `adc_to_param()`
(linear mapping) remains available for controls with no sweet spot.

### Knob Mapping Verification

The mapping functions live in `sonido-platform`, which builds on the host, so
`param_map.rs` and `knob_mapping.rs` carry their own unit tests
(`cargo test -p sonido-platform`). Two integration tests run the real
`adc_to_param_biased` against every registered effect:

| Test | What it catches |
|------|----------------|
| `noon_mapping.rs` `biased_mapping_endpoints` (sonido-effects) | Dead zone where knob 0→min or knob 1→max fails, for every writable parameter |
| `noon_mapping.rs` `noon_is_the_default` | Knob center doesn't select the descriptor default |
| `noon_mapping.rs` `biased_mapping_monotonic` | Non-monotonic output from the biased split |
| `knob_calibration.rs` `noon_is_level_matched_and_extremes_are_usable` (sonido-platform) | A jam effect at noon more than ±1.5 dB (integrated LUFS) from the dry guitar; a knob extreme outside −15..+8 dB (K3, the level knob: −30..+13 dB) or non-finite |

`knob_calibration.rs` drives each of the 8 `JAM_EFFECT_IDS` through its `knob_map`
and `knob_to_param` with a synthetic strummed guitar peaking at −20 dBFS
(`sonido_analysis::signals::strummed_guitar`); `--nocapture` prints the table.

**READ_ONLY diagnostic params** — metering outputs, not knob-writable. The mapping
tests skip them and no knob map points at them:

| Effect | Index | Param |
|--------|-------|-------|
| Compressor | 11 | Gain Reduction |
| Gate | 8 | Gate Open |
| Chorus | 9 | LFO Phase |
| Flanger | 8 | LFO Phase |
| Phaser | 10 | LFO Phase |
| Tremolo | 7 | LFO Phase |

**Guitarist-ready effects** — effects with ≤6 writable params, directly mappable to Hothouse's
6 knobs without paging: distortion (6), preamp (3), wah (5), filter (4), vibrato (3),
bitcrusher (5), ringmod (5), limiter (5), looper (6).

**Usage pattern** (all Hothouse examples):

```rust
use sonido_daisy::controls::HothouseBuffer;
use sonido_daisy::hothouse::hothouse_control_task;

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // ... clock + peripheral init ...

    // Extract control pins BEFORE audio peripherals (both consume from p)
    let ctrl = sonido_daisy::hothouse_pins!(p);
    spawner.spawn(hothouse_control_task(ctrl, &CONTROLS)).unwrap();

    // ... audio setup ...

    interface.start_callback(move |input, output| {
        // Lock-free reads — never blocks
        let drive = CONTROLS.read_knob(0);
        let toggle = CONTROLS.read_toggle(0);
        let foot = CONTROLS.read_footswitch(0);
        CONTROLS.write_led(0, 1.0); // LED bridge
        // ... DSP processing ...
    }).await;
}
```

### Embassy Patterns

#### StaticCell for USB Buffers

The USB serial examples (`bench_kernels.rs`, `hothouse_diag.rs`) use
`StaticCell<T>` from the `static_cell` crate for USB buffer allocation. This avoids `static mut` and the associated `unsafe` blocks:

```rust
use static_cell::StaticCell;
use embassy_usb::class::cdc_acm::State;

static EP_OUT_BUF:  StaticCell<[u8; 256]>      = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]>      = StaticCell::new();
static BOS_DESC:    StaticCell<[u8; 256]>      = StaticCell::new();
static MSOS_DESC:   StaticCell<[u8; 256]>      = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]>       = StaticCell::new();
static CDC_STATE:   StaticCell<State<'static>> = StaticCell::new();

// In main — no unsafe needed:
let cdc_state = CDC_STATE.init(State::new());
let mut builder = embassy_usb::Builder::new(
    driver, config,
    CONFIG_DESC.init([0; 256]),
    BOS_DESC.init([0; 256]),
    MSOS_DESC.init([0; 256]),
    CONTROL_BUF.init([0; 64]),
);
```

#### Shared Tasks from sonido_daisy

Import shared Embassy tasks instead of defining them locally:

```rust
use sonido_daisy::{heartbeat, usb_task};

// In main:
spawner.spawn(heartbeat(led)).unwrap();
spawner.spawn(usb_task(usb)).unwrap();
```

#### Lock-Free Shared State (Audio Callback ↔ Tasks)

The audio callback runs synchronously in the Embassy executor thread at
1500 Hz. Data crosses the callback↔task boundary via `ControlBuffer` — a
generic lock-free buffer using `Relaxed` atomics (single-core Cortex-M7,
no cache coherence issues). `HothouseBuffer` is the Hothouse-specific alias:

```rust
use sonido_daisy::controls::HothouseBuffer;

static CONTROLS: HothouseBuffer = HothouseBuffer::new();

// Control task (writer, 100 Hz): normalized ADC values
// hothouse_control_task writes knobs, toggles, footswitches

// Audio callback (reader, 1500 Hz): lock-free reads
let drive = CONTROLS.read_knob(0);     // 0.0–1.0, edges snapped
let (val, changed) = CONTROLS.read_knob_changed(0); // with change flag
CONTROLS.write_led(0, 1.0);           // LED bridge (callback→task)
```

For application-specific atomics (e.g., audio level metering in
`hothouse_diag.rs`), raw `AtomicU32`/`AtomicI32` with `Relaxed` ordering
remain appropriate.

### Library — `src/lib.rs`

| Symbol | Value | Purpose |
|--------|-------|---------|
| `SAMPLE_RATE` | 48,000.0 Hz | Default audio sample rate |
| `BLOCK_SIZE` | 32 samples | DMA half-transfer size |
| `CHANNELS` | 2 | Stereo |
| `DMA_BUFFER_SIZE` | 128 | `BLOCK_SIZE * CHANNELS * 2` (double-buffer) |
| `CPU_CLOCK_HZ` | 480,000,000 | Performance-profile clock |
| `CYCLES_PER_BLOCK` | 320,000 | CPU cycles available per block at 480 MHz (`rcc::cycles_per_block()` for either profile) |
| `measure_cycles(\|\| { })` | — | DWT cycle counter wrapper |
| `enable_cycle_counter()` | — | Call once at startup before measuring |
| `enable_d2_sram()` | — | Turn on D2 SRAM1–3 clocks (off after reset) before touching DMA buffers |
| `enable_fpu_ftz()` | — | FPSCR flush-to-zero + default-NaN |
| `u24_to_f32()` / `f32_to_u24()` | — | SAI sample conversion (24-bit right-aligned, 2^23 scale) |
| `heartbeat` / `usb_task` | — | Shared Embassy tasks |
| `BufWriter` | — | `fmt::Write` into a fixed byte slice |
| `codec_pins!` | — | Build `audio::CodecPins` for the Seed 1.2 |
| `ControlBuffer` | — | Re-export from `controls.rs` |
| `ClockProfile`, `rcc_config`, `cycles_per_block` | — | Re-exports from `rcc.rs` |
| `Adapter`, `DirectPolicy` | — | Re-export of `sonido_core::kernel::{Adapter, DirectPolicy}` (`sonido-core/src/kernel/adapter.rs`, feature `alloc`) |

`EffectSlot`, `BypassCrossfade`, `sanitize_stereo` and `CONTROL_POLL_EVERY` are not
re-exported at the crate root; import them from `sonido_daisy::effect_slot`.

### Feature Flags

| Feature | Enables | Required By |
|---------|---------|-------------|
| *(none)* | Core library: audio, controls, hothouse, LED, RCC, SDRAM, QSPI flash, tap tempo | Simple examples (blinky, passthrough, diagnostics, bench_kernels) |
| `alloc` | `Adapter`/`DirectPolicy` re-exports | graph_audio_test, qspi_test, single_delay, single_reverb |
| `platform` | Implies `alloc`; pulls in `sonido-platform`; enables `HothousePlatform`, `effect_slot`, `patch_loader`, `rigs` | sonido_pedal, single_effect |

### Dependencies

| Crate | Version | Purpose |
|-------|:-------:|---------|
| `embassy-stm32` | 0.5 | Async HAL — SAI, DMA, GPIO, ADC, FMC |
| `embassy-executor` | 0.9 | Async task executor for Cortex-M |
| `embassy-time` | 0.5 | Timer and delay utilities |
| `embassy-usb` | 0.5 | USB CDC ACM serial (benchmark output, the pedal's rig link) |
| `embassy-sync` | 0.7 | Async `Mutex` (the pedal's shared flash driver) |
| `stm32-fmc` | 0.4 | SDRAM controller (AS4C16M32MSA-6 device definition) |
| `cortex-m` | 0.7 | Low-level Cortex-M access — DWT, SCB, MPU |
| `cortex-m-rt` | 0.7 | Runtime — vector table, entry point |
| `embedded-alloc` | 0.6 | Heap allocator (`LlffHeap`; SDRAM in the pedal, D2 SRAM in `heap_test`) |
| `critical-section` | 1 | Critical-section implementation (single-core) |
| `libm` | 0.2 | `no_std` float math |
| `grounded` | 0.2 | DMA buffer management (GroundedArrayCell) |
| `static_cell` | 2 | Safe static initialization for USB buffers |
| `sonido-core`, `sonido-effects`, `sonido-registry`, `sonido-patch` | path | DSP, kernels, the effect registry and UIDs, rig format + `PatchPlayer` + pedal state + USB link (`default-features = false`) |
| `sonido-platform` | path, optional | Knob mapping, cycle table, `PlatformController` (feature `platform`) |
| `sonido-rigs` | path, build-dependency | Factory rigs, encoded into the image by `build.rs` |
| `defmt` | 1.0 | Efficient embedded logging |
| `defmt-rtt` | 1.1 | RTT transport for defmt |
| `panic-probe` | 1.0 | Panic handler with defmt output |

---

## Getting Started

### Prerequisites

1. **Micro USB data cable** — NOT charge-only.
   Data cables have 4 wires (power + D+/D-); charge-only cables have 2.

2. **Linux udev rule** for DFU access without sudo:

   ```bash
   sudo tee /etc/udev/rules.d/50-daisy-stm-dfu.rules << 'EOF'
   SUBSYSTEMS=="usb", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="df11", TAG+="uaccess"
   EOF
   ```

   `uaccess` grants the logged-in user access. Don't add `GROUP="plugdev"`:
   distributions without that group (Fedora) make current systemd-udev skip
   the whole rule line. The same applies to probe-rs's published
   `69-probe-rs.rules`, which names `plugdev` on every line; on Fedora, strip
   it after installing: `sudo sed -i 's/, GROUP="plugdev"//' /etc/udev/rules.d/69-probe-rs.rules`.

   ```bash
   sudo udevadm control --reload-rules
   sudo udevadm trigger
   ```

3. **dfu-util** for command-line flashing:

   ```bash
   sudo apt install dfu-util   # Debian/Ubuntu
   sudo dnf install dfu-util   # Fedora
   ```

4. **Rust embedded target:**

   ```bash
   rustup target add thumbv7em-none-eabihf
   ```

5. **cargo-binutils** (provides `cargo objcopy` for creating flashable binaries):

   ```bash
   cargo install cargo-binutils
   rustup component add llvm-tools
   ```

6. **probe-rs** *(optional — only needed for defmt RTT debug output via SWD probe):*

   ```bash
   cargo install probe-rs-tools --locked
   ```

### Phase 1: Validate Hardware

*Bare Seed + USB. No probe required.*

#### Bootloader Behavior

The Electrosmith bootloader lives in the STM32's internal flash (128 KB). On every
power-on or reset, it runs for a **2.5-second grace period**:

- **LED pulses sinusoidally** — bootloader is alive and listening for DFU/media
- **BOOT pressed during the grace period extends it** — keeps listening (acknowledged by rapid blinks)
- After grace period, bootloader jumps to user program (if one is stored in QSPI)
- **No program stored** — stays in grace period indefinitely until DFU flash
- **SOS blink pattern** (3 short, 3 long, 3 short) — invalid binary detected

To enter DFU mode for flashing:

1. **Press RESET** — LED pulses sinusoidally for 2.5 seconds (grace period)
2. **Run `dfu-util`** within the grace period
3. That's it — the bootloader accepts DFU transfers during the grace period

If you need more time (e.g., typing the command), tap **RESET** first, then
press **BOOT** while the LED is pulsing to extend the grace period.

> **Don't hold BOOT through RESET.** Observed on a Seed with bootloader v6.4
> (2026-10-22): BOOT held while RESET is released starts the STM32's **ROM**
> bootloader instead — still USB DFU `0483:df11`, but `dfu-util -l` lists only
> "Internal Flash" and "Option Bytes", no QSPI region, and a QSPI flash fails
> with "Last page ... is not writeable" (nothing is written). A plain RESET
> starts the Electrosmith bootloader, whose DFU lists
> `@Flash /0x90000000/...` — check for that before flashing. The ROM mode is
> useful only for (re)installing a bootloader into internal flash.

Verify DFU is active before flashing:

```bash
lsusb | grep "0483:df11"
# Should show: STMicroelectronics STM Device in DFU Mode
```

> **First-time Daisy:** check which bootloader you have. With the Electrosmith
> bootloader, RESET makes the LED pulse slowly (the grace period). Some Seeds
> arrive with only a blink demo in internal flash and no Electrosmith
> bootloader (one did in October 2026): the LED blinks, and RESET never pulses;
> BOOT held through RESET brings up the STM32's ROM bootloader (`dfu-util -l` lists only "Internal Flash"
> and "Option Bytes", no QSPI). Install the Electrosmith bootloader with the
> browser flasher (Option A, "Flash Latest Bootloader"), or skip it for
> development: an SWD probe loads any example straight into RAM (Phase 2).

#### Option A — Browser flash (fastest, no Rust needed)

1. Enter DFU mode:
   **press RESET** (then press **BOOT** while the LED pulses, for a longer window)
2. Verify DFU detection:
   ```bash
   lsusb | grep "0483:df11"
   ```
   Should show: `STMicroelectronics STM Device in DFU Mode`
3. Open [flash.daisy.audio](https://flash.daisy.audio/) in **Chrome** (WebUSB)
4. Click **Connect** → select **DFU in FS Mode** → **Flash Blink**
5. LED blinks = hardware works

   > **What you see:** Steady on/off blink (~1 Hz). This is the factory blink
   > program, not the bootloader pulse. If you see this, your hardware is good.

#### Option B — Sonido blinky (validates full toolchain + BOOT_SRAM)

All examples use **BOOT_SRAM** mode: the Electrosmith bootloader copies firmware
from QSPI flash to AXI SRAM on each boot. Code executes from zero-wait-state
SRAM, allowing Embassy to safely reconfigure clocks.

Build from the crate directory (picks up `.cargo/config.toml` target):

```bash
cd crates/sonido-daisy
cargo objcopy --example blinky_bare --release -- -O binary blinky.bin
```

Press RESET, then flash to QSPI within the 2.5s grace period (bootloader copies to SRAM on boot):

```bash
dfu-util -a 0 -s 0x90040000:leave -D blinky.bin
```

LED blinks = BOOT_SRAM path + toolchain + flash all working.

> **What you see:** The `dfu-util` output ends with something like:
> ```
> Downloading element to address = 0x90040000, size = XXXX
> Download done.
> File downloaded successfully
> dfu-util: Error during download get_status
> ```
> The "Error during download get_status" is **normal** — the `:leave` flag
> causes the device to reset out of DFU mode. After reset, the bootloader
> copies the binary from QSPI to SRAM and jumps. LED blinks = success.

For Embassy runtime validation (async timer, GPIO HAL):

```bash
cargo objcopy --example blinky --release -- -O binary blinky.bin
dfu-util -a 0 -s 0x90040000:leave -D blinky.bin
```

### Phase 2: Kernel Benchmarks

*Bare Seed + USB. No probe required — results output via USB serial.*

Flash via DFU:

```bash
cd crates/sonido-daisy
cargo objcopy --example bench_kernels --release -- -O binary bench.bin
dfu-util -a 0 -s 0x90040000:leave -D bench.bin
```

> **dfu-util output:** Same as Phase 1 — "Error during download get_status" is normal.

After flashing, the Daisy resets, runs the benchmarks, then enumerates
as a USB serial device (CDC ACM, VID:PID `1209:0001`). You may need a udev rule for non-root access:

```bash
# If /dev/ttyACM0 shows "permission denied":
sudo tee /etc/udev/rules.d/50-daisy-cdc.rules << 'EOF'
SUBSYSTEMS=="usb", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", TAG+="uaccess"
EOF
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Read results with any terminal:

```bash
cat /dev/ttyACM0
# or: screen /dev/ttyACM0 115200
```

Output repeats every 5 seconds so you can connect at any time:

```
=== Sonido Kernel Benchmarks ===
sample_rate=48000 block=32 warmup=8 timed=128 budget=320000cyc/block @480MHz
  kernel                mean_cyc  mean%   worst_cyc  worst%
  preamp                   10641    3.32       10663    3.33
  ...
=== End ===
```

Each line is one kernel (all 36), heap-allocated in SDRAM as on the pedal,
warmed up for 8 blocks, then timed over 128 32-sample stereo blocks of a 220 Hz
sine: the mean block and the worst block, against the 320,000-cycle budget.
Results from a Seed are in [Benchmarks](BENCHMARKS.md#cortex-m7-measured-on-a-daisy-seed).

> **Device not appearing?** After flashing, the Daisy has to finish the
> benchmarks before it initializes USB. Check with `dmesg | tail` — you should see
> `cdc_acm` and a `/dev/ttyACM*` assignment. If nothing appears, unplug and
> replug USB (the board resets on reconnect).

With an SWD probe (ST-Link V3 Mini, ~$12) on the Seed's SWD header, no
bootloader or DFU is needed: probe-rs loads the example straight into AXI SRAM
(the same placement BOOT_SRAM gives it), starts it, and prints the defmt log,
results included. Nothing is written to flash; a power cycle brings back
whatever was there.

```bash
cargo run --example bench_kernels --release
# = probe-rs run --chip STM32H750IBKx target/thumbv7em-none-eabihf/release/examples/bench_kernels
```

The defmt log also carries a second pass the USB table does not: each effect on
the **rig path** (as `EffectRegistry::create` builds it, with the pedal's control
tick), one `rig=<name> mean=… worst=…` line per effect. Those worst blocks are the
pedal's cycle table; save the log and regenerate the table with
`scripts/update_cycle_table.py` ([Benchmarks](BENCHMARKS.md#updating-the-cycle-table)).

`probe-rs read --chip STM32H750IBKx b32 0x5C001000 1` checks the wiring: it
reads the debug ID code (`0x20036450` on an H750, revision V).

### Phase 3: Audio Passthrough

*Requires audio I/O — Hothouse carrier board or breadboard wiring to SAI pins.*

`examples/passthrough.rs` copies audio input directly to output with no
processing. Validates the full audio path: codec ADC → SAI RX → DMA → CPU →
DMA → SAI TX → codec DAC. The binary uses `output.copy_from_slice(input)` — no
format conversion needed.

`examples/passthrough_blink.rs` adds the heartbeat LED task on top of
passthrough — confirms audio + Embassy multitasking coexist.

```bash
cd crates/sonido-daisy
cargo objcopy --example passthrough --release -- -O binary -R .sram1_bss passthrough.bin
dfu-util -a 0 -s 0x90040000:leave -D passthrough.bin
```

### Phase 4: Single Effect

*Requires Hothouse — uses knobs, toggle, footswitch, LED.*

`examples/single_effect.rs` uses `EffectSlot` — the all-in-one wrapper that
bundles click-free bypass (`BypassCrossfade`), NaN/Inf sanitization, and
hardware knob mapping (`apply_knobs()`, all 6 knobs through the effect's
`knob_map`). It runs distortion by default; change the three `CHANGE THESE` lines
(kernel import, `TestKernel` alias, `EFFECT_ID`) to test another effect.
Footswitch 1 toggles bypass on press; LED 1 shows active/bypassed. The toggles are
unused.

```rust
// EffectSlot pattern (single_effect.rs)
let mut slot = EffectSlot::new(Box::new(effect), EFFECT_ID, SAMPLE_RATE);
slot.apply_knobs(&knob_vals);                 // biased ADC→param, noon = default
let (l, r) = slot.process_stereo(in_l, in_r); // effect → NaN guard → bypass ramp
```

`sonido_pedal.rs` (Tier 5) uses the standalone utilities — `BypassCrossfade`
and `sanitize_stereo` — around its `PatchPlayer`, since the player's graph owns
the effects rather than individual `EffectSlot` wrappers.

```bash
cd crates/sonido-daisy
cargo objcopy --example single_effect --release --features alloc,platform -- -O binary -R .sram1_bss single_effect.bin
dfu-util -a 0 -s 0x90040000:leave -D single_effect.bin
```

### Diagnostics

Audio output and input diagnostic tools for isolating hardware issues on the
Hothouse. These are not numbered tiers — use them when the audio path is
misbehaving and you need to determine whether the problem is digital, analog,
input-side, or output-side.

| Example | What It Tests | Output |
|---------|---------------|--------|
| `tone_out.rs` | DAC → analog output path | 440 Hz sine wave to output jack |
| `square_out.rs` | DAC → analog output path (max amplitude) | 1 kHz full-scale square wave to output jack |
| `hothouse_diag.rs` | Audio passthrough + input levels + 6 knobs + GPIO (FS, toggles) | `AUDIO in=-46.8dBFS ... \| K1=... \| FS1=... T3=...` via USB serial every 2 seconds |

#### Build & Flash

All diagnostics follow the same pattern. From `crates/sonido-daisy/`:

```bash
cd crates/sonido-daisy
cargo objcopy --example <name> --release -- -O binary -R .sram1_bss <name>.bin
dfu-util -a 0 -s 0x90040000:leave -D <name>.bin
```

For example, to flash `hothouse_diag`:

```bash
cargo objcopy --example hothouse_diag --release -- -O binary -R .sram1_bss hothouse_diag.bin
dfu-util -a 0 -s 0x90040000:leave -D hothouse_diag.bin
```

`hothouse_diag` reports over USB serial after flashing:

```bash
cat /dev/ttyACM0
# or: screen /dev/ttyACM0 115200
```

Output repeats every 2 seconds:

```
AUDIO in=-46.8dBFS rms=0.0045 peak=0.0078 | K1=0.51 K2=0.00 K3=1.00 K4=0.25 K5=0.75 K6=0.33 | FS1=OFF FS2=OFF T1=MID T2=MID T3=UP
```

#### Diagnostic Test Sequence

When debugging audio issues on the Hothouse, run these in order:

1. **Flash `tone_out`** — plug headphones or an amp into the output jack and
   listen for a 440 Hz sine tone. The onboard user LED (PC7) shows the
   `heartbeat` double blink once per second to confirm firmware
   is running. If the LED doesn't blink, the firmware didn't flash correctly.
   - **Hear 440 Hz clearly** → DAC and output path work. Skip to step 3.
   - **Hear 440 Hz underneath noise** → DAC works, analog stage is oscillating
     on top of it. Output op-amp issue.
   - **Hear only noise, no tone** → proceed to step 2.

2. **Flash `square_out`** — outputs the loudest possible digital signal
   (1 kHz square wave at ±0.95). Same heartbeat on the user LED.
   If you cannot hear this through the analog noise, the DAC output is
   effectively disconnected from the output jack — the analog circuit is
   completely overriding it.

3. **Flash `hothouse_diag`** — with nothing plugged into the input jack,
   read the USB serial output and check the audio RMS level.
   - **High RMS with nothing plugged in** → noise is being injected before the
     codec ADC (input op-amp oscillation, ground loop, or power supply issue).
   - **Low RMS with nothing plugged in** → input side is clean; the problem is
     output-only.
   - **RMS changes when a cable/instrument is plugged in** → codec ADC is
     reading real signal. The analog input path works.
   - **K1–K6 values respond to knob turns** → ADC and control path work.

### Build & Flash Reference

All commands run from `crates/sonido-daisy/` (picks up `.cargo/config.toml`).

**Build** (any example; add `--features alloc` or `--features alloc,platform`
where the example requires it — see [Tier System](#tier-system)):

```bash
cargo build --example <name> --release
```

**Flash via DFU** (USB, no probe — BOOT_SRAM). Audio examples also pass
`-R .sram1_bss` so the NOLOAD DMA-buffer section is not written into the binary:

```bash
cargo objcopy --example <name> --release -- -O binary -R .sram1_bss <name>.bin
dfu-util -a 0 -s 0x90040000:leave -D <name>.bin
```

**Flash via SWD probe** (supports defmt RTT):

```bash
cargo run --example <name> --release
```

> **Power:** USB alone is sufficient for development. External VIN (5–17V DC)
> only needed inside the Hothouse enclosure.

---

## The Pedal (Rig Player)

The pedal firmware (`examples/sonido_pedal.rs`) is a rig player. Every sound it
makes is a rig, an ordinary `sonido_patch::Patch`, played by
`sonido_patch::PatchPlayer`, the runtime the graph-player plugin uses, with
effects built by `EffectRegistry::create`, the constructor the plugins and the
editor use. A rig's parameters are therefore smoothed (`Adapter<K, SmoothedPolicy>`)
and it sounds the same on the pedal as in a DAW.

### Architecture Overview

```
  QSPI flash                 rig list (wraps)                  firmware image
  state sector ──→ ┌──────┬────────────────┬──────────────┐ ←── factory rigs
  rig bank ──────→ │ Jam  │ bank rigs 1–15 │ factory rigs │     (build.rs)
  USB rig link ──→ └──────┴───────┬────────┴──────────────┘
                                  │ load: read, check_rig, build
                                  ▼
   ┌─────────┐  macros / jam    ┌───────────────────────────────────────┐
   │ ADC × 6 │  edits / A·B·    │ PatchPlayer                           │
   │ Toggles │ ───────────────→ │  graph of registry-built effects      │
   │ Footsw  │  morph position  │  Input → … → Output                   │
   └─────────┘                  └───────────────────────────────────────┘
```

The input is summed to mono (`(L + R) / 2`) and fed to both sides of the player.

### The Rig List

The pedal powers on into the rig it last played (hold FS1 while powering on to
start in the jam). The list wraps around, in this order:

1. **Jam** — three slots you fill on the pedal from the eight `JAM_EFFECT_IDS`,
   routed by T3. It is saved to flash as you play.
2. **Bank rigs** — every slot of the QSPI rig bank (15) that holds a decodable rig,
   sent over the [USB rig link](#usb-rig-link) from the editor or `sonido pedal`.
3. **Factory rigs** — the six rigs of `sonido_rigs::factory_rigs()`, compiled into
   the image (`src/rigs.rs`, see [Factory Rigs](#factory-rigs)).

Hold a footswitch to step through the list (FS1 back, FS2 forward), in any T2
position; LED 1 then blinks the rig's position (the jam is 1).

### Controls

`sonido pedal controls` prints this whole section as a card, drawn from the code
that runs it: what every footswitch press, toggle, knob page, LED and power-on
gesture does lives in `crates/sonido-patch/src/controls.rs`, and the firmware's
control tick (`control_tick()` in `sonido_pedal.rs`) dispatches through those
functions, so the card can't drift from the pedal. The controls follow one rule for the
footswitches, in every rig and every T2 position: the left one is on/off, the right
one is time, and holding either changes rig. And one rule for the knobs: the top
row (K1 K2 K3) is levels, with K3 (top right) always a volume, and the bottom row
(K4 K5 K6) is the sound ([ADR-032](DESIGN_DECISIONS.md#adr-032-one-control-rule-for-the-pedal)).

| Control | Everywhere |
|---------|------------|
| FS1 tap | On/off: bypass crossfades to the dry input (5 ms) while the rig keeps running |
| FS1 hold | Previous rig, at 0.4 s, while still held |
| FS2 tap | Tap tempo (LED 2 flashes it; in the jam the delay's time follows it). With T2 in the middle: glide to the other side of the morph; a tap mid-glide turns back |
| FS2 hold | Next rig, at 0.4 s, while still held |
| T2 | Up A, middle Morph, down B |

| Control | Jam | Rig |
|---------|-----|-----|
| T1 | Which slot the knobs edit (1 / 2 / 3) | — |
| T3 | Routing: Linear / Parallel / Fan, read when the jam loads | — |
| Knobs, T2 up or down | The T1 slot's A or B sound through the effect's `knob_map`: K1 its extra control, K2 mix, K3 output, K4–K6 its three main controls; a knob takes over once moved | The rig's six macros |
| Knobs, T2 middle | The setup page: K1 input, K2 morph time, K3 master; K4 K5 K6 pick slot 1, 2, 3's effect | The rig's six macros (factory rigs: K1 input, K2 morph time, K3 master, K4–K6 the rig's own controls) |

A press shorter than `HOLD_TICKS` (40 control polls at 100 Hz, 0.4 s) is a tap and
acts when the footswitch is released; a hold acts as soon as it reaches 0.4 s. The
tapped tempo (120 BPM until the first tap) applies to every rig as its
`TempoContext` and is saved with the state.

At power-on, hold FS1 to start in the jam, or both footswitches to start the USB
bootloader for a firmware update ([Bypass and Bootloader](#bypass-and-bootloader)).
During play the footswitches hold nothing dangerous: there is no bootloader hold,
no slot clear and no both-footswitch gesture.

### Toggle Mapping

| Toggle | UP (0) | MID (1) | DOWN (2) |
|--------|--------|---------|----------|
| **T1** | Jam slot 1 | Jam slot 2 | Jam slot 3 |
| **T2** | **A**: hear/edit A | **Morph**: FS2 taps glide between A and B; the jam's setup page | **B**: hear/edit B |
| **T3** | **Linear**: 1 → 2 → 3 | **Parallel**: split → [1,2,3] → merge | **Fan**: 1 → split → [2,3] → merge |

T1 and T3 only act on the jam; T3 is the jam's routing, read when the jam loads, and
moving it while the jam plays rebuilds it. In a rig, T1 and T3 do nothing
(assignable toggles in rigs are future work). Empty slots are skipped when wiring
(`sonido_patch::pedal::jam_patch`), so with one populated slot every topology is a
plain chain. T2 sets the morph position of whatever plays: A = 0, B = 1, Morph =
wherever the glides leave it. Moving T2 into the middle starts the morph from the
side it came from (from B at 1, otherwise at 0).

### The Jam

**The setup page.** With T2 in the middle the knobs are the jam's setup page
(`setup_page()`): K1 input (−12..+12 dB), K2 morph time (0.2–5 s for a full glide
between A and B) and K3 master (−60..+12 dB), each with its default at noon (0 dB,
1 s, 0 dB); K4, K5 and K6 pick the effect in slots 1, 2 and 3. Input, master and
morph time are the jam's `JamSettings` (`crates/sonido-patch/src/pedal.rs`):
`jam_patch()` carries them in the patch's globals and morph speed, so they apply in
every T2 position and are saved with the jam in the state sector.

**Picking effects.** A pick knob's travel is nine equal zones: fully left is an
empty slot, then `JAM_EFFECT_IDS` in order (distortion, bitcrusher, filter, ringmod,
chorus, phaser, delay, reverb). A knob near a zone's edge stays in its zone until it
is 15% of a zone past it (`pick_zone`), and a pick loads only once the knob has
rested in its zone for 0.3 s (`PICK_SETTLE_TICKS` = 30 polls), so sweeping past
effects does not build each one. A newly picked effect starts at its voiced defaults
(what noon gives on every knob), with A and B equal. A pick that would take the jam
past the pedal's CPU rule (`cycle_table::chain_fits`, see
[Chain Configurations](#chain-configurations)) is refused, and LED 1 blinks 4 (over
budget); with a reverb loaded that rules out a second reverb, and next to reverb and
distortion only filter, ringmod or bitcrusher fit.

**A/B editing.** With T2 up the knobs write the focused (T1) slot's A state, with T2
down its B state, through the effect's `knob_map` and `knob_to_param` (noon = the
descriptor default, see [Biased Knob Mapping](#biased-knob-mapping-noon--default)):
K1 the effect's extra control, K2 its mix, K3 its output level, and K4–K6 its three
main controls ([Jam Effect List](#jam-effect-list-and-knob-mapping)). The first move
out of A seeds each slot's B as a copy of A, so a slot whose B was never edited does
not move during a morph.

**Touch takeover.** After a slot, mode or rig change, a knob edits again only once
it has moved 2% of its travel (`TOUCH_TRAVEL`); untouched knobs leave the stored
values alone. The setup page's knobs, pick knobs included, work the same way. There
is no display to show where a stored value sits, so waiting for a knob to sweep past
it (pickup) left knobs dead.

**Tap tempo.** An FS2 tap with T2 up or down sets the tempo (`tap_tempo()`): every
rig gets it as its `TempoContext`, and in the jam each slot holding the delay gets
one beat as its time (60,000 / BPM ms, within the time parameter's range), in A and
B. A knob resting on the delay's time takes over again only once moved.

**Morph.** With T2 in the middle, an FS2 tap glides the morph position to the other
side (toward B from below the middle, toward A from above it), taking the jam's morph
time for a full sweep, and stops there; a tap mid-glide turns it back. The player
interpolates every parameter per control tick by its descriptor (frequency
logarithmic, STEPPED snapped at the midpoint, else linear).

The jam is an ordinary patch: `jam_patch()` turns the slots (each a `JamSlot`:
effect UID, A and B parameter values) into one, and it plays through the same
`PatchPlayer` as every other rig.

### Rigs

On a rig, K1–K6 drive the rig's six macros (`PatchPlayer::set_macro`) in every T2
position, and an FS2 tap with T2 in the middle glides its A/B morph at the rig's own
morph speed. Every factory rig maps its macros by the pedal's knob rule: K1 input
gain (±12 dB, 0 at noon), K2 morph time (0.2 s, 1 s at noon, 5 s), K3 master volume
(±12 dB around the rig's level-matching trim), and K4–K6 the rig's own three
controls (see the crate docs in `crates/sonido-rigs/src/lib.rs`). The A/B morph moves
the parameters the macros leave alone, so a knob never fights the morph.

#### Factory Rigs

Defined once in `crates/sonido-rigs/src/lib.rs` and shared with the graph-player
plugin's rig library. The CPU column is the rig's estimate under the pedal's rule
(`cycle_table::chain_cycles` against the 320,000-cycle block; the limit is 80%):

| # | Rig | Signal path | K4 | K5 | K6 | CPU estimate |
|--:|-----|-------------|----|----|----|-----:|
| 1 | Shimmer Cathedral | pitch_shift → plate_reverb, beside delay | Shimmer (octave mix) | Wash (plate output) | Echo (delay mix) | 44% |
| 2 | Fuzz Lab | distortion → filter, beside bitcrusher → ringmod; into tape | Fuzz (drive) | Filter (cutoff) | Crush (crusher mix) | 57% |
| 3 | Space Echo | preamp → tape → delay → spring_reverb | Echo (delay mix) | Spring (spring mix) | Grit (preamp gain) | 59% |
| 4 | Granular Drift | glitch, beside time_stretch → texture; into plate_reverb | Cloud (texture output) | Stretch (time-stretch output) | Stutter (glitch mix) | 48% |
| 5 | Harmonic Habitat | harmonic_habitat → chorus → delay | Habitat (mix) | Chorus (mix) | Echo (delay mix) | 56% |
| 6 | Amp Stack | gate → amp → cabinet | Bass | Mid | Treble | 77% |

Each rig's master trim (`crates/sonido-rigs/src/trims.rs`) is generated by
`cargo run -p sonido-rigs --example calibrate --release`, which plays the
reference guitar through the rig with every knob at noon, halfway through its
morph, and sets the master so the rig plays at the dry guitar's loudness.
`crates/sonido-rigs/tests/factory.rs` holds every rig to that (±0.5 dB at morph
0.5, ±3 dB at A and at B), to the pedal's CPU and memory rule
(`validate_for_daisy`), and to a round trip through the
4 KB binary format with all six macros named. The firmware's `build.rs` encodes
them into the image, so the pedal ships the bytes, not the code that builds them.

### Bypass and Bootloader

An FS1 tap toggles bypass, in every rig and T2 position. Bypass crossfades to the
dry input over 5 ms (`BypassCrossfade`) while the rig keeps running, so turning it
back on picks up where the input is, without a burst of stale buffers.

The USB bootloader (DFU) for a firmware update is never a gesture during play. Hold
both footswitches while powering the pedal on (keep them down until the firmware
starts, after the Daisy bootloader's 2.5 s grace period), or send the rig link's
`Bootloader` request (`sonido pedal update`). Either way the firmware writes the
Daisy bootloader's "stay in DFU" magic (`0xB0074EFA`) to backup SRAM and resets
(`enter_daisy_bootloader()`), so the bootloader waits in DFU and the next firmware
can be flashed without opening the enclosure:

```bash
sonido pedal update sonido_pedal.bin   # bootloader request, then dfu-util
sonido pedal update                    # bootloader only; prints the dfu-util command
```

`sonido pedal update` flashes through `sonido_patch::dfu`, which writes firmware to
`0x90040000` and accepts only the Daisy bootloader (it lists the QSPI region
`0x90000000`); the STM32 ROM bootloader (BOOT held through RESET) has the same USB id
but cannot reach QSPI flash, and is refused. Rigs never go over DFU: they go over
the [USB rig link](#usb-rig-link), which checks them first.

### Loading a Rig

A rig change (footswitch hold, a jam pick or T3, USB) is a request the
`rebuild_task` picks up within 5 ms. Loading (`load()`):

1. **Check first.** A bank rig is read from flash and decoded, a factory rig
   decoded from the image, and either is checked with `patch_loader::check_rig`
   (every effect available, routing valid, CPU within `CHAIN_FRACTION`, SDRAM
   within budget) *before* the old rig stops. A refused rig leaves the current
   one playing, and LED 1 blinks the reason (`error_blink_code`): 2 = empty or
   corrupt slot, 3 = an effect this firmware lacks, 4 = too heavy for the CPU or
   memory, 5 = routing that does not build.
2. **Ramp to dry.** `UPDATING` makes the audio callback crossfade to the dry input
   and leave the player alone; the task waits 8 ms for the ramp.
3. **Build.** Each node's effect is built in turn, yielding to the executor between
   them so audio keeps up. A jam rebuild reuses the effects of slots that did not
   change, tails included.
4. **Start at the values.** The new player gets the morph position from T2, the
   macros from the knobs (rigs), the tempo, and then `apply_controls_now()`, which
   snaps every parameter to its value (`Effect::snap_params`) instead of sweeping
   there from the effects' defaults.
5. `UPDATING` clears and the callback ramps back to the rig. A rig whose graph
   does not build falls back to the jam.

On success LED 1 blinks the rig's position, and the active rig is marked for the
next state save.

### Real-Time Hardening

The audio task runs on its own `InterruptExecutor` (on the spare UART4
interrupt, priority 6), above the thread-mode executor that runs every other
task (rig loading, flash, USB, state saves, the load report). It preempts them
at any instruction, so however long they run, audio keeps its blocks: a rig
load plays dry until the new rig is ready. Measured before this, with one
executor for everything, building a factory rig took up to 12 ms (Granular
Drift's graph) against about 0.6 ms of slack, and every rig change dropped
audio three or four times. Other tasks touch the pedal's state through
`shared()`, a few-microsecond critical section, so the callback never sees it
half-changed. On top of that:

- **Flash reads yield per page.** `QspiFlash::read` transfers 256 bytes (about
  80 µs) at a time and yields between pages; `write_sector` waits out erase and
  program with timers and verifies page by page. One 4 KB read used to block for
  1.3 ms and dropped audio on every state save.
- **Table-driven CRC32** (`crates/sonido-patch/src/binary.rs`), for the rig and
  state sectors and the USB frames.
- **Rigs snap, not sweep.** A freshly built rig used to smooth every parameter
  from its default, redesigning every coefficient per sample at once: a 116%
  peak at boot. `Effect::snap_params` (`crates/sonido-core/src/effect.rs`) and
  `PatchPlayer::apply_controls_now` (`crates/sonido-patch/src/runtime.rs`) start
  it at its values.
- **The boot rig is built before the codec starts**, so building it (milliseconds
  of work) never competes with running audio.
- **Rig changes ramp to dry** (above).
- **No heavy work in an audio block.** The cabinet built its impulse response
  twice, the second time in its first audio block: 12,288 software `sin`/`cos`
  calls, about 4 ms. Its normalization now rotates a phasor, and the kernel
  remembers the IR it was built with.
- **Knob hysteresis.** `ControlBuffer` holds each knob's value until the reading
  leaves a 0.15%-of-travel band (`KNOB_HYSTERESIS` in `src/controls.rs`), so an
  untouched knob reads exactly the same every poll and the parameters it drives
  settle; a jittering knob kept a reverb rig about 8 points busier.

Measured on hardware on 2026-10-22, after these fixes and the control redesign:
the jam distortion → reverb → ringmod ran at 74–75% mean and 84–85% peak, with 0
blocks over budget; two rounds through every factory rig and the jam, plus a rig
written to and erased from the bank over USB, gave 0 codec overruns (worst block
91.4%). The firmware image is 477,944 bytes of the 491,520-byte (480 KB) BOOT_SRAM
limit; CI fails a build that doesn't fit.

### Flash: Rig Bank and State Sector

The last 64 KB of the 8 MB QSPI flash belongs to the pedal (`src/qspi_flash.rs`):

| Region | Flash address | Contents |
|--------|---------------|----------|
| Rig bank, slots 0–14 (`PATCH_SLOT_COUNT` = 15) | `0x007F_0000` + n × 4096 (`patch_slot_addr`) | One encoded `sonido-patch` rig per 4 KB sector |
| State sector (`STATE_SECTOR_ADDR`) | `0x007F_F000` | `PedalState` |

`QspiFlash::write_sector` refuses any address outside this region
(`FlashError::OutsideBank`), so the firmware never erases or programs its own
image. At boot the pedal reads the state sector and scans the bank for slots
that decode.

`PedalState` (`crates/sonido-patch/src/pedal.rs`) holds the active rig, the jam's
topology, its three slots (effect UID, A and B values), its `JamSettings` (input,
master and morph time, carried in the jam patch's globals and morph speed) and the
tapped BPM, behind a `SPS1` magic and a CRC32. `save_task` writes it once nothing has changed for
2 s (`SAVE_QUIET_MS`), checking every 500 ms, so playing never waits on flash
and the pedal comes back as you left it.

`examples/qspi_test.rs` is the hardware check for this path (JEDEC id, write
guard, a write/verify/erase of the state sector, slot 0 decode); run it from RAM
with a probe:

```bash
cd crates/sonido-daisy
cargo run --release --example qspi_test --features alloc
```

### USB Rig Link

While it plays, the Seed's USB port is a CDC-ACM serial device (`1209:0001`,
product "Sonido Pedal") speaking `sonido_patch::link`
(`crates/sonido-patch/src/link.rs`): the host sends one request frame and reads
one reply frame. A frame is the sync bytes `"SL"`, a kind byte, an argument byte,
a little-endian payload length (at most one 4 KB sector), the payload, and a
CRC32 of everything after the sync. Rigs travel as the same 4 KB sectors the bank
stores.

| Request | Argument | Reply |
|---------|----------|-------|
| `Hello` | — | `Info`: protocol version, slot and factory-rig counts, active rig, effect UIDs, firmware name |
| `ReadSlot` | slot | `Sector` (an erased slot reads all `0xFF`) |
| `WriteSlot` | slot | `Ok` once the sector decodes, passes `check_rig`, is written and verified; the pedal then plays it |
| `EraseSlot` | slot | `Ok` (if the erased slot was playing, the pedal returns to the jam) |
| `Activate` | rig byte (0 = jam, 1–15 = bank slot + 1, `0x80` + n = factory rig n) | `Ok` |
| `ReadJam` | — | `Sector`: the jam as a rig |
| `Status` | — | `Status`: active rig, bypass, morph position, last second's load mean and peak |
| `Bootloader` | — | `Ok`; the pedal then restarts into the Daisy bootloader (USB DFU) for a firmware update |

Refusals reply `Error` with an `ErrorCode`: `BadSlot`, `NotARig`,
`UnknownEffect`, `OverBudget`, `Flash`, `Unsupported`, `BadRig`.

The host side is `sonido_patch::client` (`crates/sonido-patch/src/client.rs`):
`LinkClient`, generic over any `Read + Write` stream, sends one request and waits
for its reply. `LinkClient::send_rig` stores a rig the way the editor and the CLI
send rigs: over the bank rig with the same name, otherwise in the first empty slot;
the pedal then plays it. `open_pedal` and `pedal_ports` (feature `serial`) find the
pedal by its USB id `1209:0001`, and `enter_bootloader` sends `Bootloader`. Before
sending, the editor and `sonido pedal push` check the rig with
`sonido_patch::validate::validate_for_daisy`, the rule the pedal applies again. From
the command line:

```bash
sonido pedal list                     # the rig list, in footswitch order
sonido pedal push my_rig.json         # replace the rig with that name, or take the first empty slot
sonido pedal pull jam -o jam.json     # the jam as editor JSON
sonido pedal play "Space Echo"        # jam, a bank slot 1–15, or a factory rig by name
```

See [CLI_GUIDE.md](CLI_GUIDE.md#pedal) for every subcommand.

### CPU Load Meter

The pedal times every audio callback with the DWT cycle counter and logs once a
second over defmt RTT. Run it from RAM with a probe attached to see it:

```bash
cd crates/sonido-daisy
cargo build --release --example sonido_pedal --features alloc,platform
probe-rs run --chip STM32H750IBKx --protocol swd target/thumbv7em-none-eabihf/release/examples/sonido_pedal
# level in peak=<dB×10> rms=<dB×10>  out peak=<dB×10> rms=<dB×10> (tenths)
# load mean=<m>% peak=<p>% over_budget=<n>/<blocks> rig=jam slots=[distortion, reverb, ringmod] topo=linear bypassed=false
```

`mean` and `peak` are the average and the slowest callback of that second against
the 320,000-cycle block budget; `over_budget` counts callbacks that missed it. On a
rig other than the jam the line names the rig byte instead of the slots. The same
mean and peak are in the USB `Status` reply. A RAM run is lost at power-off; the
bootloader then starts whatever is in QSPI.

### LED Feedback

| LED | Meaning |
|-----|---------|
| LED1 (over FS1, on/off) | Lit while the pedal is on, dark when off. Blinks the rig's position after a rig change, or an error code for a refused rig or a refused jam pick (4: over budget) |
| LED2 (over FS2, time) | T2 up or down: a 70 ms flash on every beat of the tapped tempo (`BEAT_FLASH_S`). T2 in the middle, where FS2 glides: dark at A, lit at B, flickering while it glides. Dark when off |

Each LED shows what the footswitch below it does (`controls::led1` / `led2` in
`crates/sonido-patch/src/controls.rs`, which the firmware calls and
`sonido pedal controls` prints).

The LEDs are on/off: `hothouse_control_task` drives each GPIO from the value the
callback last wrote (threshold 0.5) at the 100 Hz control rate, too slow to dim
one. A glide always runs to an end, so the morph rests only at A or B, and
on/off shows it.

### Jam Effect List and Knob Mapping

8 effects, in the order of `JAM_EFFECT_IDS` (`crates/sonido-registry/src/lib.rs`),
which is the order of a pick knob's zones on the setup page: zone 0 (fully left) is
an empty slot, zones 1–8 these effects. Rigs and the saved jam identify effects by
stable UID (`EFFECT_UIDS`), not by this position, so the list is free to change
order:

| Zone | Effect ID | `knob_map` (K1–K6 → param index) | K1 · K2 · K3 \| K4 · K5 · K6 |
|---:|---|---|---|
| 1 | `distortion` | `[5, 4, 2, 0, 1, 3]` | dynamics · mix · output \| drive · tone · shape |
| 2 | `bitcrusher` | `[—, 3, 4, 0, 1, 2]` | — · mix · output \| bits · downsample · jitter |
| 3 | `filter` | `[—, —, 2, 0, 1, 3]` | — · — · output \| cutoff · resonance · type |
| 4 | `ringmod` | `[—, 3, 4, 0, 1, 2]` | — · mix · output \| frequency · depth · waveform |
| 5 | `chorus` | `[3, 2, 8, 0, 1, 4]` | voices · mix · output \| rate · depth · feedback |
| 6 | `phaser` | `[2, 4, 9, 0, 1, 3]` | stages · mix · output \| rate · depth · feedback |
| 7 | `delay` | `[3, 2, 9, 0, 1, 4]` | ping-pong · mix · output \| time · feedback · feedback LP |
| 8 | `reverb` | `[3, 4, 7, 0, 1, 2]` | pre-delay · mix · output \| room size · decay · damping |

— = `NULL_KNOB` (knob unused). The Hothouse's knobs are two rows, and every map
follows the pedal's knob rule:

| Row | Knobs | Always means |
|-----|-------|-------------|
| Top: levels and extras | K1 | A secondary control (stages, voices, ping-pong, pre-delay, …) |
| | K2 | The wet/dry mix, when the effect has one |
| | K3 | A volume: the effect's output level (the master on the setup page and in the factory rigs) |
| Bottom: the sound | K4 K5 K6 | The three controls that shape the effect most; K4 is always its main control (param 0) |

The maps live in `knob_map()` in `crates/sonido-platform/src/knob_mapping.rs`,
keyed by registry ID (it also maps 7 effects that are not in the jam).
`knob_to_param()` in the same file turns a 0–1 knob reading into a parameter
value on the biased curve:

```rust
pub const NULL_KNOB: u8 = 0xFF; // sentinel for unmapped knobs

pub fn knob_map(effect_id: &str) -> Option<[u8; 6]> {
    const N: u8 = NULL_KNOB;
    match effect_id {
        //                    K1  K2  K3 | K4  K5  K6
        "filter" => Some([N, N, 2, 0, 1, 3]),     // — — out | cutoff resonance type
        "chorus" => Some([3, 2, 8, 0, 1, 4]),     // voices mix out | rate depth feedback
        // ... knobs[k] = param index for knob K
        _ => None,
    }
}
```

The map's unit tests hold the rule for every jam effect (`k3_is_the_level_and_the_sound_is_below`:
K3 mapped, K4 = param 0).

Jam effects are built like every rig's, by UID through the registry
(`patch_loader::make_effect`).

### Scale-Aware ADC Conversion

`sonido_platform::adc_to_param()` (`crates/sonido-platform/src/param_map.rs`) uses
`ParamDescriptor::scale` for proper knob curves; `adc_to_param_biased()` applies the
same interpolation to each half of the noon split:

```rust
fn adc_to_param(desc: &ParamDescriptor, normalized: f32) -> f32 {
    let val = interpolate_scaled(desc.min, desc.max, normalized, desc.scale);
    if desc.flags.contains(ParamFlags::STEPPED) { roundf(val) } else { val }
}
// interpolate_scaled: Linear → lerp; Logarithmic → lerp in log2 space;
// Power(exp) → min + t^exp * (max - min)
```

This gives `from_knobs()`-quality response: log sweep for frequency knobs
(cutoff, LFO rate), linear for dB/mix, power curves for custom params.

### How to Add a New Effect to the Jam

Any registered effect can already be in a rig; only the jam's pick list is
curated.

1. **Add the effect ID to `JAM_EFFECT_IDS`** in
   `crates/sonido-registry/src/lib.rs`, where its pick zone should be (the list runs
   dirt, color, motion, space). The list is not append-only: the jam is saved by
   effect UID, so reordering it moves only the pick zones. Each effect added makes
   every zone narrower (the knob's travel is one zone per effect plus empty).

2. **Give it a knob map.** If `knob_map()` in
   `crates/sonido-platform/src/knob_mapping.rs` has no entry for it, add one: map the
   6 knobs to parameter indices by checking the kernel's `KernelParams`
   implementation (the `descriptor(index)` calls in the kernel file). Use `NULL_KNOB`
   (0xFF) for unused knobs and follow the knob rule: K1 a secondary control, K2 the
   mix, K3 the output level, K4–K6 the three main controls with K4 the main one
   (param 0). Add its ID to `jam_ids()` in that file's tests, which hold every jam
   effect to the rule. Add a knob floor in `knob_min_override()` if the bottom of a
   range is unusable on guitar.

3. **Voice it at noon.** Noon is the descriptor default, so the defaults must be
   the sweet spot, level-matched by the effect's calibration (re-run
   `cargo run -p sonido-effects --example calibrate --release` after changing them).

4. **Test it on the host:**
   ```bash
   cargo test -p sonido-platform --test knob_calibration -- --nocapture
   ```
   which checks the new effect at noon and at every knob's extremes. Its
   `EFFECT_CYCLES` entry (all 36 effects are measured) decides what it can share
   the jam with.

5. **Build and verify** (the image must stay within the 480 KB BOOT_SRAM limit):
   ```bash
   cd crates/sonido-daisy
   cargo check --example sonido_pedal --release --features alloc,platform
   ```

### How to Modify Modes or Add Features

All mutable state lives in statics so the `start_callback` closure captures
nothing (a large captured future starved the executor and caused SAI overruns —
see the [v2 design note](superpowers/specs/2026-04-09-sonido-pedal-v2-design.md)):
`PLAYER` (`Option<PatchPlayer>`), `CB` (`Cb`, everything the callback keeps between
blocks: mode, morph position and glide, takeover anchors, pick-knob zones,
footswitch timers, LED blink and beat state, meters) and `PEDAL` (`Pedal`: the active rig, which
bank slots hold rigs, the jam slots and their pick cursors, topology, `JamSettings`,
BPM). They are `static mut`: the callback runs on the audio executor and uses them
freely, except while `UPDATING` is set, when the rebuild task owns them and the
callback uses only its own crossfade (`XFADE`); every other task goes through
`shared()`, a short critical section. Tasks talk to the callback through atomics (`BYPASSED`, `UPDATING`, `REQUEST`,
`BLINK`, `STATE_DIRTY`, `TO_BOOTLOADER`, the load meter), and share the flash driver through an
`embassy_sync` `Mutex` (`FLASH`).

```
Audio callback (every block, ~1500 Hz):
├── UPDATING? → ramp to dry input via the bypass crossfade, return
├── Deinterleave u24 → f32, sum to mono
├── PLAYER.process_block_stereo()  (in bypass too: the rig keeps running)
├── sanitize_stereo → bypass crossfade (to dry while BYPASSED) → f32 → u24
└── Every CONTROL_POLL_EVERY (15) blocks (~100 Hz): controls()
    ├── Toggles: T1 slot, T2 mode (first move out of A seeds B), T3 topology (jam: requests a rebuild)
    ├── Footswitches: holds (0.4 s) = previous/next rig (request);
    │   taps at release: FS1 = bypass, FS2 = tap_tempo() or, T2 middle, glide A↔B
    ├── Knobs: rig → set_macro(k); jam A/B → edit_jam_slot (knob_map + knob_to_param,
    │          touch takeover); jam T2 middle → setup_page (JamSettings, pick knobs)
    ├── Jam: input gain and master volume from JamSettings
    ├── Glide, set_morph_position(), apply_controls()
    └── LED feedback

rebuild_task (every 5 ms): REQUEST? → switch_to(rig) → load()  (see Loading a Rig)
save_task (every 500 ms): state changed and quiet for 2 s → write the state sector
link_task: USB frames → handle() → reply (after a Bootloader reply: enter_daisy_bootloader())
load_report_task (every 1 s): defmt level and load lines; Status numbers
```

Rig changes never happen inside the callback; parameter changes go through the
player (`set_corner_param` for jam edits, `set_macro` for rig knobs).

---

## Creating a New Firmware Example

Step-by-step guide for adding a new binary to `crates/sonido-daisy/examples/`.

### 1. Create the file

```bash
touch crates/sonido-daisy/examples/my_example.rs
```

### 2. Minimal template

Every example needs these elements:

```rust
//! Tier N: One-line description of what it validates.
//!
//! Detailed explanation of the example's purpose and hardware requirements.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_stm32 as hal;
use panic_probe as _;

use sonido_daisy::{ClockProfile, heartbeat, led::UserLed};

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // Clock init (Performance = 480 MHz, Efficient = 400 MHz)
    let config = sonido_daisy::rcc_config(ClockProfile::Performance);
    let p = hal::init(config);

    // Heartbeat LED (every example should have this)
    let led = UserLed::new(p.PC7);
    spawner.spawn(heartbeat(led)).unwrap();

    // Your code here...
}
```

### 3. If you need heap (delay lines, Box\<dyn Effect\>, Vec)

Add SDRAM init before any allocations:

```rust
extern crate alloc;
use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

// In main(), after hal::init():
let mut cp = unsafe { cortex_m::Peripherals::steal() };
let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
unsafe { HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE); }
```

### 4. If you need audio

Use `AudioPeripherals` → `prepare_interface()` → `start_interface()` →
`start_callback()` (as in `sonido_pedal.rs` and `single_effect.rs`):

```rust
let audio = sonido_daisy::audio::AudioPeripherals {
    codec_pins: sonido_daisy::codec_pins!(p),
    sai1: p.SAI1,
    dma1_ch0: p.DMA1_CH0,
    dma1_ch1: p.DMA1_CH1,
};
let interface = audio.prepare_interface(Default::default()).await;
let mut interface = defmt::unwrap!(interface.start_interface().await);

defmt::unwrap!(interface.start_callback(|input: &[u32], output: &mut [u32]| {
    // 64 interleaved u32s per call: deinterleave, process, reinterleave
}).await);
```

### 5. Build and flash

```bash
cd crates/sonido-daisy
cargo objcopy --example my_example --release -- -O binary -R .sram1_bss my_example.bin
dfu-util -a 0 -s 0x90040000:leave -D my_example.bin
```

### 6. Update documentation

- Add the example to the Tier table in this file (EMBEDDED.md)
- Add it to the `crates/sonido-daisy/examples/*.rs` row of `docs/DOC_CODE_MAPPING.md`
- If it needs `alloc`/`platform`, add an `[[example]]` entry with `required-features`
  to `crates/sonido-daisy/Cargo.toml`

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `lsusb` shows nothing in DFU mode | Charge-only USB cable (2 wires, no D+/D-) | Use a data cable (4 wires) |
| `lsusb` shows nothing in DFU mode | DFU not entered | Press RESET (LED should pulse), run `lsusb` within 2.5s |
| `lsusb` shows nothing in DFU mode | Missing udev rule | See Prerequisites |
| "Invalid DFU suffix signature" warning | `cargo objcopy` raw binary has no DFU metadata | **Benign** — ignore |
| "Error during download get_status" | `:leave` flag resets device out of DFU | **Normal** — flash succeeded |
| SOS blink pattern (3 short, 3 long, 3 short) | Invalid binary in QSPI | Use `--release` (debug too large for 480 KB SRAM) |
| No `/dev/ttyACM*` after bench flash | USB re-enumeration delay | Wait 2-3s, check `dmesg \| tail`, replug USB |
| SAI overrun error with SDRAM + audio | D-cache or heavy init racing DMA start-up | See [D-Cache Timing](#d-cache-timing) |
| Codec won't init on breadboard | DGND/AGND not connected | Bridge DGND↔AGND (carrier boards do this internally) |
| Firmware flashed but doesn't run (no LED activity) | dfu-util QSPI write unreliable | Verify with `blinky` first; update bootloader to v6.3 via [flash.daisy.audio](https://flash.daisy.audio/) |
| Firmware flashed but doesn't run (no LED activity) | Stale bootloader | Update to v6.3+ at [flash.daisy.audio](https://flash.daisy.audio/) (select "Flash Latest Bootloader") |

> **dfu-util reliability:** Some versions of dfu-util (especially v0.11) have
> reported QSPI write reliability issues with certain bootloader versions.
> If flashing appears to succeed but firmware doesn't run, update the bootloader
> to v6.3+ via [flash.daisy.audio](https://flash.daisy.audio/) (Chrome, WebUSB).
> Always validate the flash pipeline by flashing `blinky` first — if blinky
> doesn't blink, the issue is in the flash/boot path, not your firmware.

---

## Heap and Cache

Heap lives in 64 MB SDRAM. Memory is abundant — CPU cycles are the constraint.

Each `InterpolatedDelay` buffer = `max_delay_samples * 4` bytes (f32).
Per-effect heap at 48 kHz as the registry builds it (kernel plus adapter;
`sonido_platform::cycle_table::EFFECT_SDRAM`, all 36 effects; see also
[Memory Budget](#memory-budget)), the largest first:

| Effect | Heap @ 48 kHz | Notes |
|--------|:-------------:|-------|
| Looper | ~23 MB | 60 s stereo |
| Time stretch | ~1.0 MB | |
| Delay | ~776 KB | 2 s stereo (`MAX_DELAY_S` in `delay.rs`) |
| Texture, Glitch | ~525 KB each | |
| Reverb | ~118 KB | 8-line FDN + allpasses + predelay |
| Spring reverb, Plate reverb, Harmonic habitat | ~100–115 KB each | |
| Pitch shift | ~66 KB | |
| Tuner | ~33 KB | |
| Chorus | ~24 KB | |
| Every other effect | under 20 KB | Mostly state held inline; the smoothed adapter adds a few hundred bytes |

### Memory Placement (BOOT_SRAM)

```
AXI SRAM (480 KB usable, 0-wait — code executes here)
└── .text + .rodata (firmware code)

DTCM (128 KB, 0-wait — data, hot path)
├── Stack (grows down from 0x2002_0000)
└── .bss + .data (globals, statics)

D2 SRAM (288 KB, 1–2 wait)
├── RAM_D2_DMA (32 KB @ 0x3000_0000): audio DMA buffers (.sram1_bss)
└── RAM_D2 (256 KB @ 0x3000_8000): general purpose (heap_test puts its heap here)

SDRAM (64 MB, 4–8 wait, MPU cacheable — heap)
├── Global heap allocator (all Vec/Box allocations)
├── Delay lines (~776 KB for 2 s stereo delay)
├── Reverb buffers (~117 KB)
└── Looper buffers (~23 MB for 60 s stereo)
```

**Hot/cold path separation:** The per-sample audio callback runs from DTCM
stack (0-wait local variables) and reads/writes delay lines in SDRAM via
the Cortex-M7 L1 data cache. Sequential delay line access (1 read + 1
write per sample) is cache-friendly: a 32-byte cache line holds 8 `f32`
samples, giving ~87.5% hit rate. Cold-path operations (kernel `new()`,
parameter changes, effect swaps) touch SDRAM uncached during allocation —
this is fine since they run once, not per-sample.

**SDRAM initialization:** The FMC controller and SDRAM power-up sequence
must be run by each application — the bootloader does not initialize SDRAM.
Use the `init_sdram!` macro after `embassy_stm32::init()`:

```rust
let p = embassy_stm32::init(config);
let mut cp = unsafe { cortex_m::Peripherals::steal() };
let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
unsafe { HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE); }
```

The macro configures MPU Region 0 at `0xC000_0000` (64 MB, write-back cacheable),
MPU Region 1 at `0x3000_0000` (512 KB covering D2 SRAM, non-cacheable for DMA
buffers), enables I-cache, sets up all 54 FMC GPIO pins, and runs the
AS4C16M32MSA-6 power-up sequence.

### D-Cache Timing

`init_sdram!` enables the I-cache but not the D-cache; call
`sonido_daisy::sdram::enable_dcache()` yourself. Every example that uses the
SDRAM heap (`sonido_pedal`, `graph_audio_test`, `single_delay`, `bench_kernels`)
calls it immediately after `init_sdram!` and `HEAP.init()`, before any audio setup:

```rust
let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
unsafe { HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE); }
sonido_daisy::sdram::enable_dcache(); // before AudioPeripherals / start_callback
```

The SAI clocks only start inside `start_callback()`, so the cache is already on
when DMA begins, and `start_callback()` retries through the first few overruns
while the DMA pipeline primes. The `enable_dcache()` doc comment in `sdram.rs`
still recommends enabling it from a deferred task after DMA is running; no example
does that today.

**Why DMA buffers stay coherent:** MPU Region 1 marks the D2 SRAM range
(`0x3000_0000`, 512 KB) as Normal Non-Cacheable, so the D-cache never caches
DMA buffer accesses.

**Performance impact:** D-cache gives ~3–5x speedup on SDRAM delay line reads
(4–8 wait states → ~1 cycle for cache hits), per the `enable_dcache()` docs. It
has not been measured on this board.

### Chain Configurations

With the heap in 64 MB SDRAM, memory is no longer a constraint for
effect chains: only a rig with three loopers (23 MB each) would outgrow it.
CPU budget is the limiting factor.

One rule decides whether a chain runs on the pedal, shared by the editor and the
CLI (`sonido_patch::validate::validate_for_daisy`) and the pedal (`check_rig` for
rigs, the jam's pick knobs): each effect's `EFFECT_CYCLES` (its worst block on the rig path,
measured on a Daisy Seed on 2026-10-22, see
[Benchmarks](BENCHMARKS.md#cortex-m7-measured-on-a-daisy-seed)) plus
`NODE_OVERHEAD_CYCLES` (400) per node, within `CHAIN_FRACTION` (80%) of the
320,000-cycle block (`sonido_platform::cycle_table::chain_fits`). The estimates
below are that sum (`chain_cycles`); memory is the sum of `EFFECT_SDRAM`.

| Chain | Memory | Estimate | Runs on the pedal |
|-------|-------:|---------:|:-----------------:|
| Preamp → Wah → Distortion → Chorus | ~25 KB | 38.6% | yes |
| Preamp → Distortion → Chorus → Delay(2s) | ~801 KB | 41.8% | yes |
| Preamp → Tape → Flanger → Delay(2s) | ~788 KB | 58.3% | yes |
| Distortion → Reverb → Ringmod (the heaviest jams) | ~119 KB | 79.4% | yes |
| Compressor → Distortion → Reverb | ~119 KB | 89.4% | no |
| Preamp → Distortion → Chorus → Delay → Reverb | ~919 KB | 103.8% | no |
| Reverb, Reverb | ~237 KB | 124.0% | no |

The factory rigs estimate between 44% (Shimmer Cathedral) and 77% (Amp Stack)
([Factory Rigs](#factory-rigs)). Every one of the 36 effects fits a block alone,
worst block included; the full table is in [Benchmarks](BENCHMARKS.md).

**Why 80%.** The estimate counts the effects; the whole pedal adds the codec DMA,
the player and the controls, and its slowest callbacks run above its mean. Measured
in a Hothouse with a guitar on 2026-10-22 with the [CPU load meter](#cpu-load-meter),
after the [real-time hardening](#real-time-hardening) and the control redesign: the
jam distortion → reverb → ringmod (estimate 79.4%) ran at 74.7% mean and 84–85% at
its slowest callbacks, with no callback over budget and no codec overrun at boot.

**The previous firmware, measured.** Before the rig player, the pedal ran its own
`ProcessingGraph` of unsmoothed `Adapter<K, DirectPolicy>` kernels. Measured the same
way earlier on 2026-10-22, steady playing only (the second of each change left out):

| Slots | Mean | Typical peak | Highest peak | Callbacks over budget |
|-------|-----:|-----:|-----:|-----:|
| empty | 1.9% | 2.4% | 2.6% | 0 of 454,622 |
| delay | 11.2% | 13.8% | 14.0% | 0 of 286,577 |
| reverb | 57.7% | 65.0% | 72.3% | 0 of 307,636 |
| reverb, bitcrusher | 62.0% | 67.0% | 76.5% | 0 of 262,663 |
| distortion → reverb | 70.6% | 77.0% | 81.2% | 0 of 87,058 |
| distortion → reverb → bitcrusher | 78.1% | 85.1% | 89.0% | 0 of 75,050 |
| distortion → reverb → delay | 80.3% | 87.1% | 89.7% | 0 of 13,509 |
| reverb, reverb (now unselectable) | 123.2% | 131.0% | 136.0% | 5,777 of 5,777 |

"Typical peak" is the median of each second's slowest callback. That firmware alone
cost about 2%; with effects loaded the mean ran 0–5 points above the sum of the
kernels' bare means, and the typical peak 5–9 points above the mean, which is why
the pedal budgets on worst blocks with margin. Its rule, on bare-kernel numbers,
was 85% for a chain; the distortion rows were measured with an intermediate soft
clipper that cost 17% more than the shipped one.

---

## Hothouse

The Cleveland Music Co. Hothouse is a DIY pedal enclosure for Daisy Seed.
Open-source hardware (CC BY-SA 4.0). Stereo version (Sep 2024+): 6 knobs, 3 toggles, 2 footswitches, 2 LEDs.

- **Repo**: [clevelandmusicco/HothouseExamples](https://github.com/clevelandmusicco/HothouseExamples)
- **Hardware**: [clevelandmusicco/open-source-pedals](https://github.com/clevelandmusicco/open-source-pedals/tree/main/hothouse)
- **Wiki**: [10-Minute Quick Start](https://github.com/clevelandmusicco/HothouseExamples/wiki/10%E2%80%90Minute-Quick-Start)

### Controls — Pin Mappings

**Knobs** (10K potentiometers, ADC analog inputs):

| Hothouse | Daisy Pin | STM32 GPIO | ADC Input | sonido Mapping |
|----------|-----------|------------|:-----------:|----------------|
| KNOB_1 | D16 | PA3 | ADC1_INP15 | `ControlId::hardware(0)` |
| KNOB_2 | D17 | PB1 | ADC1_INP5 | `ControlId::hardware(1)` |
| KNOB_3 | D18 | PA7 | ADC1_INP7 | `ControlId::hardware(2)` |
| KNOB_4 | D19 | PA6 | ADC1_INP3 | `ControlId::hardware(3)` |
| KNOB_5 | D20 | PC1 | ADC1_INP11 | `ControlId::hardware(4)` |
| KNOB_6 | D21 | PC4 | ADC1_INP4 | `ControlId::hardware(5)` |

**Toggle Switches** (3-way, 2 GPIO pins each, pull-up, active-low):

| Hothouse | Up Pin | Down Pin | sonido Mapping |
|----------|--------|----------|----------------|
| SWITCH_1 | D9 (PB4) | D10 (PB5) | `ControlId::hardware(6)` |
| SWITCH_2 | D7 (PG10) | D8 (PG11) | `ControlId::hardware(7)` |
| SWITCH_3 | D5 (PD2) | D6 (PC12) | `ControlId::hardware(8)` |

**Footswitches** (momentary, active-low GPIO):

| Hothouse | Daisy Pin | STM32 GPIO | sonido Mapping |
|----------|-----------|------------|----------------|
| FOOTSWITCH_1 (left) | D25 | PA0 | `ControlId::hardware(9)` |
| FOOTSWITCH_2 (right) | D26 | PD11 | `ControlId::hardware(10)` |

**LEDs** (GPIO output):

| Hothouse | Daisy Pin | STM32 GPIO | sonido Mapping |
|----------|-----------|------------|----------------|
| LED_1 | D22 | PA5 | `ControlId::hardware(11)` |
| LED_2 | D23 | PA4 | `ControlId::hardware(12)` |

Pins come from the `hothouse_pins!` macro; the `ControlId` indices are the ones
`HothousePlatform` (`src/hothouse.rs`) exposes.

- **Audio** — 1/4" TRS stereo I/O, instrument level (200mV–1V p-p).
  Synth line out (~2.8V) needs padding; Eurorack (5–10V) will clip.
- **Free pins** — D11/D12 (PB8/PB9, I2C for OLED), D13/D14 (PB6/PB7, UART for MIDI)

### Bootloader Shortcut

The C++ HothouseExamples enter DFU when FOOTSWITCH_1 is held for 2 seconds
(`CheckResetToBootloader()`). `sonido_pedal.rs` keeps the bootloader off the
footswitches during play: hold **both** footswitches while powering on, or send the
USB rig link's `Bootloader` request (`sonido pedal update`). It writes the
bootloader's infinite-timeout magic to backup SRAM and resets, so the bootloader
stays in DFU mode. No need to open the enclosure after the first flash. The other
examples have no shortcut.

### Toggle Reading

`hothouse::decode_toggle()` reads the two pull-up GPIO pins of a 3-way toggle:

```rust
match (up.is_low(), dn.is_low()) {
    (true, false) => 0, // UP
    (false, true) => 2, // DN
    _ => 1,             // MID (or fault — both low — treated as MID)
}
```

---

## Platform Abstraction

The `PlatformController` trait (`crates/sonido-platform/src/lib.rs`) exposes
hardware controls as `ControlId` → normalized `ControlState`. On the Hothouse:

1. `hothouse_control_task` reads ADC/GPIO pins into the `HothouseBuffer`
2. `HothousePlatform` (feature `platform`) serves those values as `ControlId`s
   0–12; no example uses it today: `sonido_pedal.rs` reads the `HothouseBuffer`
   directly
3. On the pedal, knob readings become rig macro positions
   (`PatchPlayer::set_macro`) or, in the jam, parameter values through
   `knob_map` + `knob_to_param`, written into the `PatchPlayer`'s registry-built
   effects (with T2 in the middle, the jam's setup page: levels and effect picks)

`ControlMapper` (also in `sonido-platform`) is not used by the firmware.

### What Sonido Already Handles

| Capability | Detail |
|------------|--------|
| Cross-compilation | `no_std` crates: `sonido-core`, `sonido-effects`, `sonido-registry`, `sonido-platform`, `sonido-patch` (all firmware dependencies) and `sonido-synth` |
| DMA-ready DSP | `DspKernel::process_stereo()` — no alloc, no dispatch |
| ADC mapping | `knob_to_param()` / `adc_to_param()` from `ParamDescriptor`s; `KernelParams::from_knobs()` for direct-kernel examples |
| Control discovery | `ParameterInfo` — runtime parameter introspection |
| Math safety | `libm` everywhere — no `f32::sin()` in `no_std` crates |
| Heap support | Delay buffers use `Vec` with any global allocator |
| Rig loading | `patch_loader.rs` reads and decodes a `sonido-patch` sector from the QSPI rig bank and checks it against the pedal's CPU and memory rule; `sonido_pedal.rs` plays it through `PatchPlayer` |
| Persistence | The rig bank (15 slots) and the state sector (active rig, jam, topology, `JamSettings`, tempo) in QSPI, written by the firmware itself; rigs arrive over the USB rig link |
| Tap tempo | `tap_tempo.rs`, on the pedal's FS2 in every rig (T2 up or down); in the jam it sets the delay's time |

### What Needs Work

| Gap | Detail |
|-----|--------|
| Block processing | Biquad/SVF have per-sample `process()` only — block version would improve CM7 cache behavior |
| Expression pedal | `expression.rs` (smoothing, deadzone, slew) exists; no example reads an expression input |
| MIDI CC routing | `midi.rs` parses USB-MIDI packets; nothing routes CC to parameters |
| Device sync | `device_sync.rs` exists as library code only |

---

## Hardware Interface Gaps

Features needed for production pedal deployment: expression pedal input, CV input
(Eurorack), MIDI CC routing, pot calibration, control curves, parameter pages,
and debounce. Full details and implementation plans are in
[ROADMAP.md — Embedded Hardening](ROADMAP.md#embedded-hardening).

---

## References

### Electrosmith

- [Daisy Seed Product Page](https://electro-smith.com/products/daisy-seed)
- [Datasheet v1.1.5](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/Daisy_Seed_datasheet_v1-1-5.pdf)
- [Memory Variants](https://electro-smith.com/pages/memory-what-is-the-difference)
- [Web Programmer](https://flash.daisy.audio/) — Chrome, WebUSB
- [Troubleshooting](https://daisy.audio/troubleshooting/)
- [STM32H750](https://www.st.com/en/microcontrollers-microprocessors/stm32h750-value-line.html)

### Rust Ecosystem

- [daisy-embassy](https://crates.io/crates/daisy-embassy)
  ([repo](https://github.com/daisy-embassy/daisy-embassy))
- [Embassy STM32H750 docs](https://docs.embassy.dev/embassy-stm32/git/stm32h750xb/index.html)
- [Embassy DMA double-buffer issue](https://github.com/embassy-rs/embassy/issues/702)
- [zlosynth/kaseta](https://github.com/zlosynth/kaseta) —
  Production Rust DSP on Daisy
- [probe-rs](https://probe.rs/docs/getting-started/installation/)

### Cleveland Music Co.

- [Hothouse Product Page](https://clevelandmusicco.com/hothouse-diy-digital-signal-processing-platform-kit/)
- [HothouseExamples](https://github.com/clevelandmusicco/HothouseExamples) — C++/PureData examples
- [Open Source Hardware](https://github.com/clevelandmusicco/open-source-pedals/tree/main/hothouse) — Gerber, BOM, CPL
- [USB Noise Wiki](https://github.com/clevelandmusicco/HothouseExamples/wiki/About-USB-Noise)

### Community

- [Daisy Forum: Rust development](https://forum.electro-smith.com/t/rust-starter-for-daisy-seed/684)
- [Daisy Forum: Rev 7 noise floor](https://forum.electro-smith.com/t/rev-7-noise-floor-vs-rev-4/4943)
- [Daisy Forum: Hothouse thread](https://forum.electro-smith.com/t/hothouse-dsp-pedal-kit/5631)

### Sonido Internal

- [Kernel Architecture](KERNEL_ARCHITECTURE.md)
- [Benchmarks](BENCHMARKS.md) — Cortex-M7 cycles measured on a Daisy Seed, and how to regenerate the cycle table
- [Patch Platform](PATCH_PLATFORM.md) — the rig format, `PatchPlayer`, editor export
- [Architecture](ARCHITECTURE.md)
