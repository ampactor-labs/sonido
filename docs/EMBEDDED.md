# Embedded Guide

Deploying Sonido on the Electrosmith Daisy Seed (STM32H750 Cortex-M7) and the
Cleveland Music Co. Hothouse DIY pedal platform.

> **Current hardware:** Daisy Seed 65 MB (Rev 7 / PCM3060), bare board + USB.
> Hothouse kit arriving separately — Phases 3-4 require it.

---

## Daisy Seed

| Spec | Value |
|------|-------|
| MCU | STM32H750IBK6 (ARM Cortex-M7, single core) |
| Clock | 480 MHz (libDaisy defaults to 400 MHz for thermal headroom) |
| FPU | Single-precision hardware FPU (no double, no SIMD) |
| SDRAM | 64 MB (IS42S16400J) — "65 MB" variant |
| QSPI Flash | 8 MB (IS25LP064A) |
| Audio Codec | PCM3060 (TI) — Rev 7, current production |
| Audio | 24-bit stereo, up to 96 kHz |
| GPIO | 31 configurable pins (12x 16-bit ADC, 2x 12-bit DAC) |
| USB | Micro-USB (power, flashing, debug, serial) |

> **Rev 7 noise floor:** ~15 dB worse than Rev 4 (225 uVrms vs 40 uVrms).
> Contributing factors: higher analog voltage (3.6 Vpp vs 2.1 Vpp) and PCB
> ground plane design. No official fix from Electrosmith.
> Use `--features=seed_1_2` with daisy-embassy.

### Memory Map

| Region | Address | Size | Wait States | Use |
|--------|---------|------|:-----------:|-----|
| ITCM | `0x0000_0000` | 64 KB | 0 (instruction only) | Code hot paths |
| DTCM | `0x2000_0000` | 128 KB | 0 (data only) | Audio buffers, stack, hot DSP state |
| AXI SRAM | `0x2400_0000` | 512 KB | 0–1 | Delay lines, reverb buffers, heap |
| D2 SRAM1 | `0x3000_0000` | 128 KB | 1–2 | DMA buffers (SAI audio) |
| D2 SRAM2 | `0x3002_0000` | 128 KB | 1–2 | DMA buffers |
| D2 SRAM3 | `0x3004_0000` | 32 KB | 1–2 | Small peripheral buffers |
| D3 SRAM4 | `0x3800_0000` | 64 KB | 1–2 | Low-power domain |
| SDRAM | `0xC000_0000` | 64 MB | 4–8 | Long delay lines (>500ms), loopers |

**Total internal SRAM: 1 MB.** DTCM is fastest but only 128 KB.
AXI SRAM (512 KB) is the primary working memory for DSP allocations.

### Audio Path

```
Codec ADC → SAI RX → DMA → SRAM buffer (ping)
                              │
                     Process buffer (pong) ← CPU
                              │
         SAI TX ← DMA ← SRAM buffer (ping) → Codec DAC
```

- **DMA double-buffer** — CPU processes one half while DMA fills/drains the other
- **Block size** — 32 samples default (0.67 ms at 48 kHz), configurable to 64
- **Format** — 24-bit I2S, processed as `f32` internally
- **Known limitation** — Embassy uses a circular-buffer workaround, not hardware
  M0AR/M1AR double-buffer registers
  ([embassy#702](https://github.com/embassy-rs/embassy/issues/702))

---

## sonido-daisy Crate

Firmware crate with tiered examples at `crates/sonido-daisy/`.
Not in workspace `default-members` (requires ARM target).

```bash
# Cross-compile check (no flash)
cargo check -p sonido-daisy \
    --target thumbv7em-none-eabihf
```

### Tier System

| Tier | Example | What It Validates | Hardware Needed |
|:----:|---------|-------------------|-----------------|
| 1 | `blinky_bare.rs` | Toolchain, flash, BOOT_SRAM path | Seed + USB |
| 1 | `blinky.rs` | Embassy runtime + clock init | Seed + USB |
| 2 | `bench_kernels.rs` | DWT cycle counts for all 19 kernels | Seed + USB |
| 3 | `passthrough.rs` *(stub)* | Codec, DMA, audio path | Seed + audio I/O |
| 4 | `single_effect.rs` *(stub)* | Real-time DSP, ADC parameter mapping | Seed + audio I/O + pot |

### Library — `src/lib.rs`

| Symbol | Value | Purpose |
|--------|-------|---------|
| `SAMPLE_RATE` | 48,000.0 Hz | Default audio sample rate |
| `BLOCK_SIZE` | 128 samples | DMA half-transfer size |
| `CHANNELS` | 2 | Stereo |
| `DMA_BUFFER_SIZE` | 512 | `BLOCK_SIZE * CHANNELS * 2` (double-buffer) |
| `CYCLES_PER_BLOCK` | 1,280,000 | CPU cycles available per block at 480 MHz |
| `measure_cycles(\|\| { })` | — | DWT cycle counter wrapper |
| `enable_cycle_counter()` | — | Call once at startup before measuring |

### Dependencies

| Crate | Version | Purpose |
|-------|:-------:|---------|
| `embassy-stm32` | 0.5 | Async HAL — SAI, DMA, GPIO, ADC |
| `embassy-executor` | 0.9 | Async task executor for Cortex-M |
| `embassy-time` | 0.5 | Timer and delay utilities |
| `daisy-embassy` | 0.2 | Daisy Seed BSP — codec init, audio interface |
| `cortex-m` | 0.7 | Low-level Cortex-M access — DWT, SCB |
| `cortex-m-rt` | 0.7 | Runtime — vector table, entry point |
| `embedded-alloc` | 0.6 | Heap allocator for DSP buffer allocations |
| `defmt` | 1.0 | Efficient embedded logging |
| `defmt-rtt` | 1.1 | RTT transport for defmt |
| `panic-probe` | 1.0 | Panic handler with defmt output |

**Feature flags:**
- `seed_1_2` *(default)* — Rev 7 PCM3060 codec
- `seed_1_1` — Rev 5 WM8731 codec

---

## Getting Started

### Prerequisites

1. **Micro USB data cable** — NOT charge-only.
   Data cables have 4 wires (power + D+/D-); charge-only cables have 2.

2. **Linux udev rule** for DFU access without sudo:

   ```bash
   sudo tee /etc/udev/rules.d/50-daisy-stm-dfu.rules << 'EOF'
   SUBSYSTEMS=="usb", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="df11", \
       MODE="0666", GROUP="plugdev", TAG+="uaccess"
   EOF
   ```

   ```bash
   sudo udevadm control --reload-rules
   sudo udevadm trigger
   ```

3. **dfu-util** for command-line flashing:

   ```bash
   sudo apt install dfu-util
   ```

4. **Rust embedded target:**

   ```bash
   rustup target add thumbv7em-none-eabihf
   ```

5. **probe-rs** *(optional — needed for Phase 2 defmt output):*

   ```bash
   cargo install probe-rs-tools --locked
   ```

### Phase 1: Validate Hardware

*Bare Seed + USB. No probe required.*

#### Option A — Browser flash (fastest, no Rust needed)

1. Enter DFU mode:
   **hold BOOT** → **press/release RESET** → **release BOOT**
2. Verify DFU detection:
   ```bash
   lsusb | grep "0483:df11"
   ```
   Should show: `STMicroelectronics STM Device in DFU Mode`
3. Open [flash.daisy.audio](https://flash.daisy.audio/) in **Chrome** (WebUSB)
4. Click **Connect** → select **DFU in FS Mode** → **Flash Blink**
5. LED blinks = hardware works

#### Option B — Sonido blinky (validates full toolchain + BOOT_SRAM)

All examples use **BOOT_SRAM** mode: the Electrosmith bootloader copies firmware
from QSPI flash to AXI SRAM on each boot. Code executes from zero-wait-state
SRAM, allowing Embassy to safely reconfigure clocks.

Build from the crate directory (picks up `.cargo/config.toml` target):

```bash
cd crates/sonido-daisy
cargo objcopy --example blinky_bare --release -- -O binary blinky.bin
```

Enter DFU mode, then flash to QSPI (bootloader copies to SRAM on boot):

```bash
dfu-util -a 0 -s 0x90040000:leave -D blinky.bin
```

LED blinks = BOOT_SRAM path + toolchain + flash all working.

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

After flashing, the Daisy enumerates as a USB serial device (CDC ACM).
Read results with any terminal:

```bash
cat /dev/ttyACM0
# or: screen /dev/ttyACM0 115200
```

Output repeats every 5 seconds so you can connect at any time:

```
=== Sonido Kernel Benchmarks ===
sample_rate=48000 block_size=128 budget=1280000 cycles
       preamp     XXXXX cycles  X.XX%
   distortion     XXXXX cycles  X.XX%
   compressor     XXXXX cycles  X.XX%
...
=== End ===
```

With an SWD probe (ST-Link V3 Mini, ~$12), results are also available via
defmt RTT:

```bash
cargo run --example bench_kernels --release
```

### Phase 3: Audio Passthrough

*Requires audio I/O — Hothouse carrier board or breadboard wiring to SAI pins.*

Not possible on bare Seed without wiring up the codec.
`examples/passthrough.rs` is a stub awaiting the daisy-embassy audio interface
builder (handles codec init and DMA setup).

### Phase 4: Single Effect

*Requires audio I/O + potentiometer on an ADC pin.*

Wire one ADC pin to a pot, process audio through a kernel with `from_knobs()`
mapping ADC readings to parameters.
`examples/single_effect.rs` is a stub.

### Build & Flash Reference

All commands run from `crates/sonido-daisy/` (picks up `.cargo/config.toml`).

**Build** (any example):

```bash
cargo build --example <name> --release
```

**Flash via DFU** (USB, no probe — BOOT_SRAM):

```bash
cargo objcopy --example <name> --release -- -O binary <name>.bin
dfu-util -a 0 -s 0x90040000:leave -D <name>.bin
```

**Flash via SWD probe** (supports defmt RTT):

```bash
cargo run --example <name> --release
```

> **Power:** USB alone is sufficient for development. External VIN (5–17V DC)
> only needed inside the Hothouse enclosure.

---

## Memory Budget

Each `InterpolatedDelay` buffer = `max_delay_samples * 4` bytes (f32).

| Effect | Buffer Size @ 48 kHz | Notes |
|--------|:--------------------:|-------|
| Reverb (stereo) | ~110 KB | 8+8 combs + 4+4 allpasses |
| Reverb (mono) | ~55 KB | Half the buffers |
| Delay (2s, stereo) | ~750 KB | **Exceeds AXI SRAM** — needs SDRAM |
| Delay (500ms, stereo) | ~188 KB | Fits in AXI SRAM |
| Delay (300ms, mono) | ~56 KB | Default delay time |
| Chorus | ~8 KB | 20ms max delay |
| Flanger | ~4 KB | ~10ms max delay |
| All others | < 1 KB each | Phaser, Distortion, Compressor, Gate, etc. |

### Memory Placement (BOOT_SRAM)

```
AXI SRAM (480 KB usable, 0-wait — code executes here)
├── .text + .rodata (firmware code, ~90 KB for full bench)
└── ~390 KB headroom

DTCM (128 KB, 0-wait — data)
├── Stack (8–16 KB)
├── .bss + .data (globals, filter state)
└── ~100 KB for hot per-sample DSP state

D2 SRAM (288 KB, 1–2 wait — heap + DMA)
├── Heap allocator (~256 KB) — delay lines, comb buffers
├── Audio DMA buffers (SAI, 2 KB)
└── ~30 KB headroom

SDRAM (64 MB, 4–8 wait)
├── Long delay lines (> 500ms)
├── Sampler / looper buffers
└── Large lookup tables
```

### Chain Configurations

**Comfortable** — < 300 KB, fits AXI SRAM with headroom:

| Chain | Memory | CPU Est. |
|-------|-------:|---------:|
| Preamp → Distortion → Chorus → Delay(300ms) | ~65 KB | ~30% |
| Gate → Tape → Flanger → Delay(300ms) | ~62 KB | ~22% |
| Preamp → Wah → Distortion → Chorus | ~10 KB | ~32% |

**Tight** — CPU 50–80%:

| Chain | Memory | CPU Est. |
|-------|-------:|---------:|
| Preamp → Distortion → Chorus → Delay → Reverb(mono) | ~120 KB | ~78% |
| Compressor → Distortion → Reverb(mono) | ~57 KB | ~77% |

**Needs SDRAM** — any chain with stereo Delay > 500ms.

**CPU budget exceeded** — EQ or Vibrato (>100% alone), Phaser + Reverb (>124%).
See [Benchmarks](BENCHMARKS.md) for full Cortex-M7 cycle estimates. Phase 2
benchmarks will provide real measurements to validate these.

---

## Hothouse

The Cleveland Music Co. Hothouse is a DIY pedal enclosure for Daisy Seed.

> **Not yet available** — this section is reference for Phases 3–4.

### Controls

| Control | Type | Daisy Pin | sonido Mapping |
|---------|------|-----------|----------------|
| KNOB 1–6 | 10K pot (ADC) | PIN 21–25, 28 | `ControlId::hardware(0x00..0x05)` |
| TOGGLE 1–3 | 3-way (GPIO) | PIN 5–10 | `ControlId::hardware(0x10..0x12)` |
| FOOTSWITCH 1–2 | Momentary (GPIO) | PIN 27, 14 | `ControlId::hardware(0x20..0x21)` |
| LED 1–2 | Status (GPIO) | PIN 4, 3 | `ControlId::hardware(0x30..0x31)` |

- **Audio** — 1/4" TRS stereo I/O, instrument level (200mV–1V p-p).
  Synth line out (~2.8V) needs padding; Eurorack (5–10V) will clip.
- **Free pins** — D11/D12 (I2C for OLED), D13/D14 (UART for MIDI)

### Control Combinatorics

| Surface | States | Purpose |
|---------|:------:|---------|
| 3 toggles × 3 positions | 27 | Preset / bank selection |
| 6 knobs | continuous | Per-preset parameters |
| 2 footswitches | 4 | Bypass, tap tempo, page cycle |

### Design Patterns

- **Bank + Preset** — TOGGLE 1 = bank (A/B/C), TOGGLE 2 = preset (1/2/3),
  TOGGLE 3 = modifier. 27 presets accessible without a display.
- **Parameter pages** — Footswitch long-press (>500ms) cycles pages.
  LED blink count = current page. Knob pickup prevents parameter jumps.
- **Tap tempo** — FOOTSWITCH 1 tap = record interval, hold = reset.
  LED 1 blinks at tempo.

### Software Patterns

**Toggle reading** — 2 GPIO pins per 3-way toggle:

```rust
match (up_pin, down_pin) {
    (true, false)  => Position::Up,
    (false, false) => Position::Middle,
    (false, true)  => Position::Down,
    _ => unreachable!(), // both true = hardware fault
}
```

**Footswitch modes:**

| Mode | Behavior |
|------|----------|
| Momentary | Read pin state directly |
| Latching | Software toggle on press |
| Long-press | Detect hold >500ms for secondary function |

---

## Platform Abstraction

The `PlatformController` trait (`crates/sonido-platform/src/lib.rs`) maps
hardware controls to effect parameters. The Daisy/Hothouse firmware:

1. Reads ADC/GPIO pins → `ControlId` values
2. Routes controls via `ControlMapper` → kernel parameters via `from_knobs()`
3. Processes audio blocks via `DspKernel::process_stereo()`

### What Sonido Already Handles

| Capability | Detail |
|------------|--------|
| Cross-compilation | 6 `no_std` crates build for `thumbv7em-none-eabihf` |
| DMA-ready DSP | `DspKernel::process_stereo()` — no alloc, no dispatch |
| ADC mapping | `KernelParams::from_knobs()` — 0.0–1.0 → parameter ranges |
| Control discovery | `ParameterInfo` — runtime parameter introspection |
| Math safety | `libm` everywhere — no `f32::sin()` in `no_std` crates |
| Heap support | Delay buffers use `Vec` with any global allocator |

### What Needs Work

| Gap | Detail |
|-----|--------|
| Memory placement | Large buffers (delay >500ms) need linker sections for SDRAM |
| Block processing | Biquad/SVF have per-sample `process()` only — block version would improve CM7 cache behavior |

---

## Hardware Interface Gaps

Features needed for production pedal deployment.
Tracked in [ROADMAP.md](ROADMAP.md) — Embedded Hardening section.

### Expression Pedal

TRS expression pedals output variable voltage via potentiometer wiper.
Real pedals need:

- **Calibration** — per-pedal min/max (typical sweep 0.05–0.92, not 0.0–1.0)
- **Response curves** — log for volume, S-curve for wah, custom LUT for morphing
- **Polarity detection** — tip-hot vs ring-hot, auto-detect on first sweep

Implementation: `ControlType::Expression` in `sonido-platform` with
`ExpressionConfig`.

### CV Input (Eurorack)

- **Unipolar** — 0–5V
- **Bipolar** — ±5V (requires external conditioning; Daisy ADC reads 0–3.3V)

Implementation: `ControlType::CvInput` with voltage range and scaling parameters.

### MIDI CC Routing

Via UART (pins D13/D14):

- CC learn mode (footswitch-triggered)
- Program Change → preset recall
- MIDI Clock → `TempoManager` sync
- Running status parsing for bandwidth efficiency

Namespace: `ControlId::midi(0x02XX)`.

### Pot Calibration

Real pots read 0.003–0.991, not 0.0–1.0.

- Per-pot min/max stored in flash
- Dead zones near boundaries (prevents jitter)
- Hysteresis — require N-step ADC change before updating (noise rejection)

### Control Curves

Per-control response shaping:

| Curve | Use Case |
|-------|----------|
| Linear | Most parameters (depth, mix, rate) |
| Logarithmic | Frequency, volume (perceptual linearity) |
| Reverse log | Attack/release times |
| S-curve | Crossfade, morph position |
| Custom LUT | 16–32 point lookup table for arbitrary response |

Applied in `ControlMapper::map_control()` after calibration, before dispatch.

### Debounce

`Debouncer<const N: usize>` in `sonido-platform`:

- Configurable window (default 30ms)
- Edge-triggered mode for footswitches
- Level-triggered mode for toggles

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

### Community

- [Daisy Forum: Rust development](https://forum.electro-smith.com/t/rust-starter-for-daisy-seed/684)
- [Daisy Forum: Rev 7 noise floor](https://forum.electro-smith.com/t/rev-7-noise-floor-vs-rev-4/4943)

### Sonido Internal

- [Kernel Architecture](KERNEL_ARCHITECTURE.md)
- [Benchmarks](BENCHMARKS.md) — Cortex-M7 cycle estimates
- [Architecture](ARCHITECTURE.md)
