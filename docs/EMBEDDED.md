# Embedded Guide

Deploying Sonido on the Electrosmith Daisy Seed (STM32H750 Cortex-M7) and the
Cleveland Music Co. Hothouse DIY pedal platform.

> **Current hardware:** Daisy Seed 65 MB (Rev 7 / PCM3060) + Hothouse DIY pedal
> platform (assembled, not yet validated). Phases 1-2 require bare Seed + USB;
> Phases 3-4 require the Hothouse for audio I/O and controls.

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
| AXI SRAM | `0x2400_0000` | 512 KB (480 KB usable under BOOT_SRAM) | 0–1 | Delay lines, reverb buffers, heap |
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
- **Block size** — 32 samples (0.67 ms at 48 kHz), matching libDaisy C++ default — see `BLOCK_SIZE` in `sonido-daisy/src/lib.rs`
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
| 1 | `heap_test.rs` | SRAM clock enable + heap allocation | Seed + USB |
| 2 | `bench_mini.rs` | Single kernel DWT cycle benchmark (PreampKernel) | Seed + USB |
| 2 | `bench_kernels.rs` | DWT cycle counts for all 19 kernels (dual-budget) | Seed + USB |
| 3 | `silence.rs` | Codec/SAI/DMA init — digital silence output | Hothouse |
| 3 | `tone_out.rs` | DAC signal path health (440 Hz sine) | Hothouse |
| 3 | `square_out.rs` | DAC health check (1 kHz full-scale square) | Hothouse |
| 3 | `passthrough.rs` | Codec, DMA, audio passthrough | Seed + audio I/O |
| 3 | `passthrough_blink.rs` | Audio passthrough + LED heartbeat task | Hothouse |
| 3 | `hothouse_diag.rs` | All Hothouse hardware (knobs, toggles, FS, temp) | Hothouse |
| 4 | `single_effect.rs` | Real-time DSP, ADC parameter mapping (distortion) | Hothouse |
| 5 | `morph_pedal.rs` | EmbeddedAdapter + ProcessingGraph DAG + A/B morph | Hothouse |

### Modern Rust on Daisy Seed

**sonido-daisy** owns the full platform layer (clock, audio, ADC, LED). Key patterns:

- **Clock**: `sonido_daisy::rcc_config(ClockProfile::Performance)` or `::Efficient` — 480 MHz / 400 MHz with proper PLL2_P ADC clock fix.
- **Audio**: `AudioPeripherals` + `start_callback()` — async loop, yields every DMA transfer (~0.667 ms at 48 kHz, 32-sample blocks).
- **LED / UI**: Use `sonido_daisy::heartbeat` — the shared lub-dub blink task. Every binary spawns it: `spawner.spawn(heartbeat(UserLed::new(p.PC7))).unwrap();`.
- **USB / Serial**: Same spawned-task pattern. See `hothouse_diag.rs`.
- **Audio callback is real-time** — NEVER block in the audio callback. No ADC reads, no USB, no allocation. Only pure DSP math and lock-free ControlBuffer reads. Blocking ADC reads (~8 µs at CYCLES387_5) cause DMA overruns → hard faults.
- **Task return type**: Use `async fn task(...) { }` (implicit `()` return), not `-> !`.

Reference implementation: `crates/sonido-daisy/examples/single_effect.rs`

### Control / Audio Separation

ADC and GPIO reads run in a separate Embassy task (`hothouse_control_task`) at 50 Hz, matching libDaisy's architecture where `seed.adc` runs via DMA independently of audio. Shared state flows through a lock-free `ControlBuffer` using `Relaxed` atomics.

```
┌─────────────────────┐              ┌──────────────────────┐
│ hothouse_control_task│  ControlBuffer  │   Audio Callback     │
│ (50 Hz, async task) │───────────────→│ (1500 Hz, real-time) │
│                     │  (lock-free)    │                      │
│ • ADC knob reads    │              │ • read_knob()         │
│ • GPIO toggle reads │              │ • read_toggle()       │
│ • GPIO footswitches │              │ • read_footswitch()   │
│ • IIR smoothing     │  ←────────────│ • write_led()         │
│ • LED GPIO output   │  LED bridge   │                      │
└─────────────────────┘              └──────────────────────┘
```

**Library modules:**

| Module | Purpose | Dependencies |
|--------|---------|-------------|
| `controls.rs` | `ControlBuffer<KNOBS,TOGGLES,FS,LEDS>` — lock-free shared state with IIR smoothing, change detection, LED bridge | `core` only |
| `hothouse.rs` | `HothouseControls`, `hothouse_control_task`, `hothouse_pins!` macro, `decode_toggle` | `controls.rs` + Embassy |
| `embedded_adapter.rs` | `EmbeddedAdapter<K>` — zero-smoothing `Effect + ParameterInfo` for `DspKernel` | `sonido-core` (feature `alloc`) |
| `param_map.rs` | `adc_to_param()` — scale-aware ADC→parameter conversion with STEPPED rounding | `sonido-core` (feature `alloc`) |

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

All examples use `StaticCell<T>` from the `static_cell` crate for USB buffer
allocation. This avoids `static mut` and the associated `unsafe` blocks:

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

// Control task (writer, 50 Hz): IIR-smoothed ADC values
// hothouse_control_task writes knobs, toggles, footswitches

// Audio callback (reader, 1500 Hz): lock-free reads
let drive = CONTROLS.read_knob(0);     // 0.0–1.0, smoothed
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
| `CYCLES_PER_BLOCK` | 320,000 | CPU cycles available per block at 480 MHz |
| `measure_cycles(\|\| { })` | — | DWT cycle counter wrapper |
| `enable_cycle_counter()` | — | Call once at startup before measuring |
| `ControlBuffer` | — | Re-export from `controls.rs` |
| `EmbeddedAdapter` | — | Re-export from `embedded_adapter.rs` (feature `alloc`) |
| `adc_to_param()` | — | Re-export from `param_map.rs` (feature `alloc`) |

### Feature Flags

| Feature | Enables | Required By |
|---------|---------|-------------|
| *(none)* | Core library: audio, controls, hothouse, LED, RCC | Simple examples (blinky, passthrough) |
| `alloc` | `EmbeddedAdapter`, `adc_to_param`, DSP-dependent modules | morph_pedal, bench_kernels |
| `platform` | `HothousePlatform` (`PlatformController` impl) + implies `alloc` | Future platform integration |

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
| `embedded-alloc` | 0.6 | Heap allocator (backed by SDRAM) |
| `grounded` | 0.2 | DMA buffer management (GroundedArrayCell) |
| `static_cell` | 2 | Safe static initialization for USB buffers |
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
- **BOOT button extends grace period** — hold to keep listening (acknowledged by rapid blinks)
- After grace period, bootloader jumps to user program (if one is stored in QSPI)
- **No program stored** — stays in grace period indefinitely until DFU flash
- **SOS blink pattern** (3 short, 3 long, 3 short) — invalid binary detected

To enter DFU mode for flashing:

1. **Press RESET** — LED pulses sinusoidally for 2.5 seconds (grace period)
2. **Run `dfu-util`** within the grace period
3. That's it — the bootloader accepts DFU transfers during the grace period

If you need more time (e.g., typing the command), hold **BOOT** while pressing
RESET to extend the grace period indefinitely. Release BOOT when ready.

> **Important:** The BOOT button is **PG3** (a GPIO pin read by the Electrosmith
> bootloader), **not** the STM32's BOOT0 pin. Both "just RESET" and "BOOT+RESET"
> enter the same Electrosmith bootloader — BOOT simply extends the window.
> There is no separate "STM32 System DFU" mode accessible via these buttons.

Verify DFU is active before flashing:

```bash
lsusb | grep "0483:df11"
# Should show: STMicroelectronics STM Device in DFU Mode
```

> **First-time Daisy:** The bootloader comes pre-flashed from the factory.
> If your Seed has never been used, it will sit in the grace period with
> a pulsing LED — this is normal and means it's ready for DFU.

#### Option A — Browser flash (fastest, no Rust needed)

1. Enter DFU mode:
   **press RESET** (or **hold BOOT** → **press RESET** → **release BOOT** for extended window)
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

After flashing, the Daisy resets, runs benchmarks (~1 second), then enumerates
as a USB serial device (CDC ACM). You may need a udev rule for non-root access:

```bash
# If /dev/ttyACM0 shows "permission denied":
sudo tee /etc/udev/rules.d/50-daisy-cdc.rules << 'EOF'
SUBSYSTEMS=="usb", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", \
    MODE="0666", GROUP="plugdev", TAG+="uaccess"
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
sample_rate=48000 block_size=128 budget=1280000 cycles
       preamp     XXXXX cycles  X.XX%
   distortion     XXXXX cycles  X.XX%
   compressor     XXXXX cycles  X.XX%
...
=== End ===
```

> **Device not appearing?** After flashing, the Daisy needs ~2 seconds to run
> benchmarks and initialize USB. Check with `dmesg | tail` — you should see
> `cdc_acm` and a `/dev/ttyACM*` assignment. If nothing appears, unplug and
> replug USB (the board resets on reconnect).

With an SWD probe (ST-Link V3 Mini, ~$12), results are also available via
defmt RTT:

```bash
cargo run --example bench_kernels --release
```

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

`examples/single_effect.rs` processes audio through a distortion kernel with 4
ADC knobs mapped to Drive, Tone, Output, and Mix. Toggle 1 selects clipping
mode (Overdrive / Distortion / Fuzz). Footswitch 1 toggles bypass.

This is the canonical pattern for **direct kernel usage** on embedded:
`from_knobs(adc_0..adc_3)` maps ADC readings to a `DistortionParams` struct,
which is passed directly to `DspKernel::process_stereo()`. No `KernelAdapter`,
no smoothing.

```bash
cd crates/sonido-daisy
cargo objcopy --example single_effect --release -- -O binary -R .sram1_bss single_effect.bin
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
| `hothouse_diag.rs` | Input levels + 6 knobs + GPIO (FS, toggles) + CPU temp | `AUDIO in=-46.8dBFS ... \| K1=... \| FS1=... \| CPU 52C` via USB serial every 2 seconds |

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
AUDIO in=-46.8dBFS rms=0.0045 peak=0.0078 | K1=0.512 K2=0.000 K3=1.000 K4=0.250 K5=0.750 K6=0.333 | FS1=0 FS2=0 SW1=MID SW2=MID SW3=UP | CPU 52C
```

#### Diagnostic Test Sequence

When debugging audio issues on the Hothouse, run these in order:

1. **Flash `tone_out`** — plug headphones or an amp into the output jack and
   listen for a 440 Hz sine tone. The onboard user LED (PC7) blinks at 1 Hz
   (500ms on / 500ms off) — same pattern as `blinky` — to confirm firmware
   is running. If the LED doesn't blink, the firmware didn't flash correctly.
   - **Hear 440 Hz clearly** → DAC and output path work. Skip to step 3.
   - **Hear 440 Hz underneath noise** → DAC works, analog stage is oscillating
     on top of it. Output op-amp issue.
   - **Hear only noise, no tone** → proceed to step 2.

2. **Flash `square_out`** — outputs the loudest possible digital signal
   (1 kHz full-scale square wave). Same 1 Hz blink heartbeat on user LED.
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
   - **CPU temp > 80°C** → thermal issue; check enclosure ventilation.

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

## Morph Pedal v2

The morph pedal (`examples/morph_pedal.rs`) is the flagship firmware — a
3-slot, DAG-routed, A/B morphing guitar pedal with 14 curated effects. It
demonstrates all of sonido's embedded capabilities: `ProcessingGraph` for
routing, `EmbeddedAdapter` for zero-smoothing kernel access, and scale-aware
ADC parameter mapping.

### Architecture Overview

```
   ┌─────────┐     ┌───────────────────┐     ┌──────────┐
   │ ADC × 6 │──→──│  adc_to_param()   │──→──│ Graph    │
   │ Toggles  │     │  (ParamScale-     │     │ .effect_ │
   │ Footsw   │     │   aware scaling)  │     │ set_param│
   └─────────┘     └───────────────────┘     └──────────┘
                                                   │
                        ┌──────────────────────────┘
                        ▼
   ┌─────────────────────────────────────────────────────┐
   │             ProcessingGraph (compiled DAG)           │
   │  Input → [EmbeddedAdapter<K>] → ... → Output        │
   │          (zero-smoothing kernel wrapper)             │
   └─────────────────────────────────────────────────────┘
```

Two sonido pillars are used together:

- **ProcessingGraph** — DAG routing with serial/parallel/fan topologies via
  split/merge nodes. Zero-alloc per audio block after `compile()`.
- **EmbeddedAdapter\<K: DspKernel\>** — wraps any kernel in `Effect + ParameterInfo`
  (and thus `EffectWithParams` via blanket impl) with zero smoothing overhead.
  `set_param()` writes directly to the kernel's typed params struct. The value
  is live on the next `process_stereo()` call.

Why not `KernelAdapter`? It adds per-sample `SmoothedParam::advance()` calls
(~1.15M advances/sec for 24 params × 48kHz). On embedded, ADCs are
hardware-filtered and params change at ~100Hz. Smoothing is redundant overhead.

### EmbeddedAdapter Pattern

`EmbeddedAdapter<K>` is defined locally in `morph_pedal.rs` (~55 lines):

```rust
struct EmbeddedAdapter<K: DspKernel> {
    kernel: K,
    params: K::Params,
}

impl<K: DspKernel> Effect for EmbeddedAdapter<K> {
    fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.kernel.process_stereo(l, r, &self.params)
    }
    fn set_sample_rate(&mut self, sr: f32) { self.kernel.set_sample_rate(sr); }
    fn reset(&mut self) { self.kernel.reset(); self.params = K::Params::from_defaults(); }
    // ... block processing, is_true_stereo(), latency_samples()
}

impl<K: DspKernel> ParameterInfo for EmbeddedAdapter<K> {
    fn param_count(&self) -> usize { K::Params::COUNT }
    fn param_info(&self, idx: usize) -> Option<ParamDescriptor> { K::Params::descriptor(idx) }
    fn get_param(&self, idx: usize) -> f32 { self.params.get(idx) }
    fn set_param(&mut self, idx: usize, val: f32) { self.params.set(idx, val); }
}
```

Comparison with sonido's other DSP bridge patterns:

| Pattern | Context | Smoothing | Trait impl |
|---------|---------|-----------|------------|
| `KernelAdapter<K>` | Desktop / Plugin | Yes (SmoothedParam per param) | Effect + ParameterInfo |
| `EmbeddedAdapter<K>` | Morph pedal firmware | None (direct write) | Effect + ParameterInfo |
| Direct `DspKernel` | `single_effect.rs` | None (`from_knobs()`) | No trait — call `process_stereo()` directly |

Use `EmbeddedAdapter` when you need `ProcessingGraph` compatibility (the graph
requires `Effect + ParameterInfo` behind `EffectWithParams`). Use direct kernel
access when you have a single fixed effect and don't need the graph.

### Three-Mode Interaction

Toggle 3 selects the operating mode:

| T3 | Mode | Purpose |
|----|------|---------|
| UP | Explore | Scroll effects into 3 slots, shape params with knobs |
| MID | Build | Capture Sound A/B snapshots per-slot, preview with T1 |
| DN | Morph | Footswitch-controlled crossfade between Sound A and B |

Toggle 2 selects routing topology (in all modes):

| T2 | Routing | Topology |
|----|---------|----------|
| UP | Serial | Input → E0 → E1 → E2 → Output |
| MID | Parallel | Input → Split → E0,E1,E2 → Merge → Output |
| DN | Fan | Input → E0 → Split → E1,E2 → Merge → Output |

Toggle 1 has mode-dependent function:

| Mode | T1 UP | T1 MID | T1 DN |
|------|-------|--------|-------|
| Explore | Focus slot 0 | Focus slot 1 | Focus slot 2 |
| Build | Preview Sound A | Preview 50% lerp | Preview Sound B |
| Morph | Speed 0.5s | Speed 2s | Speed 5s |

**Explore mode** — Boot state. All slots empty (Input→Output passthrough).
FS2 creates the first effect by scrolling right through the effect list. FS1
scrolls left. T1 selects which slot (0/1/2) the footswitches and knobs
control. Knobs set parameters via `adc_to_param()` with scale-aware mapping
(log for frequencies, linear for dB). "Knob = truth" — the physical pot
position is always the param value, no pickup logic.

**Build mode** — On entry, captures current state as Sound A, copies to Sound
B. `build_slot` starts at the first populated slot. Knobs always write to
Sound B for the active `build_slot`. FS2 captures and advances to next
populated slot. FS1 captures and goes back. T1 previews: UP = Sound A across
all slots, MID = 50% lerp, DN = Sound B (live knobs for current slot, stored
values for others). LED2 blinks slot number (1/2/3 pulses).

**Morph mode** — FS1 held → morph_t ramps toward 0.0 (Sound A). FS2 held →
toward 1.0 (Sound B). Neither → hold position. `interpolate_and_apply()`
writes lerped params to all slots every control poll (~100Hz). STEPPED params
snap at t=0.5 (same as `KernelParams::lerp()`). K6 overrides morph speed
(0.2–10.0s). LED2 brightness tracks morph_t via PWM.

### Curated Effect List and Knob Mapping

14 effects ordered chillest → gnarliest, with consistent knob roles:

| Role | Knob | Always means |
|------|------|-------------|
| K1 | Primary | The defining control (rate, cutoff, drive, threshold) |
| K2 | Secondary | Next most important (depth, feedback, resonance, tone) |
| K3 | Color | Texture modifier (damping, HF rolloff, stages, jitter) |
| K4 | Character | Mode/shape, often STEPPED (voices, TZF, ping-pong) |
| K5 | Mix | Wet/dry blend when available |
| K6 | Level | Output/makeup gain |

Mapping is stored in a `const` array:

```rust
const NULL_KNOB: u8 = 0xFF;  // sentinel for unmapped knobs

struct EffectEntry {
    id: &'static str,        // registry ID for logging
    knobs: [u8; 6],          // knobs[k] = param index for knob K
}

const EFFECT_LIST: [EffectEntry; 14] = [
    EffectEntry { id: "filter",     knobs: [0, 1, 0xFF, 0xFF, 0xFF, 2] },
    EffectEntry { id: "tremolo",    knobs: [0, 1, 2, 3, 0xFF, 6] },
    // ... 12 more entries
];
```

The `create_effect(idx, sr)` factory creates the correct `EmbeddedAdapter`
for each index:

```rust
fn create_effect(idx: usize, sr: f32) -> Option<Box<dyn EffectWithParams + Send>> {
    match idx {
        0  => Some(Box::new(EmbeddedAdapter::new(FilterKernel::new(sr)))),
        1  => Some(Box::new(EmbeddedAdapter::new(TremoloKernel::new(sr)))),
        // ... all 14
        _ => None,
    }
}
```

### Scale-Aware ADC Conversion

`adc_to_param()` uses `ParamDescriptor::scale` for proper knob curves:

```rust
fn adc_to_param(desc: &ParamDescriptor, norm: f32) -> f32 {
    let val = match desc.scale {
        ParamScale::Linear      => desc.min + norm * (desc.max - desc.min),
        ParamScale::Logarithmic => exp2f(log2f(desc.min) + norm * (log2f(desc.max) - log2f(desc.min))),
        ParamScale::Power(exp)  => desc.min + powf(norm, exp) * (desc.max - desc.min),
    };
    if desc.flags.contains(ParamFlags::STEPPED) { roundf(val) } else { val }
}
```

This gives `from_knobs()`-quality response: log sweep for frequency knobs
(cutoff, LFO rate), linear for dB/mix, power curves for custom params.

### A/B Morphing

`SoundSnapshot` captures all parameter values + STEPPED flags for 3 slots:

```rust
struct SoundSnapshot {
    params: [[f32; MAX_PARAMS]; NUM_SLOTS],     // MAX_PARAMS=16, NUM_SLOTS=3
    stepped: [[bool; MAX_PARAMS]; NUM_SLOTS],   // cached ParamFlags::STEPPED per param
    param_counts: [usize; NUM_SLOTS],
}
```

`interpolate_and_apply()` writes lerped values to the graph every control
poll. STEPPED params snap at t=0.5 — the same logic as `KernelParams::lerp()`:

```rust
let val = if stepped { if t < 0.5 { a } else { b } } else { a + (b - a) * t };
effect.effect_set_param(p, val);  // direct write via EmbeddedAdapter
```

### Presets

9-slot in-memory ring buffer (`Vec<Preset>` on SDRAM heap). Presets survive
until power-off. Save: both-FS tap in Build mode. Load: both-FS tap in
Explore mode (cycles through occupied slots). Each `Preset` stores: effect
indices, routing, Sound A, Sound B, morph speed.

### How to Add a New Effect to the Morph Pedal

1. **Add the kernel import** in the `use sonido_effects::{...}` block at the
   top of `morph_pedal.rs`.

2. **Add a new entry to `EFFECT_LIST`** at the desired position. Map the 6
   knobs to parameter indices by checking the kernel's `KernelParams`
   implementation (look at `descriptor(index)` calls in the kernel file).
   Use `NULL_KNOB` (0xFF) for unused knobs. Follow the role convention:
   K1=Primary, K2=Secondary, K3=Color, K4=Character, K5=Mix, K6=Level.

3. **Add a match arm to `create_effect()`** at the matching index:
   ```rust
   N => Some(Box::new(EmbeddedAdapter::new(MyKernel::new(sr)))),
   ```

4. **Update `NUM_EFFECTS`** constant to match the new list length.

5. **Build and verify:**
   ```bash
   cd crates/sonido-daisy
   cargo check --example morph_pedal --release
   ```

### How to Modify Modes or Add Features

The control logic lives in a single `start_callback` closure in `main()`.
The structure is:

```
Audio callback (runs every block, ~1500Hz):
├── Deinterleave u32 → f32
├── graph.process_block() (compiled DAG, zero-alloc)
├── Reinterleave f32 → u32
└── Every POLL_EVERY blocks (~100Hz):
    ├── Read 6 ADC knobs
    ├── Decode 3 toggles + 2 footswitches
    ├── Mode-specific control logic (match on current Mode)
    │   ├── Explore: scroll effects, map knobs to focused slot
    │   ├── Build: capture snapshots, navigate slots, preview
    │   └── Morph: ramp morph_t, interpolate_and_apply()
    └── Global bypass detection (both-FS hold)
```

Key state variables (all local to the callback closure):

| Variable | Type | Purpose |
|----------|------|---------|
| `effect_indices` | `[Option<usize>; 3]` | Which effect is in each slot |
| `node_ids` | `[Option<NodeId>; 3]` | Graph node per slot (None = empty) |
| `graph` | `ProcessingGraph` | Compiled DAG |
| `mode` | `Mode` | Current operating mode |
| `routing` | `Routing` | Current topology |
| `focused_slot` | `usize` | Which slot knobs/FS control |
| `sound_a` / `sound_b` | `SoundSnapshot` | A/B parameter snapshots |
| `build_slot` | `usize` | Which slot is being sculpted in Build |
| `morph_t` | `f32` | Current morph position (0.0=A, 1.0=B) |

Graph rebuild (via `build_graph()`) happens only on topology changes: effect
scroll, slot population change, or T2 toggle. Parameter changes never rebuild
— they go through `graph.effect_with_params_mut(nid).effect_set_param()`.

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

Use `AudioPeripherals` + `start_callback()`:

```rust
use sonido_daisy::audio::AudioPeripherals;

// Build AudioPeripherals from pin assignments (see passthrough.rs for full list)
let audio = AudioPeripherals { /* SAI1, DMA, codec pins */ };
let mut interface = audio.start_interface(&mut cp.SCB).await;

// D-cache: spawn deferred task BEFORE start_callback
spawner.spawn(deferred_dcache()).unwrap();

interface.start_callback(|input: &[u32], output: &mut [u32]| {
    // Deinterleave, process, reinterleave
}).await;
```

### 5. Build and flash

```bash
cd crates/sonido-daisy
cargo objcopy --example my_example --release -- -O binary -R .sram1_bss my_example.bin
dfu-util -a 0 -s 0x90040000:leave -D my_example.bin
```

### 6. Update documentation

- Add the example to the Tier table in this file (EMBEDDED.md)
- Add it to `docs/DOC_CODE_MAPPING.md` line 60 (daisy examples list)
- Add it to `CLAUDE.md` Key Files table (sonido-daisy examples line)

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
| SAI overrun error with SDRAM + audio | D-cache enabled during DMA init | Use `deferred_dcache()` task — enable D-cache AFTER audio DMA starts (see [D-Cache Timing](#d-cache-timing-critical)) |
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

## Memory Budget

Heap lives in 64 MB SDRAM. Memory is abundant — CPU cycles are the constraint.

Each `InterpolatedDelay` buffer = `max_delay_samples * 4` bytes (f32).

| Effect | Buffer Size @ 48 kHz | Notes |
|--------|:--------------------:|-------|
| Reverb (stereo) | ~110 KB | 8+8 combs + 4+4 allpasses |
| Reverb (mono) | ~55 KB | Half the buffers |
| Delay (2s, stereo) | ~750 KB | Fits in SDRAM (0.001% of 64 MB) |
| Delay (500ms, stereo) | ~188 KB | Fits in SDRAM |
| Chorus | ~8 KB | 20ms max delay |
| Flanger | ~4 KB | ~10ms max delay |
| All others | < 1 KB each | Phaser, Distortion, Compressor, Gate, etc. |

### Memory Placement (BOOT_SRAM)

```
AXI SRAM (480 KB usable, 0-wait — code executes here)
├── .text + .rodata (firmware code, ~90–160 KB)
└── ~320–390 KB headroom

DTCM (128 KB, 0-wait — data, hot path)
├── Stack (8–16 KB)
├── .bss + .data (globals, filter coefficients)
└── ~100 KB for per-sample DSP state (local vars in process_stereo)

D2 SRAM (288 KB, 1–2 wait — DMA only)
├── Audio DMA buffers (SAI TX/RX, .sram1_bss section, ~2 KB)
└── Reserved for future DMA peripherals

SDRAM (64 MB, 4–8 wait, MPU cacheable — heap)
├── Global heap allocator (all Vec/Box allocations)
├── Delay lines (768 KB for 2s stereo delay)
├── Reverb comb/allpass buffers (~110 KB stereo)
├── Sampler / looper buffers (up to ~10 min at 48 kHz)
└── Large lookup tables
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
Use the [`init_sdram!`] macro after `embassy_stm32::init()`:

```rust
let p = embassy_stm32::init(config);
let mut cp = unsafe { cortex_m::Peripherals::steal() };
let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
unsafe { HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE); }
```

The macro configures MPU Region 0 at `0xC000_0000` (write-back cacheable),
MPU Region 1 at `0x3000_0000` (D2 SRAM non-cacheable for DMA buffers),
enables I-cache, sets up all 54 FMC GPIO pins, and runs the AS4C16M32MSA-6
power-up sequence.

### D-Cache Timing (Critical)

**D-cache must be enabled AFTER SAI DMA is running.** Enabling during DMA
initialization causes bus matrix stalls that starve the DMA controller,
resulting in SAI overrun errors. This is an undocumented interaction between
the Cortex-M7 cache enable sequence and Embassy's ring buffer DMA driver.

The `init_sdram!` macro enables I-cache (safe — instruction fetches are
read-only) but intentionally does NOT enable D-cache. Call
`sonido_daisy::sdram::enable_dcache()` separately:

| Scenario | When to call `enable_dcache()` |
|----------|-------------------------------|
| No audio (benchmarks, diagnostics) | Immediately after `init_sdram!` |
| With audio (morph pedal, effects) | Deferred task, ~500ms after spawn (before audio setup) |

```rust
// Pattern for firmware WITH audio:
#[embassy_executor::task]
async fn deferred_dcache() {
    embassy_time::Timer::after_millis(500).await;
    sonido_daisy::sdram::enable_dcache();
}

// Spawn BEFORE audio setup starts:
spawner.spawn(deferred_dcache()).unwrap();
// ... audio setup + start_callback() ...
```

```rust
// Pattern for firmware WITHOUT audio:
let sdram_ptr = sonido_daisy::init_sdram!(p, &mut cp.MPU, &mut cp.SCB);
unsafe { HEAP.init(sdram_ptr as usize, sonido_daisy::sdram::SDRAM_SIZE); }
sonido_daisy::sdram::enable_dcache(); // safe — no DMA running
```

**Why it works after DMA is running:** MPU Region 1 marks the entire D2 SRAM
range (`0x3000_0000`, 512 KB) as Normal Non-Cacheable (`TEX=001, C=0, B=0`).
Once enabled, D-cache respects these attributes and will not cache DMA buffer
accesses, preserving coherency. The problem is only during the cache enable
*sequence* itself, which briefly stalls the AXI bus matrix.

**Performance impact:** D-cache gives ~3–5x speedup on SDRAM delay line reads
(4–8 wait states → ~1 cycle for cache hits). Without D-cache, a 3-effect chain
that uses 73% budget at 480 MHz would use ~250%+ — unusable.

### Chain Configurations

With the heap in 64 MB SDRAM, memory is no longer a constraint for
effect chains. Any combination of all 19 effects fits comfortably.
CPU budget is the limiting factor.

**Comfortable** — CPU < 50%:

| Chain | Memory | CPU Est. |
|-------|-------:|---------:|
| Preamp → Distortion → Chorus → Delay(2s) | ~780 KB | ~30% |
| Gate → Tape → Flanger → Delay(2s) | ~780 KB | ~22% |
| Preamp → Wah → Distortion → Chorus | ~10 KB | ~32% |

**Tight** — CPU 50–80%:

| Chain | Memory | CPU Est. |
|-------|-------:|---------:|
| Preamp → Distortion → Chorus → Delay → Reverb | ~890 KB | ~78% |
| Compressor → Distortion → Reverb(stereo) | ~112 KB | ~77% |

After the 4.5x performance optimization (commit `1c2194d` — D-cache, I-cache,
`target-cpu=cortex-m7`, DSP micro-optimizations), all 19 effects individually
fit under budget at 480 MHz. Total for all 19 is ~564K cycles (176% budget),
so chains of 3-4 effects are comfortable. See [Benchmarks](BENCHMARKS.md) for
measured Cortex-M7 cycle counts.

---

## Hothouse

The Cleveland Music Co. Hothouse is a DIY pedal enclosure for Daisy Seed.
Open-source hardware (CC BY-SA 4.0). Stereo version (Sep 2024+): 6 knobs, 3 toggles, 2 footswitches, 2 LEDs.

- **Repo**: [clevelandmusicco/HothouseExamples](https://github.com/clevelandmusicco/HothouseExamples)
- **Hardware**: [clevelandmusicco/open-source-pedals](https://github.com/clevelandmusicco/open-source-pedals/tree/main/hothouse)
- **Wiki**: [10-Minute Quick Start](https://github.com/clevelandmusicco/HothouseExamples/wiki/10%E2%80%90Minute-Quick-Start)

### Controls — Pin Mappings

**Knobs** (10K potentiometers, ADC analog inputs):

| Hothouse | Daisy Pin | STM32 GPIO | ADC Channel | sonido Mapping |
|----------|-----------|------------|:-----------:|----------------|
| KNOB_1 | D16 | PA3 | 0 | `ControlId::hardware(0x00)` |
| KNOB_2 | D17 | PB1 | 1 | `ControlId::hardware(0x01)` |
| KNOB_3 | D18 | PA7 | 2 | `ControlId::hardware(0x02)` |
| KNOB_4 | D19 | PA6 | 3 | `ControlId::hardware(0x03)` |
| KNOB_5 | D20 | PC1 | 4 | `ControlId::hardware(0x04)` |
| KNOB_6 | D21 | PC4 | 5 | `ControlId::hardware(0x05)` |

**Toggle Switches** (3-way, 2 GPIO pins each):

| Hothouse | Up Pin | Down Pin | sonido Mapping |
|----------|--------|----------|----------------|
| SWITCH_1 | D9 (PB4) | D10 (PB5) | `ControlId::hardware(0x10)` |
| SWITCH_2 | D7 (PG10) | D8 (PG11) | `ControlId::hardware(0x11)` |
| SWITCH_3 | D5 (PD2) | D6 (PC12) | `ControlId::hardware(0x12)` |

**Footswitches** (momentary, active-low GPIO):

| Hothouse | Daisy Pin | STM32 GPIO | sonido Mapping |
|----------|-----------|------------|----------------|
| FOOTSWITCH_1 (left) | D25 | PA0 | `ControlId::hardware(0x20)` |
| FOOTSWITCH_2 (right) | D26 | PD11 | `ControlId::hardware(0x21)` |

**LEDs** (GPIO output):

| Hothouse | Daisy Pin | STM32 GPIO | sonido Mapping |
|----------|-----------|------------|----------------|
| LED_1 | D22 | PA5 | `ControlId::hardware(0x30)` |
| LED_2 | D23 | PA4 | `ControlId::hardware(0x31)` |

- **Audio** — 1/4" TRS stereo I/O, instrument level (200mV–1V p-p).
  Synth line out (~2.8V) needs padding; Eurorack (5–10V) will clip.
- **Free pins** — D11/D12 (PB8/PB9, I2C for OLED), D13/D14 (PB6/PB7, UART for MIDI)

### Bootloader Shortcut

Hold left footswitch (FOOTSWITCH_1) for 2 seconds → LEDs flash 3x alternately →
Daisy resets to DFU bootloader. No need to open enclosure after first flash.
Implemented via `CheckResetToBootloader()` in the C++ examples; Rust equivalent
needed in sonido-daisy.

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
| Block processing | Biquad/SVF have per-sample `process()` only — block version would improve CM7 cache behavior |
| Flash persistence | Morph pedal presets are in-memory only (SDRAM) — lost on power cycle. Future: save to QSPI flash |
| Expression pedal | No expression pedal / CV input support yet |
| MIDI CC routing | No MIDI CC → parameter mapping |
| Bootloader shortcut | Hothouse footswitch → DFU reset not yet implemented in Rust (C++ has `CheckResetToBootloader()`) |

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
