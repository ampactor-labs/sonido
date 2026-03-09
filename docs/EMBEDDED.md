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
- **Block size** — 32 samples default in libDaisy C++ (0.67 ms at 48 kHz). Sonido uses 128 samples (2.67 ms) — see `BLOCK_SIZE` in `sonido-daisy/src/lib.rs`
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

### Modern Rust on Daisy Seed

**daisy-embassy** is the canonical approach. Key patterns:

- **Audio**: `start_callback()` loop — async, yields every DMA transfer (~0.667 ms at 48 kHz, 32-sample blocks). Other tasks run between transfers.
- **LED / UI**: Use `sonido_daisy::heartbeat` — the shared 1 Hz blink task in `src/lib.rs`. Every binary spawns it before audio init: `let led = board.user_led; spawner.spawn(heartbeat(led)).unwrap();`. Never define a local heartbeat.
- **USB / Serial**: Same spawned-task pattern. See `audio_input_diag.rs`.
- **Audio callback runs in executor context** (not ISR) — it is safe to call Embassy primitives from the callback.
- **Task return type**: Use `async fn task(...) { }` (implicit `()` return), not `-> !`. Embassy 0.9 task macro behavior with `-> !` is unverified on STM32H750.

Reference implementation: `~/.cargo/registry/src/.../daisy-embassy-0.2.3/examples/passthrough.rs`

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

Not possible on bare Seed without wiring up the codec.
`examples/passthrough.rs` is a stub awaiting the daisy-embassy audio interface
builder (handles codec init and DMA setup).

### Phase 4: Single Effect

*Requires audio I/O + potentiometer on an ADC pin.*

Wire one ADC pin to a pot, process audio through a kernel with `from_knobs()`
mapping ADC readings to parameters.
`examples/single_effect.rs` is a stub.

### Diagnostics

Audio output and input diagnostic tools for isolating hardware issues on the
Hothouse. These are not numbered tiers — use them when the audio path is
misbehaving and you need to determine whether the problem is digital, analog,
input-side, or output-side.

| Example | What It Tests | Output |
|---------|---------------|--------|
| `tone_out.rs` | DAC → analog output path | 440 Hz sine wave to output jack |
| `square_out.rs` | DAC → analog output path (max amplitude) | 1 kHz full-scale square wave to output jack |
| `audio_input_diag.rs` | Codec ADC input noise floor | RMS/peak/dBFS via USB serial every 1 second |
| `temp_diag.rs` | STM32H750 CPU temperature | °C + session min/max via USB serial every 2 seconds; warns above 80°C |

#### Build & Flash

All three diagnostics follow the same pattern. From `crates/sonido-daisy/`:

```bash
cd crates/sonido-daisy
cargo objcopy --example <name> --release -- -O binary -R .sram1_bss <name>.bin
dfu-util -a 0 -s 0x90040000:leave -D <name>.bin
```

For example, to flash `tone_out`:

```bash
cargo objcopy --example tone_out --release -- -O binary -R .sram1_bss tone_out.bin
dfu-util -a 0 -s 0x90040000:leave -D tone_out.bin
```

`audio_input_diag` reports over USB serial after flashing:

```bash
cat /dev/ttyACM0
# or: screen /dev/ttyACM0 115200
```

Output repeats every second:

```
IN: rms=0.0023 peak=0.0089 dBFS=-52.8
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

3. **Flash `audio_input_diag`** — with nothing plugged into the input jack,
   read the USB serial output and check the RMS level.
   - **High RMS with nothing plugged in** → noise is being injected before the
     codec ADC (input op-amp oscillation, ground loop, or power supply issue).
   - **Low RMS with nothing plugged in** → input side is clean; the problem is
     output-only.
   - **RMS changes when a cable/instrument is plugged in** → codec ADC is
     reading real signal. The analog input path works.

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
| Memory placement | Large buffers (delay >500ms) need linker sections for SDRAM |
| Block processing | Biquad/SVF have per-sample `process()` only — block version would improve CM7 cache behavior |

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
