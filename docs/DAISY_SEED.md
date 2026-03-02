# Daisy Seed Integration Reference

Hardware integration guide for running sonido on the Electrosmith Daisy Seed
(STM32H750 Cortex-M7). Covers the MCU, Rust ecosystem, memory budget, audio
I/O, and the path from sonido's existing no_std crates to a running pedal.

## Hardware Overview

### Daisy Seed Board

| Spec | Value |
|------|-------|
| MCU | STM32H750IBK6 (ARM Cortex-M7, single core) |
| Clock | 480 MHz (libDaisy defaults to 400 MHz for thermal headroom) |
| FPU | Single-precision hardware FPU (no double, no SIMD) |
| On-board SDRAM | 64 MB (IS42S16400J) — "65MB" variant |
| On-board Flash | 8 MB QSPI (IS25LP064A) |
| Audio Codec | AK4556 (rev 4), WM8731 (rev 5), PCM3060 (rev 7) |
| Audio | 24-bit stereo, up to 96 kHz |
| GPIO | 31 configurable pins (12x 16-bit ADC, 2x 12-bit DAC) |
| USB | Micro-USB (power, flashing, debug, serial) |
| Price | ~$30 |

### Board Revisions (Codec)

| Revision | Audio Codec | daisy-embassy Feature | Notes |
|----------|------------|----------------------|-------|
| Rev 4 | AK4556 (AKM) | `seed` | Original. AKM factory fire (2020) ended supply. |
| Rev 5 | WM8731 (Wolfson) | `seed_1_1` (default) | Interim replacement. |
| Rev 7 | PCM3060 (TI) | `seed_1_2` | Current production. |

**Rev 7 noise floor**: The PCM3060 revision has a measurably higher noise floor
than earlier revisions (~15 dB worse in community measurements: 225 µVrms Rev 7
vs 40 µVrms Rev 4). Contributing factors include higher analog voltage (3.6 Vpp
vs 2.1 Vpp) and PCB ground plane design. The PCM3060 datasheet recommends
external low-pass filtering for noise rejection. No official fix from Electrosmith.

**If purchased in 2025-2026, you have Rev 7.** Use `--features=seed_1_2` with
daisy-embassy.

### STM32H750 Memory Map

| Region | Address | Size | Access | Use for |
|--------|---------|------|--------|---------|
| ITCM | 0x0000_0000 | 64 KB | 0-wait, instruction only | Code hot paths |
| DTCM | 0x2000_0000 | 128 KB | 0-wait, data only | Audio buffers, filter state, hot DSP data |
| AXI SRAM | 0x2400_0000 | 512 KB | 0-1 wait | Delay lines, reverb buffers, heap |
| D2 SRAM1 | 0x3000_0000 | 128 KB | 1-2 wait | DMA buffers (SAI audio) |
| D2 SRAM2 | 0x3002_0000 | 128 KB | 1-2 wait | DMA buffers |
| D2 SRAM3 | 0x3004_0000 | 32 KB | 1-2 wait | Small peripheral buffers |
| D3 SRAM4 | 0x3800_0000 | 64 KB | 1-2 wait | Low-power domain |
| External SDRAM | 0xC000_0000 | 64 MB | 4-8 wait | Long delay lines (>500ms) |

**Total internal SRAM: 1 MB.** Key constraint: DTCM is fastest but only 128 KB.
AXI SRAM (512 KB) is the primary working memory for DSP allocations. External
SDRAM is available for large buffers but access latency is 4-8x worse.

### Audio Path

The Daisy Seed uses the STM32H750's SAI (Serial Audio Interface) peripheral
connected to the on-board codec. Audio flows through DMA double-buffering:

```
Codec ADC → SAI RX → DMA → SRAM buffer (ping)
                              ↓
                     Process buffer (pong) ← CPU
                              ↓
           SAI TX ← DMA ← SRAM buffer (ping)  → Codec DAC
```

- **DMA double-buffer**: While CPU processes one half-buffer, DMA fills/drains
  the other. Callback fires at each half-transfer.
- **Default block size**: 32 samples (zlosynth `daisy` crate), configurable to 64.
  At 48 kHz, 32 samples = 0.67 ms latency.
- **Sample rate**: 48 kHz default, 96 kHz optional (feature flag in `daisy` crate).
- **Format**: 24-bit I2S, processed as f32 internally.

## Getting Started

### Prerequisites

1. **Micro USB data cable** — NOT charge-only. Charge-only cables have only 2
   wires (power); data cables have 4 (power + D+/D-). Test: if a cable can
   transfer files to a phone, it's a data cable. Anker, Amazon Basics, and
   CableCreation are reliable brands.

2. **Linux udev rule** for DFU access without sudo:
   ```bash
   sudo tee /etc/udev/rules.d/50-daisy-stm-dfu.rules << 'EOF'
   # STM32 DFU bootloader (Daisy Seed)
   SUBSYSTEMS=="usb", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="df11", MODE="0666", GROUP="plugdev", TAG+="uaccess"
   EOF
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```

3. **dfu-util** for command-line flashing:
   ```bash
   sudo apt install dfu-util   # Ubuntu/Debian
   sudo pacman -S dfu-util     # Arch
   ```

### Step 1: Flash Blink (validate hardware)

This is the officially recommended first step from Electrosmith.

1. Connect Daisy Seed via micro USB data cable
2. Enter DFU mode: **hold BOOT → press/release RESET → release BOOT**
3. Verify: `lsusb` shows `0483:df11 STMicroelectronics STM Device in DFU Mode`
4. Open [flash.daisy.audio](https://flash.daisy.audio/) in **Chrome** (WebUSB, Chrome-only)
5. Click Connect → select "DFU in FS Mode" → Flash Blink
6. **Success**: LED blinks continuously

### Step 2: Rust Toolchain

```bash
rustup target add thumbv7em-none-eabihf
cargo install probe-rs-tools --locked
```

### Step 3: Audio Passthrough

Flash the daisy-embassy passthrough example to verify codec + DMA. See
Implementation Path below.

### Step 4: Single Sonido Effect

Import `sonido-effects` (no_std) and process audio through a single effect.

### Power

The Seed runs on USB power alone for development. External VIN (5–17 V DC)
is only needed inside the Hothouse enclosure or custom PCB. Both USB and VIN
can be connected simultaneously.

## Rust Ecosystem for Daisy Seed

### Chosen Framework: `daisy-embassy`

| | |
|---|---|
| Crate | [`daisy-embassy`](https://crates.io/crates/daisy-embassy) v0.2.3 |
| Repo | https://github.com/daisy-embassy/daisy-embassy |
| Framework | [Embassy](https://embassy.dev/) v0.5.0 (async/await on `embassy-stm32`) |
| License | MIT |
| Status | Active development (last update: Feb 2026, 6 contributors) |

Embassy is where embedded Rust is converging. `embassy-stm32` (345k downloads,
backed by Akiles) provides the HAL with SAI driver and DMA support.
`daisy-embassy` wraps it into Daisy-specific builders handling codec
initialization, clock configuration, and audio DMA automatically.

```rust
// daisy-embassy audio passthrough (simplified)
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = daisy_embassy::default_rcc();
    let p = embassy_stm32::init(config);
    let board = daisy_embassy::DaisySeed::new(/* peripherals */);
    let mut audio = board.audio_interface();

    audio.start(|input, output| {
        // Process audio here — this is where sonido effects run
        for (i, o) in input.iter().zip(output.iter_mut()) {
            *o = effect.process(*i);
        }
    }).await;
}
```

**Known limitation**: Embassy's DMA double-buffering for audio uses a
circular-buffer workaround, not the hardware M0AR/M1AR double-buffer
registers (embassy-rs/embassy#702, open since 2022). daisy-embassy handles
this at the BSP level — it works, but it's not the ideal solution. Monitor
this issue for improvements.

### Why Not `daisy` by Zlosynth?

Zlosynth's [`daisy`](https://crates.io/crates/daisy) crate (v0.11.0) is
production-proven — their [`Kaseta`](https://github.com/zlosynth/kaseta)
Eurorack module ships with it. However, it's built on `stm32h7xx-hal`
(v0.16.0, last updated Mar 2024) which is stale. Embassy-stm32 is actively
maintained and has broader ecosystem support. The Kaseta architecture (thin
firmware crate over no_std DSP library) maps directly to sonido's structure
and influenced our crate design:

```
sonido-core      → dsp/       (no_std primitives)
sonido-effects   → dsp/       (no_std effects)
sonido-platform  → control/   (hardware abstraction)
new: sonido-daisy → firmware/  (Daisy-specific binary)
```

### Foundation Crates (Embassy Path)

| Crate | Version | Purpose |
|-------|---------|---------|
| `embassy-stm32` | 0.5.0 | Async HAL for STM32 (SAI, DMA, GPIO, ADC, I2C) |
| `embassy-executor` | — | Async task executor for Cortex-M |
| `daisy-embassy` | 0.2.3 | Daisy Seed BSP (codec init, audio interface builder) |
| `cortex-m` | — | Low-level Cortex-M access (NVIC, SCB, MPU) |
| `cortex-m-rt` | — | Runtime (vector table, entry point, linker script) |
| `embedded-alloc` | — | Global allocator for heap on AXI SRAM |
| `defmt` | — | Efficient embedded logging (zero-cost when disabled) |
| `probe-rs` | — | Flashing and debugging via SWD/JTAG |

### Build & Flash

```bash
# Install target
rustup target add thumbv7em-none-eabihf

# Install probe-rs for flashing/debugging
cargo install probe-rs-tools --locked

# Build for Daisy Seed
cargo build -p sonido-daisy --target thumbv7em-none-eabihf --release

# Flash via DFU (USB, no debug probe needed)
dfu-util -a 0 -s 0x08000000:leave -D target/thumbv7em-none-eabihf/release/sonido-daisy.bin

# Flash via SWD probe (ST-Link V3 Mini, supports debugging + defmt)
probe-rs run --chip STM32H750IBKx target/thumbv7em-none-eabihf/release/sonido-daisy
```

### Debug Probe (Optional, Recommended)

An **ST-Link V3 Mini** (~$12) connects to the Seed's SWD header and enables
breakpoints, variable inspection, and `defmt` RTT logging via `probe-rs`.
Not needed for initial DFU-based development, but essential for serious
firmware debugging.

Linux udev rules for probe-rs:
```bash
curl -L https://probe.rs/files/69-probe-rs.rules | sudo tee /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload && sudo udevadm trigger
```

## Memory Budget

### Per-Effect Memory Usage

Estimated from sonido source code. Each `InterpolatedDelay` buffer is
`max_delay_samples × 4 bytes` (f32). Struct overhead and SmoothedParams add
~100-200 bytes per effect.

| Effect | Buffer Formula | Memory @ 48 kHz | Notes |
|--------|---------------|-----------------|-------|
| Reverb (stereo) | 8+8 combs + 4+4 allpasses | ~110 KB | Freeverb tunings scaled from 44.1k |
| Reverb (mono) | 8 combs + 4 allpasses | ~55 KB | Half the buffers |
| Delay (2s, stereo) | 96k × 2 × 4B | ~750 KB | **Exceeds AXI SRAM** — needs SDRAM |
| Delay (500ms, stereo) | 24k × 2 × 4B | ~188 KB | Fits in AXI SRAM |
| Delay (300ms, mono) | 14.4k × 4B | ~56 KB | Default delay time |
| Chorus | 960 × 2 × 4B | ~8 KB | 20ms max delay |
| Flanger | ~480 × 2 × 4B | ~4 KB | ~10ms max delay |
| Phaser | 6 allpasses (tiny) | ~1 KB | No delay buffers |
| Distortion | No buffers | <1 KB | Pure waveshaping |
| Compressor | Envelope state | <1 KB | |
| All other effects | Minimal | <1 KB each | Gate, Tremolo, Wah, Preamp, Filter, TapeSat |

### Memory Placement Strategy

```
DTCM (128 KB, 0-wait):
  ├── Audio DMA buffers (256 samples × 2 ch × 4B = 2 KB)
  ├── Stack (8-16 KB)
  ├── SmoothedParam arrays, filter coefficients
  ├── Hot per-sample state (biquad, SVF, envelope follower)
  └── ~100 KB available for small effect state

AXI SRAM (512 KB, 0-1 wait):
  ├── Reverb buffers (~110 KB stereo)
  ├── Chorus delay lines (~8 KB)
  ├── Flanger delay lines (~4 KB)
  ├── Short delay lines (<500ms = <188 KB)
  ├── Heap allocator for dynamic chains
  └── ~200 KB headroom for effect chains

External SDRAM (64 MB, 4-8 wait):
  ├── Long delay lines (>500ms)
  ├── Sampler/looper buffers
  └── Large lookup tables
```

### Chain Configurations That Fit

**Comfortable (total memory < 300 KB, fits in AXI SRAM with headroom):**

| Chain | Memory Est. | CPU Est. (CM7) |
|-------|------------|----------------|
| Preamp → Distortion → Chorus → Delay(300ms) | ~65 KB | ~30% |
| Gate → TapeSat → Flanger → Delay(300ms) | ~62 KB | ~22% |
| Preamp → Wah → Distortion → Chorus | ~10 KB | ~32% |
| Preamp → TapeSat → Tremolo → Delay(300ms) | ~58 KB | ~19% |

**Tight (memory OK, CPU 50-80%):**

| Chain | Memory Est. | CPU Est. (CM7) |
|-------|------------|----------------|
| Preamp → Distortion → Chorus → Delay → Reverb(mono) | ~120 KB | ~78% |
| Compressor → Distortion → Reverb(mono) | ~57 KB | ~77% |

**Needs SDRAM:**

| Chain | Memory Est. | Notes |
|-------|------------|-------|
| Any chain with Delay > 500ms stereo | > 188 KB delay alone | Move delay buffer to SDRAM |
| Reverb(stereo) + Delay(2s) | ~860 KB | SDRAM required |

**Does not fit (CPU budget exceeded):**
- Eq or Vibrato (>100% CPU alone)
- Phaser + Reverb (>124% CPU)
- See `docs/BENCHMARKS.md` for full Cortex-M7 cycle estimates

## Hothouse Platform Mapping

The Cleveland Music Co. Hothouse is a DIY pedal enclosure for Daisy Seed.
See `docs/HARDWARE.md` for full pin mapping and control details.

| Hothouse Control | Daisy Pin | sonido Mapping |
|-----------------|-----------|----------------|
| KNOB_1–6 | PIN_21–25, 28 (ADC) | `ControlId::hardware(0x00..0x05)` |
| TOGGLE_1–3 | PIN_5–10 (GPIO) | `ControlId::hardware(0x10..0x12)` |
| FOOTSWITCH_1–2 | PIN_27, 14 (GPIO) | `ControlId::hardware(0x20..0x21)` |
| LED_1–2 | PIN_4, 3 (GPIO) | `ControlId::hardware(0x30..0x31)` |

The `PlatformController` trait (`crates/sonido-platform/src/lib.rs`) maps
directly to Hothouse's physical controls. A Daisy firmware implementation would:

1. Implement `PlatformController` reading ADC/GPIO pins
2. Use `ControlMapper` to route controls to effect parameters
3. Process audio in the DMA callback using sonido's `Effect` trait

### Preset System

27 preset slots via 3× three-way toggles (3^3 = 27 combinations). Each preset
maps 6 knobs to effect parameters. No display needed — LED blink patterns
indicate state.

## Implementation Path

### Hardware Available

- **Daisy Seed** (65 MB, Rev 7 / PCM3060) — acquired Feb 2026
- **Hothouse kit** (Cleveland Music Co.) — ordered, arriving separately

Phases 1-2 can be done on the bare Seed with USB audio or breadboard wiring.
Phase 3+ requires the Hothouse enclosure for proper control surface testing.

### Phase 1: Audio Passthrough (bare Seed)

1. Create `crates/sonido-daisy/` binary crate with `daisy-embassy` dependency
2. Configure Embassy executor, clock (`default_rcc()`), and audio interface
3. Audio callback: copy input → output (verify codec + DMA)
4. Flash via DFU (`dfu-util`) and test
5. **Success**: clean passthrough with no audible artifacts

### Phase 2: Single Effect (bare Seed)

1. Import `sonido-effects` (no_std, no-default-features)
2. Configure `embedded-alloc` global allocator on AXI SRAM
3. Instantiate one effect (e.g., `Distortion`) — test with static allocation first
4. Process audio through effect in callback
5. Wire one ADC pin to drive parameter (breadboard pot or fixed resistor)
6. **Success**: real-time audio, no glitches, parameter changes audible

### Phase 3: Effect Chain + Controls (Hothouse)

1. Implement `PlatformController` for Hothouse pin layout (ADC, GPIO)
2. Wire `ControlMapper` to route 6 knobs → effect parameters
3. Build a fixed effect chain (e.g., Preamp → Distortion → Chorus → Delay)
4. Toggle-based preset selection (27 configurations via 3 three-way toggles)
5. Footswitch bypass with LED feedback

### Phase 4: Dynamic Chains (Hothouse)

1. Allocate effect chain from heap (AXI SRAM, SDRAM for large buffers)
2. Toggle combinations select different chains
3. Memory-aware chain builder (refuse chains that exceed budget)
4. CPU usage monitoring (DWT cycle counter)

## Critical Constraints

### What sonido already handles

- All 5 no_std crates compile and pass tests with `--no-default-features`
- All 5 no_std crates cross-compile for `thumbv7em-none-eabihf` (verified Feb 2026)
- `Effect` trait is hardware-agnostic
- `process_block()` and `process_block_stereo()` match DMA callback pattern
- `ParameterInfo` enables runtime parameter discovery for control mapping
- `libm` used everywhere (no `f32::sin()` in no_std crates)
- `alloc` support: delay buffers use `alloc::vec::Vec` with conditional
  `extern crate alloc` — works with any global allocator

### What needs work

- **Global allocator for firmware**: The `sonido-daisy` binary crate must
  configure [`embedded-alloc`](https://crates.io/crates/embedded-alloc)
  pointing at AXI SRAM (512 KB) for heap allocation. Effects that use
  `InterpolatedDelay` (Vec-backed) require this.
- **Memory placement**: Large buffers (reverb ~110 KB, delay >188 KB) should
  live in AXI SRAM or SDRAM. May need custom allocator regions or linker
  section attributes for SDRAM-backed delay lines.
- **No `process_block()` in core primitives**: Some primitives (Biquad, SVF) only
  have per-sample `process()`. Block processing would improve cache behavior on CM7.
- **SmoothedParam no short-circuit**: When settled, still computes exponential
  smoothing per sample. Optimization target for tight CPU budgets.

## References

### Electrosmith Official
- [Daisy Seed Product Page](https://electro-smith.com/products/daisy-seed)
- [Daisy Seed Datasheet v1.1.5](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/Daisy_Seed_datasheet_v1-1-5.pdf)
- [Memory Variant Comparison](https://electro-smith.com/pages/memory-what-is-the-difference)
- [Web Programmer](https://flash.daisy.audio/) — browser-based firmware flashing
- [Web Programmer Tutorial](https://daisy.audio/tutorials/web-programmer/)
- [Getting Started Forum Post](https://forum.electro-smith.com/t/welcome-to-daisy-get-started-here/15)
- [Troubleshooting](https://daisy.audio/troubleshooting/)
- [libDaisy (C++)](https://github.com/electro-smith/libDaisy)
- [STM32H750 Product Page](https://www.st.com/en/microcontrollers-microprocessors/stm32h750-value-line.html)

### Rust Ecosystem
- [daisy-embassy crate](https://crates.io/crates/daisy-embassy) — Chosen BSP
- [daisy-embassy repo](https://github.com/daisy-embassy/daisy-embassy)
- [Embassy STM32H750XB docs](https://docs.embassy.dev/embassy-stm32/git/stm32h750xb/index.html)
- [Embassy DMA double-buffer issue](https://github.com/embassy-rs/embassy/issues/702)
- [zlosynth/daisy crate](https://github.com/zlosynth/daisy) — Alternative BSP
- [zlosynth/kaseta](https://github.com/zlosynth/kaseta) — Production Rust DSP on Daisy
- [probe-rs Installation](https://probe.rs/docs/getting-started/installation/)
- [probe-rs Probe Setup](https://probe.rs/docs/getting-started/probe-setup/)

### Community
- [Daisy Forum: Rust development](https://forum.electro-smith.com/t/rust-starter-for-daisy-seed/684)
- [Daisy Forum: Rev 7 noise floor](https://forum.electro-smith.com/t/rev-7-noise-floor-vs-rev-4/4943)
- [Daisy Forum: Rev 7 detection bug](https://forum.electro-smith.com/t/rev-7-seed-is-detected-as-rev-4/4876)

### Sonido Internal
- [Hothouse Hardware Reference](HARDWARE.md)
- [Sonido Benchmarks & Cortex-M7 Estimates](BENCHMARKS.md)
