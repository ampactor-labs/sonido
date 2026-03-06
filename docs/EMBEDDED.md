# Embedded Guide

Hardware targets, platform abstraction, and deployment guide for running Sonido on
embedded systems. Covers the Electrosmith Daisy Seed (STM32H750 Cortex-M7) and the
Cleveland Music Co. Hothouse DIY pedal platform.

---

## Hardware Targets

### Daisy Seed

| Spec | Value |
|------|-------|
| MCU | STM32H750IBK6 (ARM Cortex-M7, single core) |
| Clock | 480 MHz (libDaisy defaults to 400 MHz for thermal headroom) |
| FPU | Single-precision hardware FPU (no double, no SIMD) |
| On-board SDRAM | 64 MB (IS42S16400J) -- "65MB" variant |
| On-board Flash | 8 MB QSPI (IS25LP064A) |
| Audio Codec | AK4556 (rev 4), WM8731 (rev 5), PCM3060 (rev 7) |
| Audio | 24-bit stereo, up to 96 kHz |
| GPIO | 31 configurable pins (12x 16-bit ADC, 2x 12-bit DAC) |
| USB | Micro-USB (power, flashing, debug, serial) |
| Price | ~$30 |

#### Board Revisions (Codec)

| Revision | Audio Codec | daisy-embassy Feature | Notes |
|----------|------------|----------------------|-------|
| Rev 4 | AK4556 (AKM) | `seed` | Original. AKM factory fire (2020) ended supply. |
| Rev 5 | WM8731 (Wolfson) | `seed_1_1` (default) | Interim replacement. |
| Rev 7 | PCM3060 (TI) | `seed_1_2` | Current production. |

**Rev 7 noise floor**: The PCM3060 revision has a measurably higher noise floor
than earlier revisions (~15 dB worse in community measurements: 225 uVrms Rev 7
vs 40 uVrms Rev 4). Contributing factors include higher analog voltage (3.6 Vpp
vs 2.1 Vpp) and PCB ground plane design. No official fix from Electrosmith.

**If purchased in 2025-2026, you have Rev 7.** Use `--features=seed_1_2` with
daisy-embassy.

#### Memory Map (STM32H750)

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
AXI SRAM (512 KB) is the primary working memory for DSP allocations.

#### Audio Path

The Daisy Seed uses the STM32H750's SAI (Serial Audio Interface) peripheral
connected to the on-board codec. Audio flows through DMA double-buffering:

```
Codec ADC -> SAI RX -> DMA -> SRAM buffer (ping)
                               |
                      Process buffer (pong) <- CPU
                               |
          SAI TX <- DMA <- SRAM buffer (ping) -> Codec DAC
```

- **DMA double-buffer**: While CPU processes one half-buffer, DMA fills/drains
  the other. Callback fires at each half-transfer.
- **Default block size**: 32 samples, configurable to 64. At 48 kHz, 32 samples = 0.67 ms latency.
- **Sample rate**: 48 kHz default, 96 kHz optional.
- **Format**: 24-bit I2S, processed as f32 internally.

---

### Hothouse

The Cleveland Music Co. Hothouse is a DIY pedal enclosure for Daisy Seed.

#### Physical Controls

| Control | Type | ADC/GPIO | Values | Notes |
|---------|------|----------|--------|-------|
| KNOB_1 | 10K pot | ADC | 0.0--1.0 float | Top left |
| KNOB_2 | 10K pot | ADC | 0.0--1.0 float | Top center |
| KNOB_3 | 10K pot | ADC | 0.0--1.0 float | Top right |
| KNOB_4 | 10K pot | ADC | 0.0--1.0 float | Bottom left |
| KNOB_5 | 10K pot | ADC | 0.0--1.0 float | Bottom center |
| KNOB_6 | 10K pot | ADC | 0.0--1.0 float | Bottom right |
| TOGGLE_1 | 3-way (ON-OFF-ON) | GPIO | UP / MIDDLE / DOWN | Left toggle |
| TOGGLE_2 | 3-way (ON-OFF-ON) | GPIO | UP / MIDDLE / DOWN | Center toggle |
| TOGGLE_3 | 3-way (ON-OFF-ON) | GPIO | UP / MIDDLE / DOWN | Right toggle |
| FOOTSWITCH_1 | Momentary | GPIO | pressed / released | Left footswitch |
| FOOTSWITCH_2 | Momentary | GPIO | pressed / released | Right footswitch |
| LED_1 | Status LED | GPIO | on / off | Left LED |
| LED_2 | Status LED | GPIO | on / off | Right LED |

#### Audio I/O

| Port | Type | Channels | Level |
|------|------|----------|-------|
| INPUT | 1/4" TRS | Stereo (tip=L, ring=R) | Instrument level |
| OUTPUT | 1/4" TRS | Stereo (tip=L, ring=R) | Instrument level |

**Audio modes** (software-defined): Mono in/Mono out, Mono in/Stereo out,
Stereo in/Stereo out, Mono in/Dual mono out.

#### Control Combinatorics

| Controls | States | Use case |
|----------|--------|----------|
| 3 toggles (3-way each) | 27 combinations | Effect/bank selection |
| 6 knobs | Continuous | Per-effect parameters |
| 2 footswitches | 4 combinations | Bypass, tap, preset |

#### Pin Mapping (Daisy Seed)

```
Knobs (ADC):
  KNOB_1 = PIN_21 (A0)
  KNOB_2 = PIN_22 (A1)
  KNOB_3 = PIN_23 (A2)
  KNOB_4 = PIN_24 (A3)
  KNOB_5 = PIN_25 (A4)
  KNOB_6 = PIN_28 (A5)

Toggles (GPIO):
  TOGGLE_1_UP   = PIN_5
  TOGGLE_1_DOWN = PIN_6
  TOGGLE_2_UP   = PIN_7
  TOGGLE_2_DOWN = PIN_8
  TOGGLE_3_UP   = PIN_9
  TOGGLE_3_DOWN = PIN_10

Footswitches (GPIO):
  FOOTSWITCH_1 = PIN_27
  FOOTSWITCH_2 = PIN_14

LEDs (GPIO):
  LED_1 = PIN_4
  LED_2 = PIN_3

Audio (Codec):
  AUDIO_IN_L  = Seed audio in L
  AUDIO_IN_R  = Seed audio in R
  AUDIO_OUT_L = Seed audio out L
  AUDIO_OUT_R = Seed audio out R
```

#### Free Pins for Expansion

| Pin | Function | Suggested use |
|-----|----------|---------------|
| D11 | I2C SCL | OLED display |
| D12 | I2C SDA | OLED display |
| D13 | UART TX | MIDI out |
| D14 | UART RX | MIDI in |

#### Signal Level Limitations

**Designed for:** Instrument level (100mV -- 1V peak-to-peak)

| Source | Level | Compatibility |
|--------|-------|---------------|
| Guitar (passive) | ~200mV p-p | Optimal |
| Guitar (active) | ~500mV p-p | Fine |
| Bass | ~300mV p-p | Fine |
| Synth line out | ~2.8V p-p | Too hot, turn down or pad |
| Eurorack | 5--10V p-p | Will clip hard |

**Impedance:** 1M Ohm input (guitar pickup optimized), may affect tone from low-Z sources.

For hot signals: use external attenuator, reamp box, or turn down source volume.

---

## PlatformController

The `PlatformController` trait (`crates/sonido-platform/src/lib.rs`) maps directly to
Hothouse's physical controls. A Daisy firmware implementation:

1. Implements `PlatformController` reading ADC/GPIO pins
2. Uses `ControlMapper` to route controls to effect parameters
3. Processes audio in the DMA callback using sonido's `Effect` trait

### Hothouse Control Mapping

| Hothouse Control | Daisy Pin | sonido Mapping |
|-----------------|-----------|----------------|
| KNOB_1--6 | PIN_21--25, 28 (ADC) | `ControlId::hardware(0x00..0x05)` |
| TOGGLE_1--3 | PIN_5--10 (GPIO) | `ControlId::hardware(0x10..0x12)` |
| FOOTSWITCH_1--2 | PIN_27, 14 (GPIO) | `ControlId::hardware(0x20..0x21)` |
| LED_1--2 | PIN_4, 3 (GPIO) | `ControlId::hardware(0x30..0x31)` |

### Preset System

27 preset slots via 3x three-way toggles (3^3 = 27 combinations). Each preset
maps 6 knobs to effect parameters. No display needed -- LED blink patterns
indicate state.

---

## Software Considerations

### Debouncing

Toggles and footswitches need software debounce (~20-50ms).

### Knob Smoothing

ADC values jitter; apply exponential smoothing or hysteresis.

### Toggle Reading

Each 3-way toggle uses 2 GPIO pins:
```rust
match (up_pin, down_pin) {
    (true, false)  => Position::Up,
    (false, false) => Position::Middle,
    (false, true)  => Position::Down,
    _ => unreachable!(), // both true = hardware fault
}
```

### Footswitch Modes

- Momentary: Read state directly
- Latching (software): Toggle internal state on press
- Long-press: Detect hold duration for secondary function

---

## Memory Budget

### Per-Effect Memory Usage

Estimated from sonido source code. Each `InterpolatedDelay` buffer is
`max_delay_samples x 4 bytes` (f32).

| Effect | Buffer Formula | Memory @ 48 kHz | Notes |
|--------|---------------|-----------------|-------|
| Reverb (stereo) | 8+8 combs + 4+4 allpasses | ~110 KB | Freeverb tunings scaled from 44.1k |
| Reverb (mono) | 8 combs + 4 allpasses | ~55 KB | Half the buffers |
| Delay (2s, stereo) | 96k x 2 x 4B | ~750 KB | **Exceeds AXI SRAM** -- needs SDRAM |
| Delay (500ms, stereo) | 24k x 2 x 4B | ~188 KB | Fits in AXI SRAM |
| Delay (300ms, mono) | 14.4k x 4B | ~56 KB | Default delay time |
| Chorus | 960 x 2 x 4B | ~8 KB | 20ms max delay |
| Flanger | ~480 x 2 x 4B | ~4 KB | ~10ms max delay |
| Phaser | 6 allpasses (tiny) | ~1 KB | No delay buffers |
| Distortion | No buffers | <1 KB | Pure waveshaping |
| Compressor | Envelope state | <1 KB | |
| All other effects | Minimal | <1 KB each | Gate, Tremolo, Wah, Preamp, Filter, Tape |

### Memory Placement Strategy

```
DTCM (128 KB, 0-wait):
  +-- Audio DMA buffers (256 samples x 2 ch x 4B = 2 KB)
  +-- Stack (8-16 KB)
  +-- SmoothedParam arrays, filter coefficients
  +-- Hot per-sample state (biquad, SVF, envelope follower)
  +-- ~100 KB available for small effect state

AXI SRAM (512 KB, 0-1 wait):
  +-- Reverb buffers (~110 KB stereo)
  +-- Chorus delay lines (~8 KB)
  +-- Flanger delay lines (~4 KB)
  +-- Short delay lines (<500ms = <188 KB)
  +-- Heap allocator for dynamic chains
  +-- ~200 KB headroom for effect chains

External SDRAM (64 MB, 4-8 wait):
  +-- Long delay lines (>500ms)
  +-- Sampler/looper buffers
  +-- Large lookup tables
```

### Chain Configurations That Fit

**Comfortable (total memory < 300 KB, fits in AXI SRAM with headroom):**

| Chain | Memory Est. | CPU Est. (CM7) |
|-------|------------|----------------|
| Preamp -> Distortion -> Chorus -> Delay(300ms) | ~65 KB | ~30% |
| Gate -> Tape -> Flanger -> Delay(300ms) | ~62 KB | ~22% |
| Preamp -> Wah -> Distortion -> Chorus | ~10 KB | ~32% |
| Preamp -> Tape -> Tremolo -> Delay(300ms) | ~58 KB | ~19% |

**Tight (memory OK, CPU 50-80%):**

| Chain | Memory Est. | CPU Est. (CM7) |
|-------|------------|----------------|
| Preamp -> Distortion -> Chorus -> Delay -> Reverb(mono) | ~120 KB | ~78% |
| Compressor -> Distortion -> Reverb(mono) | ~57 KB | ~77% |

**Needs SDRAM:**

| Chain | Memory Est. | Notes |
|-------|------------|-------|
| Any chain with Delay > 500ms stereo | > 188 KB delay alone | Move delay buffer to SDRAM |
| Reverb(stereo) + Delay(2s) | ~860 KB | SDRAM required |

**Does not fit (CPU budget exceeded):**
- Eq or Vibrato (>100% CPU alone)
- Phaser + Reverb (>124% CPU)
- See `docs/BENCHMARKS.md` for full Cortex-M7 cycle estimates

---

## Design Patterns

### Bank + Preset System

```
TOGGLE_1 = Bank (A / B / C)
TOGGLE_2 = Preset within bank (1 / 2 / 3)
TOGGLE_3 = Modifier (normal / alt / extended)

Total: 27 presets accessible without menus
```

### Parameter Pages (No Display)

```
FOOTSWITCH_2 long-press = cycle parameter page
LED_2 blink pattern = indicate current page
KNOB_1-6 = different params per page
```

### Tap Tempo

```
FOOTSWITCH_1 tap = record interval
FOOTSWITCH_1 hold = reset to default
LED_1 blink = tempo indicator
```

---

## Rust Ecosystem for Daisy Seed

### Chosen Framework: daisy-embassy

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
        for (i, o) in input.iter().zip(output.iter_mut()) {
            *o = effect.process(*i);
        }
    }).await;
}
```

**Known limitation**: Embassy's DMA double-buffering for audio uses a
circular-buffer workaround, not the hardware M0AR/M1AR double-buffer
registers (embassy-rs/embassy#702, open since 2022).

### Why Not `daisy` by Zlosynth?

Zlosynth's [`daisy`](https://crates.io/crates/daisy) crate (v0.11.0) is
production-proven (their Kaseta Eurorack module ships with it), but it's built
on `stm32h7xx-hal` (v0.16.0, last updated Mar 2024) which is stale. Embassy-stm32
is actively maintained with broader ecosystem support. The Kaseta architecture
(thin firmware crate over no_std DSP library) maps directly to sonido's structure:

```
sonido-core      -> dsp/       (no_std primitives)
sonido-effects   -> dsp/       (no_std effects)
sonido-platform  -> control/   (hardware abstraction)
new: sonido-daisy -> firmware/ (Daisy-specific binary)
```

### Foundation Crates (Embassy Path)

| Crate | Version | Purpose |
|-------|---------|---------|
| `embassy-stm32` | 0.5.0 | Async HAL for STM32 (SAI, DMA, GPIO, ADC, I2C) |
| `embassy-executor` | -- | Async task executor for Cortex-M |
| `daisy-embassy` | 0.2.3 | Daisy Seed BSP (codec init, audio interface builder) |
| `cortex-m` | -- | Low-level Cortex-M access (NVIC, SCB, MPU) |
| `cortex-m-rt` | -- | Runtime (vector table, entry point, linker script) |
| `embedded-alloc` | -- | Global allocator for heap on AXI SRAM |
| `defmt` | -- | Efficient embedded logging (zero-cost when disabled) |
| `probe-rs` | -- | Flashing and debugging via SWD/JTAG |

---

## Getting Started

### Prerequisites

1. **Micro USB data cable** -- NOT charge-only. Charge-only cables have only 2
   wires (power); data cables have 4 (power + D+/D-).

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

### Step 1: Flash Blink (Validate Hardware)

1. Connect Daisy Seed via micro USB data cable
2. Enter DFU mode: **hold BOOT -> press/release RESET -> release BOOT**
3. Verify: `lsusb` shows `0483:df11 STMicroelectronics STM Device in DFU Mode`
4. Open [flash.daisy.audio](https://flash.daisy.audio/) in **Chrome** (WebUSB, Chrome-only)
5. Click Connect -> select "DFU in FS Mode" -> Flash Blink
6. **Success**: LED blinks continuously

### Step 2: Rust Toolchain

```bash
rustup target add thumbv7em-none-eabihf
cargo install probe-rs-tools --locked
```

### Step 3: Audio Passthrough

Flash the daisy-embassy passthrough example to verify codec + DMA.

### Step 4: Single Sonido Effect

Import `sonido-effects` (no_std) and process audio through a single effect.

### Build & Flash

```bash
# Build for Daisy Seed
cargo build -p sonido-daisy --target thumbv7em-none-eabihf --release

# Flash via DFU (USB, no debug probe needed)
dfu-util -a 0 -s 0x08000000:leave -D target/thumbv7em-none-eabihf/release/sonido-daisy.bin

# Flash via SWD probe (supports debugging + defmt)
probe-rs run --chip STM32H750IBKx target/thumbv7em-none-eabihf/release/sonido-daisy
```

### Debug Probe (Optional, Recommended)

An **ST-Link V3 Mini** (~$12) connects to the Seed's SWD header and enables
breakpoints, variable inspection, and `defmt` RTT logging via `probe-rs`.

Linux udev rules for probe-rs:
```bash
curl -L https://probe.rs/files/69-probe-rs.rules | sudo tee /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload && sudo udevadm trigger
```

### Power

The Seed runs on USB power alone for development. External VIN (5--17 V DC)
is only needed inside the Hothouse enclosure or custom PCB. Both USB and VIN
can be connected simultaneously.

---

## Implementation Path

### Hardware Available

- **Daisy Seed** (65 MB, Rev 7 / PCM3060) -- acquired Feb 2026
- **Hothouse kit** (Cleveland Music Co.) -- ordered, arriving separately

Phases 1-2 can be done on the bare Seed with USB audio or breadboard wiring.
Phase 3+ requires the Hothouse enclosure for proper control surface testing.

### Phase 1: Audio Passthrough (Bare Seed)

1. Create `crates/sonido-daisy/` binary crate with `daisy-embassy` dependency
2. Configure Embassy executor, clock (`default_rcc()`), and audio interface
3. Audio callback: copy input -> output (verify codec + DMA)
4. Flash via DFU and test
5. **Success**: clean passthrough with no audible artifacts

### Phase 2: Single Effect (Bare Seed)

1. Import `sonido-effects` (no_std, no-default-features)
2. Configure `embedded-alloc` global allocator on AXI SRAM
3. Instantiate one effect (e.g., Distortion) -- test with static allocation first
4. Process audio through effect in callback
5. Wire one ADC pin to drive parameter (breadboard pot or fixed resistor)
6. **Success**: real-time audio, no glitches, parameter changes audible

### Phase 3: Effect Chain + Controls (Hothouse)

1. Implement `PlatformController` for Hothouse pin layout (ADC, GPIO)
2. Wire `ControlMapper` to route 6 knobs -> effect parameters
3. Build a fixed effect chain (e.g., Preamp -> Distortion -> Chorus -> Delay)
4. Toggle-based preset selection (27 configurations via 3 three-way toggles)
5. Footswitch bypass with LED feedback

### Phase 4: Dynamic Chains (Hothouse)

1. Allocate effect chain from heap (AXI SRAM, SDRAM for large buffers)
2. Toggle combinations select different chains
3. Memory-aware chain builder (refuse chains that exceed budget)
4. CPU usage monitoring (DWT cycle counter)

---

## Critical Constraints

### What Sonido Already Handles

- All 5 no_std crates compile and pass tests with `--no-default-features`
- All 5 no_std crates cross-compile for `thumbv7em-none-eabihf`
- `Effect` trait is hardware-agnostic
- `process_block()` and `process_block_stereo()` match DMA callback pattern
- `ParameterInfo` enables runtime parameter discovery for control mapping
- `libm` used everywhere (no `f32::sin()` in no_std crates)
- `alloc` support: delay buffers use `alloc::vec::Vec` with conditional
  `extern crate alloc` -- works with any global allocator

### What Needs Work

- **Global allocator for firmware**: The `sonido-daisy` binary crate must
  configure `embedded-alloc` pointing at AXI SRAM (512 KB) for heap allocation.
- **Memory placement**: Large buffers (reverb ~110 KB, delay >188 KB) may need
  custom allocator regions or linker section attributes for SDRAM-backed delay lines.
- **No `process_block()` in core primitives**: Some primitives (Biquad, SVF) only
  have per-sample `process()`. Block processing would improve cache behavior on CM7.
- **SmoothedParam no short-circuit**: When settled, still computes exponential
  smoothing per sample. Optimization target for tight CPU budgets.

---

## References

### Electrosmith Official
- [Daisy Seed Product Page](https://electro-smith.com/products/daisy-seed)
- [Daisy Seed Datasheet v1.1.5](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/Daisy_Seed_datasheet_v1-1-5.pdf)
- [Memory Variant Comparison](https://electro-smith.com/pages/memory-what-is-the-difference)
- [Web Programmer](https://flash.daisy.audio/) -- browser-based firmware flashing
- [Web Programmer Tutorial](https://daisy.audio/tutorials/web-programmer/)
- [Getting Started Forum Post](https://forum.electro-smith.com/t/welcome-to-daisy-get-started-here/15)
- [Troubleshooting](https://daisy.audio/troubleshooting/)
- [libDaisy (C++)](https://github.com/electro-smith/libDaisy)
- [STM32H750 Product Page](https://www.st.com/en/microcontrollers-microprocessors/stm32h750-value-line.html)

### Rust Ecosystem
- [daisy-embassy crate](https://crates.io/crates/daisy-embassy) -- Chosen BSP
- [daisy-embassy repo](https://github.com/daisy-embassy/daisy-embassy)
- [Embassy STM32H750XB docs](https://docs.embassy.dev/embassy-stm32/git/stm32h750xb/index.html)
- [Embassy DMA double-buffer issue](https://github.com/embassy-rs/embassy/issues/702)
- [zlosynth/daisy crate](https://github.com/zlosynth/daisy) -- Alternative BSP
- [zlosynth/kaseta](https://github.com/zlosynth/kaseta) -- Production Rust DSP on Daisy
- [probe-rs Installation](https://probe.rs/docs/getting-started/installation/)
- [probe-rs Probe Setup](https://probe.rs/docs/getting-started/probe-setup/)

### Community
- [Daisy Forum: Rust development](https://forum.electro-smith.com/t/rust-starter-for-daisy-seed/684)
- [Daisy Forum: Rev 7 noise floor](https://forum.electro-smith.com/t/rev-7-noise-floor-vs-rev-4/4943)
- [Daisy Forum: Rev 7 detection bug](https://forum.electro-smith.com/t/rev-7-seed-is-detected-as-rev-4/4876)

### Sonido Internal
- [Kernel Architecture](KERNEL_ARCHITECTURE.md) -- DspKernel/KernelParams patterns and adding-new-effect checklist
- [Benchmarks & Cortex-M7 Estimates](BENCHMARKS.md)
- [Architecture Overview](ARCHITECTURE.md)

---

## Hardware Interface Gaps

Production pedal deployment requires hardware interface features not yet implemented.
These items are tracked in `docs/ROADMAP.md` (Embedded Hardening section).

### Expression Pedal Input

TRS expression pedals output a variable voltage (typically 0-3.3V) via a potentiometer
wiper. The ADC reads this as a continuous value, but real expression pedals have:

- **Calibration variance**: Different pedals have different min/max ADC values
  (some only sweep 0.05-0.92). Heel/toe calibration stores per-pedal endpoints.
- **Response curves**: Linear voltage ≠ linear perceived response for many parameters.
  A logarithmic curve for volume, S-curve for wah sweep, or custom LUT for scene morphing.
- **Polarity detection**: Some pedals are wired tip-hot, others ring-hot. Auto-detect
  on first sweep or manual toggle.

Implementation: `ControlType::Expression` in `sonido-platform`, with `ExpressionConfig`
struct holding calibration + curve + polarity.

### CV Input (Eurorack Crossover)

Eurorack CV signals are ±5V (bipolar) or 0-5V (unipolar). The Daisy Seed's ADC reads
0-3.3V, so external conditioning (voltage divider + offset) is needed for bipolar signals.

Implementation: `ControlType::CvInput` with voltage range, scaling, and offset parameters.
The `ControlMapper` maps conditioned ADC values to kernel parameters.

### MIDI CC Routing

MIDI CC messages arrive via UART (pins D13/D14 on Daisy Seed, exposed on Hothouse).
Routing maps CC numbers to effect parameters via `ControlId::midi(0x02XX)`.

Features needed:
- CC learn mode (footswitch-triggered)
- Program Change → preset recall
- MIDI Clock → `TempoManager` sync
- Running status parsing for bandwidth efficiency

### Pot Calibration and Dead Zones

Real potentiometers don't sweep 0.0-1.0. Typical range is 0.003-0.991, and the
relationship between rotation and resistance is only approximately linear.

Per-pot calibration:
- Store min/max ADC values per knob in flash
- Remap raw ADC to 0.0-1.0 within calibrated range
- Dead zones: ignore movement below a threshold near min/max (prevents jitter)
- Hysteresis: require N-step ADC change before updating (noise rejection)

### Control Curves

Different parameters need different response curves from the same physical knob:
- **Linear**: Most parameters (depth, mix, rate)
- **Logarithmic**: Frequency, volume (perceptual linearity)
- **Reverse log**: Attack/release times
- **S-curve**: Crossfade, morph position
- **Custom LUT**: 16-32 point lookup table for arbitrary response

Applied in `ControlMapper::map_control()` after calibration, before parameter dispatch.

### Parameter Pages

6 knobs × 1 page = 6 parameters. Most effects have 5-11 parameters. Parameter pages
multiply the available controls:

- Footswitch long-press (>500ms) cycles pages
- LED blink count indicates current page
- Knob pickup: when switching pages, knobs don't jump — the parameter only updates
  when the knob crosses the stored value (prevents audio glitches on page change)
- Typical layout: Page 1 = primary controls, Page 2 = secondary/modifier, Page 3 = utility

### Debounce

Mechanical switches bounce for 5-30ms after state change. Without debounce, a single
press registers as multiple events.

Implementation: `Debouncer<const N: usize>` struct in `sonido-platform`:
- Configurable debounce window (default 30ms)
- Edge-triggered mode for footswitches (detect press/release)
- Level-triggered mode for toggles (detect stable position)
- Generic over pin count (N) for batch processing

---

## See Also

- [Architecture](ARCHITECTURE.md) -- Crate dependency graph and design overview
- [Kernel Architecture](KERNEL_ARCHITECTURE.md) -- Kernel patterns and new-effect checklist
- [Benchmarks](BENCHMARKS.md) -- Performance data and Cortex-M7 cycle estimates
