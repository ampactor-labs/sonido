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
| QSPI Flash | 8 MB (Winbond W25Q64JV, JEDEC id `EF 40 17`) |
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

Per-effect heap usage for the 8 pedal effects (`sonido_registry::PEDAL_EFFECT_IDS`),
measured as the bytes allocated by each kernel's `new(48_000.0)` (counting global
allocator, host build). `Adapter::new_direct` (the `DirectPolicy` wrapper the pedal
uses) allocates nothing extra; each effect also costs one small `Box` in the graph.

| Effect | Kernel heap (bytes) | Notes |
|--------|-------------------:|-------|
| chorus | 23,040 | 4 modulated `InterpolatedDelay` lines |
| phaser | 0 | Allpass stages and LFOs held inline |
| distortion | 0 | |
| bitcrusher | 0 | |
| delay | 775,680 | 2 s stereo delay lines plus diffusion allpasses |
| reverb | 116,700 | 8 FDN delay lines, modulated allpasses, early-reflection tap delay, stereo predelay |
| ringmod | 0 | |
| filter | 0 | |

For comparison, effects that are not on the pedal: flanger 5,776, tape 3,848,
looper 23,040,000 (60 s stereo); preamp, gate, wah, compressor, tremolo and
vibrato allocate nothing.

The firmware runs up to 3 effects at once. The worst pedal combination is
delay + reverb + chorus = **915,420 bytes (~0.9 MB)**. That exceeds the AXI SRAM
(which holds the code anyway), so the pedal puts its heap on the 64 MB SDRAM,
where any combination fits with room to spare. See
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
`thumbv7em-none-eabihf` target.

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
| — | `qspi_read_test.rs` | W25Q64 JEDEC id + decode of patch slot 0 (defmt output) | Seed + SWD probe |
| 4 | `single_effect.rs` | One kernel via `EffectSlot` + noon-biased knob mapping (distortion by default) | Hothouse |
| 4 | `single_delay.rs`, `single_reverb.rs` | One kernel driven directly via `from_knobs()` | Hothouse |
| 5 | `sonido_pedal.rs` | `Adapter<K, DirectPolicy>` + ProcessingGraph DAG + A/B morph | Hothouse |

Examples that need features (`required-features` in `Cargo.toml`): `sonido_pedal`
and `single_effect` need `--features alloc,platform`; `graph_audio_test`,
`qspi_read_test`, `single_delay` and `single_reverb` need `--features alloc`.

### Modern Rust on Daisy Seed

**sonido-daisy** owns the full platform layer (clock, audio, ADC, LED). Key patterns:

- **Clock**: `sonido_daisy::rcc_config(ClockProfile::Performance)` or `::Efficient` — 480 MHz / 400 MHz with proper PLL2_P ADC clock fix.
- **Audio**: `AudioPeripherals::prepare_interface()` → `start_interface()` → `start_callback()` — async loop, one callback per DMA transfer (~0.667 ms at 48 kHz, 32-sample blocks). SAI clocks start inside `start_callback()`.
- **LED / UI**: Use `sonido_daisy::heartbeat` — the shared lub-dub blink task. Every binary spawns it: `spawner.spawn(heartbeat(UserLed::new(p.PC7))).unwrap();`.
- **USB / Serial**: Same spawned-task pattern. See `hothouse_diag.rs`.
- **Audio callback is real-time** — NEVER block in the audio callback. No ADC reads, no USB, no allocation. Only pure DSP math and lock-free ControlBuffer reads.

Reference implementation: `crates/sonido-daisy/examples/single_effect.rs`

### Control / Audio Separation

All 6 knobs use uniform `blocking_read()` polling in a 100 Hz Embassy task (`hothouse_control_task`, `POLL_INTERVAL_MS` = 10 in `hothouse.rs`). At ~8 µs per channel (CYCLES387_5 sample time), 6 reads cost ~48 µs per 10 ms cycle (~0.5% CPU). This matches libDaisy's own `AnalogControl` polling approach — no DMA channel, DMA buffer, or D2 SRAM allocation needed for the control path. GPIO reads (toggles, footswitches) are instant register accesses. Shared state flows through a lock-free `ControlBuffer` using `Relaxed` atomics. Knob values are not IIR-smoothed: `ControlBuffer::write_knob` snaps readings within 0.005 of either end to 0.0/1.0 and raises a change flag when a value moves more than 3e-4.

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
| `qspi.rs` | Preset-sector layout and (de)serialization for the last 4 KB of QSPI flash — not yet used by any example | `core` |
| `qspi_flash.rs` | Blocking W25Q64 read driver (`QspiFlash`, `patch_slot_addr()`) | Embassy |
| `patch_loader.rs` | `pedal_make_effect()`, `load_patch()`, `load_and_build()` — read a `sonido-patch` patch from QSPI and build a `GraphEngine` (feature `alloc`) | `sonido-patch` |
| `effect_slot.rs` | `EffectSlot` — click-free bypass ramp, NaN/Inf sanitization, knob mapping bridge; also exports `BypassCrossfade`, `sanitize_stereo`, `CONTROL_POLL_EVERY` (feature `platform`) | `sonido-core` (SmoothedParam), `sonido-platform` (knob_mapping) |
| `noon_presets.rs` | Re-exports `sonido_platform::noon::noon_value` plus compile-time length checks against `KernelParams::COUNT` (feature `platform`) | `sonido-platform` |
| `expression.rs`, `midi.rs`, `tap_tempo.rs`, `device_sync.rs` | Expression-pedal smoothing, USB-MIDI packet parser, tap-tempo BPM, two-Seed sync frame format — library code not yet used by any example | `core` |

The ADC→parameter conversions live in `sonido-platform`, not in this crate:
`crates/sonido-platform/src/param_map.rs` (`adc_to_param`, `adc_to_param_biased`),
`noon.rs` (`noon_value`, `HARDWARE_MAPPED`) and `knob_mapping.rs` (`knob_map`,
`knob_to_param`).

### Biased Knob Mapping (Noon = Sweet Spot)

The Hothouse 6-knob pedal uses a "noon = sweet spot" convention: knobs at 12 o'clock should produce a musically useful tone. This is achieved by biasing the ADC-to-parameter mapping curve, **not** by narrowing descriptor ranges (see ADR-030).

`sonido_platform::adc_to_param_biased(desc, noon, normalized)` splits the knob travel at center:
- `[0.0, 0.5]` maps `desc.min` → `noon`
- `[0.5, 1.0]` maps `noon` → `desc.max`

Both halves respect the descriptor's `ParamScale` (Linear, Logarithmic, Power).

`adc_to_param_biased` automatically falls back to linear mapping (`adc_to_param`) for
STEPPED parameters (equal knob travel per option) and when the noon value is at or near
a range extreme (within 5% of either end). This eliminates dead zones — where half the
knob travel produces no change — without requiring the caller to check parameter types.

Firmware calls the wrapper `sonido_platform::knob_to_param`, which looks up the noon value
(falling back to the descriptor default) and applies per-effect knob floors (currently
delay time starts at 30 ms instead of 1 ms):

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

The noon values are centralized in `sonido_platform::noon::noon_value(effect_id, param_idx)`.
Most equal the descriptor defaults; mix parameters use 100% for insert effects
(distortion, bitcrusher, ringmod) and 50% for blend effects (delay, reverb, chorus,
compressor). The biased mapping centers the ADC curve on these sweet spots without
narrowing the available range.

For effects without noon presets, `knob_to_param` uses the descriptor default as noon,
and `adc_to_param()` (linear mapping) remains available for controls with no sweet spot
(the morph pedal uses it for morph speed and master level).

### Noon Preset Verification

The mapping functions and noon table live in `sonido-platform`, which builds on the
host, so `param_map.rs` and `knob_mapping.rs` carry their own unit tests
(`cargo test -p sonido-platform`). `crates/sonido-effects/tests/noon_mapping.rs`
additionally checks the noon table against every effect in
`sonido_platform::noon::HARDWARE_MAPPED` (20 effects), using an inlined `std`-math copy
of the biased mapping.

**Test cases** (`noon_mapping.rs`):

| Test | What it catches |
|------|----------------|
| `noon_coverage_hardware_mapped` | Writable param of a hardware-mapped effect without a noon preset |
| `noon_values_in_range` | Stale noon after descriptor range change (the original ADR-030 bug class) |
| `biased_mapping_endpoints` | Dead zone where knob 0→min or knob 1→max fails |
| `biased_noon_at_center` | Knob center doesn't produce the sweet-spot value |
| `biased_mapping_monotonic` | Non-monotonic output from biased split algorithm |

**READ_ONLY diagnostic params** — metering outputs, not knob-writable. Noon arrays still
include an entry for them (arrays cover all `KernelParams::COUNT` params so the
compile-time length checks in `noon_presets.rs` pass), but no knob map points at them:

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
| *(none)* | Core library: audio, controls, hothouse, LED, RCC, SDRAM, QSPI | Simple examples (blinky, passthrough, diagnostics, bench_kernels) |
| `alloc` | `Adapter`/`DirectPolicy` re-exports, `patch_loader` | graph_audio_test, qspi_read_test, single_delay, single_reverb |
| `platform` | Implies `alloc`; pulls in `sonido-platform`; enables `HothousePlatform`, `effect_slot`, `noon_presets` | sonido_pedal, single_effect |

### Dependencies

| Crate | Version | Purpose |
|-------|:-------:|---------|
| `embassy-stm32` | 0.5 | Async HAL — SAI, DMA, GPIO, ADC, FMC |
| `embassy-executor` | 0.9 | Async task executor for Cortex-M |
| `embassy-time` | 0.5 | Timer and delay utilities |
| `embassy-usb` | 0.5 | USB CDC ACM serial output |
| `stm32-fmc` | 0.4 | SDRAM controller (AS4C16M32MSA-6 device definition) |
| `cortex-m` | 0.7 | Low-level Cortex-M access — DWT, SCB, MPU |
| `cortex-m-rt` | 0.7 | Runtime — vector table, entry point |
| `embedded-alloc` | 0.6 | Heap allocator (`LlffHeap`; SDRAM in the pedal, D2 SRAM in `heap_test`) |
| `critical-section` | 1 | Critical-section implementation (single-core) |
| `libm` | 0.2 | `no_std` float math |
| `grounded` | 0.2 | DMA buffer management (GroundedArrayCell) |
| `static_cell` | 2 | Safe static initialization for USB buffers |
| `sonido-core`, `sonido-effects`, `sonido-registry`, `sonido-patch` | path | DSP, kernels, effect IDs/UIDs, patch decoding (`default-features = false`) |
| `sonido-platform` | path, optional | Knob mapping, noon table, `PlatformController` (feature `platform`) |
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
slot.apply_knobs(&knob_vals);                 // noon-biased ADC→param
let (l, r) = slot.process_stereo(in_l, in_r); // effect → NaN guard → bypass ramp
```

`sonido_pedal.rs` (Tier 5) uses the standalone utilities — `BypassCrossfade`
and `sanitize_stereo` — alongside its DAG-based `ProcessingGraph`,
since the graph owns the effects rather than individual `EffectSlot` wrappers.

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

## Morph Pedal

The morph pedal (`examples/sonido_pedal.rs`, "Morph Pedal v2" in its header) is the
flagship firmware — a 3-slot, DAG-routed, A/B morphing guitar pedal with 8 curated
effects. It demonstrates `ProcessingGraph` for routing, `Adapter<K, DirectPolicy>` for
zero-smoothing kernel access, noon-biased knob mapping, and footswitch-controlled
crossfade between two parameter snapshots.

### Architecture Overview

```
   ┌─────────┐     ┌────────────────────┐     ┌──────────────┐
   │ ADC × 6 │──→──│  knob_to_param()   │──→──│ Graph        │
   │ Toggles  │     │  (noon-biased,     │     │ .effect_with_│
   │ Footsw   │     │   ParamScale-aware)│     │ params_mut() │
   └─────────┘     └────────────────────┘     └──────────────┘
                                                   │
                        ┌──────────────────────────┘
                        ▼
   ┌─────────────────────────────────────────────────────┐
   │             ProcessingGraph (compiled DAG)           │
   │  Input → [Adapter<K, DirectPolicy>] → ... → Output  │
   │          (zero-smoothing kernel wrapper)             │
   └─────────────────────────────────────────────────────┘
```

The input is summed to mono (`(L + R) / 2`) and fed to both graph inputs.

Two sonido pillars are used together:

- **ProcessingGraph** — DAG routing with serial/parallel/fan topologies via
  split/merge nodes. Zero-alloc per audio block after `compile()`.
- **`Adapter<K, DirectPolicy>`** — wraps any kernel in `Effect + ParameterInfo`
  (and thus `EffectWithParams` via blanket impl) with zero smoothing overhead.
  `set_param()` writes directly to the kernel's typed params struct. The value
  is live on the next `process_stereo()` call.

Why not `Adapter<K, SmoothedPolicy>`? It adds a per-sample `SmoothedParam::advance()`
call for every parameter. On the pedal, params change at the ~100 Hz control rate,
so the per-sample smoothing is overhead the firmware does not need.

### DirectPolicy Adapter Pattern

`Adapter<K, DirectPolicy>` is the `DirectPolicy` form of the generic
`Adapter<K, S: SmoothingPolicy>` in `crates/sonido-core/src/kernel/adapter.rs`.
Simplified:

```rust
struct Adapter<K: DspKernel, S: SmoothingPolicy> {
    kernel: K,
    policy: S,            // DirectPolicy is a zero-sized marker
    snapshot: K::Params,  // typed params the kernel reads
    sample_rate: f32,
}

// With S = DirectPolicy, in effect:
fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
    self.kernel.process_stereo(l, r, &self.snapshot)
}
fn set_param(&mut self, idx: usize, val: f32) { self.snapshot.set(idx, val); }
fn get_param(&self, idx: usize) -> f32 { self.snapshot.get(idx) }
fn param_info(&self, idx: usize) -> Option<ParamDescriptor> { K::Params::descriptor(idx) }
```

Comparison with sonido's other DSP bridge patterns:

| Pattern | Context | Smoothing | Trait impl |
|---------|---------|-----------|------------|
| `Adapter<K, SmoothedPolicy>` | Desktop / Plugin; `pedal_make_effect()` for flash-loaded patches | Yes (SmoothedParam per param) | Effect + ParameterInfo |
| `Adapter<K, DirectPolicy>` | Morph pedal, `single_effect.rs` | None (direct write) | Effect + ParameterInfo |
| Direct `DspKernel` | `single_delay.rs`, `single_reverb.rs` | None (`from_knobs()`) | No trait — call `process_stereo()` directly |

Use `Adapter<K, DirectPolicy>` when you need `ProcessingGraph` compatibility (the graph
requires `Effect + ParameterInfo` behind `EffectWithParams`). Use direct kernel
access when you have a single fixed effect and don't need the graph.

### Toggle Mapping

Three toggles divide control across three orthogonal concerns:

| Toggle | UP (0) | MID (1) | DOWN (2) |
|--------|--------|---------|----------|
| **T1** | Node 1 (effect slot 1) | Node 2 (effect slot 2) | Node 3 (effect slot 3) |
| **T2** | **A mode**: hear/edit A state | **B mode**: hear/edit B state | **Morph mode**: FS1/FS2 control morph |
| **T3** | **Linear**: 1 → 2 → 3 | **Parallel**: split → [1,2,3] → merge | **Fan**: 1 → split → [2,3] → merge |

T1 selects which of the 3 effect slots the knobs and footswitch scrolling control.
T2 selects the editing/performance mode. T3 selects routing topology (triggers graph
rebuild). Empty slots are skipped when wiring, so with one populated slot every
topology is a plain chain.

### A/B Editing

T2 selects between editing two independent parameter snapshots (A and B) and
performing a morph between them. The pedal boots with all three slots empty
(bypass).

**A mode** (T2=UP) — Knobs K1–K6 write to A-state parameters for the slot selected
by T1, through the effect's `knob_map` and `knob_to_param()` (noon-biased,
`ParamScale`-aware). Sound output reflects the A snapshot.
Footswitch taps (released in under 300 ms, `TAP_LIMIT` = 30 polls): FS1 = previous
effect, FS2 = next effect. Selecting a new effect resets both A and B states for
that slot to the effect's defaults. Scrolling skips any effect that would take the
chain past 85% of the block budget, summing the measured worst blocks in
`sonido_platform::cycle_table` (`CHAIN_BUDGET`); in practice that rules out a second
reverb, which measured 123% of the budget beside the first. Holding FS1 for 300 ms
or longer and releasing it loads the next of the 3 factory presets, starting with
preset 1 after boot; LED 2 then blinks the preset number.

Normally "knob = truth" — the physical pot position is the param value. After
changing slot (T1), changing mode (T2), scrolling effects, or loading a factory
preset, **soft takeover** locks all 6 knobs until each physical position comes
within 5% of the parameter's range (`PICKUP_THRESHOLD_FRAC`) of the current value,
preventing parameter jumps.

**B mode** (T2=MID) — Knobs write to B-state parameters. On first entry, B is
initialized as a copy of A. Sound output reflects the B snapshot. Same
footswitch effect scrolling as A mode (factory presets load only from A mode).
Switching between A and B modes snaps the sound to the stored snapshot.

**Morph mode** (T2=DOWN) — Footswitch-controlled crossfade between A and B.
Entering from A starts at `morph_t` = 0.0, from B at 1.0.
FS1 held → `morph_t` ramps toward 0.0 (A). FS2 held → toward 1.0 (B).
Neither held → latch (morph_t stays where it is). **K5** sets the morph time
(0.1–5.0 s for a full A→B sweep, default 2 s) and **K6** sets master level
(−60 to +12 dB, applied only in morph mode); both use linear `adc_to_param()`.
K1–K4 are inactive. `interpolate_and_apply()` writes lerped params to all slots
every control poll (~100 Hz). STEPPED params snap at t=0.5 (same as
`KernelParams::lerp()`). A slot whose B was never edited has B = A, so its
parameters do not move during the morph.

### Bypass and Bootloader

Pressing both footswitches together and releasing them toggles global bypass
(all modes). In bypass the input is copied straight to the output.

Holding both footswitches for 3 s (`BOOTLOADER_HOLD_TICKS` = 300 polls) writes
the Daisy bootloader's "stay in DFU" magic (`0xB0074EFA`) to backup SRAM and resets,
so the next firmware can be flashed without opening the enclosure.

### Rebuilds and Tails

Changing an effect, loading a preset or flipping T3 rebuilds the graph in
`graph_rebuild_task`; meanwhile the callback ramps the bypass crossfade to dry and
back. Effects with a tail (reverb and delay) can ring out beside the new chain
(`ProcessingGraph` spillover), but every node is rebuilt, so a reverb's tail runs
next to a fresh reverb. The pedal keeps the old tails only when they and the new
chain sum to at most 70% of the budget in measured worst blocks (`SPILLOVER_BUDGET`)
and cuts them otherwise. Without that rule, any change with a reverb loaded ran two
reverbs for the 3-second tail, about 125% of the budget, and the codec overran some
500 times a second until the tail ended.

The rebuild still runs on the same executor as the audio loop, so each change costs
one to six codec restarts (`SAI … Overrun` in the log, a few milliseconds of
dropout) under the dry ramp.

### CPU Load Meter

The pedal times every audio callback with the DWT cycle counter and logs once a
second over defmt RTT. Run it from RAM with a probe attached to see it:

```bash
cd crates/sonido-daisy
cargo build --release --example sonido_pedal --features alloc,platform
probe-rs run --chip STM32H750IBKx --protocol swd target/thumbv7em-none-eabihf/release/examples/sonido_pedal
# load mean=78.1% peak=85.1% over_budget=0/1501 slots=[distortion, reverb, bitcrusher] topo=linear bypassed=false
```

`mean` and `peak` are the average and the slowest callback of that second against
the 320,000-cycle block budget; `over_budget` counts callbacks that missed it. A RAM
run is lost at power-off; the bootloader then starts whatever is in QSPI.

### LED Feedback

| LED | Meaning |
|-----|---------|
| LED1 | Bypass status — on = active, off = bypassed |
| LED2 | A/B mode: output level meter. Morph mode: duty cycle = `morph_t` (10-poll software PWM). After a factory preset load: blinks the preset number. Off in bypass |

LED duty is approximate: `hothouse_control_task` drives the GPIO on/off from the
value the callback last wrote (threshold 0.5), so the software PWM is sampled at
the 100 Hz control rate.

### Curated Effect List and Knob Mapping

8 effects, in the order of `PEDAL_EFFECT_IDS` (`crates/sonido-registry/src/lib.rs`).
Footswitch scrolling wraps through bypass (index 0) and these 8 (indices 1–8).
Flash-loaded patches identify effects by stable UID (`EFFECT_UIDS`), not by this
index:

| Scroll index | Effect ID | `knob_map` (K1–K6 → param index) |
|---:|---|---|
| 1 | `chorus` | `[0, 1, 4, 3, 2, 8]` |
| 2 | `phaser` | `[0, 1, 2, 3, 4, 9]` |
| 3 | `distortion` | `[0, 1, 3, 5, 4, 2]` |
| 4 | `bitcrusher` | `[0, 1, 2, —, 3, 4]` |
| 5 | `delay` | `[0, 1, 4, 3, 2, 9]` |
| 6 | `reverb` | `[0, 1, 2, 3, 4, 7]` |
| 7 | `ringmod` | `[0, 1, 2, —, 3, 4]` |
| 8 | `filter` | `[0, 1, 3, —, —, 2]` |

— = `NULL_KNOB` (knob unused). Knob roles are consistent across effects:

| Role | Knob | Always means |
|------|------|-------------|
| K1 | Primary | The defining control (rate, cutoff, drive, threshold) |
| K2 | Secondary | Next most important (depth, feedback, resonance, tone) |
| K3 | Color | Texture modifier (damping, HF rolloff, stages, jitter) |
| K4 | Character | Mode/shape, often STEPPED (voices, TZF, ping-pong) |
| K5 | Mix | Wet/dry blend when available |
| K6 | Level | Output/makeup gain |

The maps live in `knob_map()` in `crates/sonido-platform/src/knob_mapping.rs`,
keyed by registry ID (it also maps 7 effects that are not on the pedal).
`knob_to_param()` in the same file turns a 0–1 knob reading into a parameter
value on the noon-biased curve:

```rust
pub const NULL_KNOB: u8 = 0xFF; // sentinel for unmapped knobs

pub fn knob_map(effect_id: &str) -> Option<[u8; 6]> {
    match effect_id {
        "filter" => Some([0, 1, 3, NULL_KNOB, NULL_KNOB, 2]),
        "chorus" => Some([0, 1, 4, 3, 2, 8]),
        // ... knobs[k] = param index for knob K
        _ => None,
    }
}
```

The `create_effect(idx, sr)` factory in `sonido_pedal.rs` creates the matching
`Adapter<K, DirectPolicy>` for each scroll index:

```rust
fn create_effect(idx: usize, sr: f32) -> Option<Box<dyn EffectWithParams + Send>> {
    match idx {
        0 => None, // null / bypass
        1 => Some(Box::new(Adapter::new_direct(ChorusKernel::new(sr), sr))),
        2 => Some(Box::new(Adapter::new_direct(PhaserKernel::new(sr), sr))),
        // ... 3–8: distortion, bitcrusher, delay, reverb, ringmod, filter
        _ => None,
    }
}
```

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

### A/B Morphing

Each slot's state is a `NodeState` holding an A `SlotSnapshot` and an optional B
snapshot:

```rust
struct SlotSnapshot {
    values: [f32; MAX_PARAMS],   // MAX_PARAMS = 16
    stepped: [bool; MAX_PARAMS], // cached ParamFlags::STEPPED per param
    count: usize,
}

struct NodeState {
    effect_index: Option<usize>,
    params_a: SlotSnapshot,
    params_b: Option<SlotSnapshot>, // None until B mode or morph is first entered
    browse_cursor: usize,
}
```

`interpolate_and_apply()` writes lerped values to the graph every control
poll. STEPPED params snap at t=0.5 — the same logic as `KernelParams::lerp()`:

```rust
let val = if a.stepped[p] {
    if t < 0.5 { a.values[p] } else { b.values[p] }
} else {
    a.values[p] + (b.values[p] - a.values[p]) * t
};
effect.effect_set_param(p, val); // direct write via Adapter<K, DirectPolicy>
```

### Presets

Three factory presets are compiled in (`factory_preset()`), each with A and B
snapshots written as the kernels' typed params (`ReverbParams { decay_pct: 40.0, … }`),
so a renamed, added or reordered parameter fails the build instead of shifting
values; the stepped flags come from the descriptors:

1. **Room → Shimmer** — reverb in slot 1
2. **Slap → Self-Osc** — delay in slot 1
3. **Clean → Saturated** — distortion in slot 1, reverb in slot 2

FS1 long-press in A mode steps through them. Edits are held in RAM only and are
lost at power-off; the pedal does not save user presets. (Separately,
`patch_loader.rs` can load a `sonido-patch` patch from QSPI flash — see
`qspi_read_test.rs` — but `sonido_pedal.rs` does not call it.)

### How to Add a New Effect to the Morph Pedal

1. **Add the kernel import** in the `use sonido_effects::{...}` block at the
   top of `sonido_pedal.rs`.

2. **Append the effect ID to `PEDAL_EFFECT_IDS`** in
   `crates/sonido-registry/src/lib.rs` (append only, so scroll indices stay put).
   If `knob_map()` in `crates/sonido-platform/src/knob_mapping.rs`
   has no entry for it, add one: map the 6 knobs to parameter indices by
   checking the kernel's `KernelParams` implementation (look at
   `descriptor(index)` calls in the kernel file). Use `NULL_KNOB` (0xFF) for
   unused knobs. Follow the role convention:
   K1=Primary, K2=Secondary, K3=Color, K4=Character, K5=Mix, K6=Level.
   Add the effect to `HARDWARE_MAPPED` and `noon_value()` in
   `crates/sonido-platform/src/noon.rs` if it is not there yet.

3. **Add a match arm to `create_effect()`** at its `PEDAL_EFFECT_IDS` position + 1
   (index 0 is bypass):
   ```rust
   N => Some(Box::new(Adapter::new_direct(MyKernel::new(sr), sr))),
   ```
   and a matching arm, keyed by effect ID, in `pedal_make_effect()`
   (`src/patch_loader.rs`) so flash patches can load it.

4. **`NUM_EFFECTS` updates itself** — it is `EFFECT_IDS.len() + 1`.

5. **Build and verify:**
   ```bash
   cd crates/sonido-daisy
   cargo check --example sonido_pedal --release --features alloc,platform
   ```

### How to Modify Modes or Add Features

All mutable state lives in statics so the `start_callback` closure captures
nothing (a large captured future starved the executor and caused SAI overruns —
see the [v2 design note](superpowers/specs/2026-04-09-sonido-pedal-v2-design.md)):
`CB_STORAGE` (a `CallbackState`), `GRAPH_STORAGE`, `NODES_STORAGE`,
`NODE_IDS_STORAGE`, plus the atomics `BYPASSED`, `NEEDS_REBUILD` and
`GRAPH_UPDATING`. They are `static mut`, not `critical_section` mutexes, because
disabling interrupts during `process_block` blocks DMA completion.

```
Audio callback (every block, ~1500 Hz):
├── GRAPH_UPDATING? → fade to dry input via bypass crossfade, return
├── BYPASSED? → copy input→output, poll footswitches every 15 blocks, return
├── Deinterleave u24 → f32, sum to mono
├── graph.process_block() (compiled DAG, zero-alloc)
├── sanitize_stereo → bypass crossfade → master gain (morph mode) → f32 → u24
└── Every CONTROL_POLL_EVERY (15) blocks (~100 Hz):
    ├── Toggles: T3 topology (sets NEEDS_REBUILD), T1 slot, T2 mode transitions
    ├── Footswitches: morph ramp, both-FS bypass/bootloader,
    │   tap = scroll effect (sets NEEDS_REBUILD), FS1 hold = factory preset
    ├── Knobs: A/B → K1–K6 via knob_map + knob_to_param (with soft takeover);
    │          Morph → K5 morph time, K6 master level
    ├── Morph: interpolate_and_apply()
    └── LED feedback

graph_rebuild_task (Embassy task, every 20 ms):
├── graph.clear_garbage() when no rebuild is running
└── NEEDS_REBUILD? → set GRAPH_UPDATING, wait 5 ms, rebuild_graph_in_place(),
    capture default params for new effects, re-apply A/B/morph state,
    clear GRAPH_UPDATING
```

Key state (`CallbackState` fields unless noted):

| Variable | Type | Purpose |
|----------|------|---------|
| `NODES_STORAGE` (static) | `Option<[NodeState; 3]>` | Effect index and A/B snapshots per slot |
| `NODE_IDS_STORAGE` (static) | `[Option<NodeId>; 3]` | Graph node per slot (None = empty) |
| `GRAPH_STORAGE` (static) | `Option<ProcessingGraph>` | Compiled DAG |
| `ab_mode` | `AbMode` | Current operating mode (A / B / Morph) |
| `topology` | `Topology` | Current routing (Linear / Parallel / Fan) |
| `focused_node` | `usize` | Which slot knobs/FS control (T1) |
| `morph_t` | `f32` | Current morph position (0.0=A, 1.0=B) |
| `morph_speed` / `master_gain` | `f32` | K5 / K6 values in morph mode |
| `pickup_locked` | `[bool; 6]` | Soft-takeover state per knob |

Graph rebuilds happen only on topology changes: effect scroll, factory preset
load, or the T3 toggle. Parameter changes never rebuild — they go through
`graph.effect_with_params_mut(nid).effect_set_param()`.

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
Per-kernel heap at 48 kHz (measured; see [Memory Budget](#memory-budget)):

| Effect | Heap @ 48 kHz | Notes |
|--------|:-------------:|-------|
| Looper | ~23 MB | 60 s stereo |
| Delay | ~776 KB | 2 s stereo (`MAX_DELAY_S` in `delay.rs`) |
| Reverb | ~117 KB | 8-line FDN + allpasses + predelay |
| Chorus | ~23 KB | |
| Flanger | ~5.8 KB | |
| Tape | ~3.8 KB | |
| Phaser, Distortion, Compressor, Gate, Wah, Preamp, Tremolo, Vibrato, Filter, Bitcrusher, Ringmod | 0 | All state inline |

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
effect chains. Any combination of all 36 effects fits comfortably.
CPU budget is the limiting factor.

Kernel CPU per chain: the sum of each kernel's mean cost measured on a Daisy
Seed ([Benchmarks](BENCHMARKS.md#cortex-m7-measured-on-a-daisy-seed),
2026-10-22, % of the 320,000-cycle block budget at 480 MHz). The firmware's own
work (codec DMA, the graph, controls) comes on top.

**Comfortable** — kernels < 50%:

| Chain | Memory | Kernel CPU |
|-------|-------:|-----------:|
| Preamp → Distortion → Chorus → Delay(2s) | ~799 KB | 27% |
| Preamp → Wah → Distortion → Chorus | ~23 KB | 28% |
| Preamp → Tape → Flanger → Delay(2s) | ~785 KB | 40% |

**Tight** — kernels 50–80%, the reverb alone is 53%:

| Chain | Memory | Kernel CPU |
|-------|-------:|-----------:|
| Compressor → Distortion → Reverb | ~117 KB | 77% |
| Preamp → Distortion → Chorus → Delay → Reverb | ~915 KB | 80% |

Every one of the 36 kernels fits a block alone, worst block included; the full
table is in [Benchmarks](BENCHMARKS.md).

**Measured on the pedal.** The whole firmware (codec DMA, graph, controls, LEDs)
running in a Hothouse with a guitar, read from the [CPU load meter](#cpu-load-meter)
on 2026-10-22, steady playing only (the second of each change is left out):

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

"Typical peak" is the median of each second's slowest callback. The firmware alone
costs about 2%; with effects loaded the mean runs 0–5 points above the sum of the
kernels' bare means, and the typical peak 5–9 points above the mean, which is why the
pedal budgets on the sum of worst blocks with margin (85% for a chain, 70% while
tails ring out). Seconds containing a change peaked at up to 96%, still with no
callback over budget. The distortion rows were measured with an intermediate soft
clipper that cost 17% more than the shipped one (38.1k vs 32.6k cycles mean in
`bench_kernels`, under 2 points of the budget).

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
(`CheckResetToBootloader()`). `sonido_pedal.rs` uses a different gesture: hold
**both** footswitches for 3 s. It writes the bootloader's infinite-timeout magic to
backup SRAM and resets, so the bootloader stays in DFU mode. No need to open the
enclosure after the first flash. The other examples have no shortcut.

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
   0–12; `sonido_pedal.rs` reads knobs through it
3. `knob_map` + `knob_to_param` turn knob readings into parameter values, written
   through `Adapter<K, DirectPolicy>` and processed via `DspKernel::process_stereo()`

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
| Patch loading | `patch_loader.rs` decodes a `sonido-patch` sector from QSPI and builds a `GraphEngine` (`qspi_read_test.rs` exercises the read + decode; the pedal does not call it yet) |

### What Needs Work

| Gap | Detail |
|-----|--------|
| Block processing | Biquad/SVF have per-sample `process()` only — block version would improve CM7 cache behavior |
| Flash persistence | Morph pedal edits are RAM-only — lost on power cycle. `qspi.rs` defines a preset-sector layout but there is no flash write path yet |
| Expression pedal | `expression.rs` (smoothing, deadzone, slew) exists; no example reads an expression input |
| MIDI CC routing | `midi.rs` parses USB-MIDI packets; nothing routes CC to parameters |
| Tap tempo, device sync | `tap_tempo.rs` and `device_sync.rs` exist as library code only |

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
- [Benchmarks](BENCHMARKS.md) — Cortex-M7 cycle estimates
- [Architecture](ARCHITECTURE.md)
