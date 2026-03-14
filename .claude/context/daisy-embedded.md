# Daisy Embedded Context

On-demand reference for embedded/Daisy Seed work in sonido-daisy.

## Hardware at a Glance

| Spec | Value |
|------|-------|
| MCU | STM32H750IBK6 (ARM Cortex-M7, single core) |
| Clock | 480 MHz (libDaisy defaults to 400 MHz for thermal headroom) |
| FPU | Single-precision hardware FPU (no double, no SIMD) |
| SDRAM | 64 MB (IS42S16400J) -- "65 MB" variant |
| QSPI Flash | 8 MB (IS25LP064A) |
| Audio Codec | PCM3060 (TI) -- Rev 7, current production |
| Audio | 24-bit stereo, up to 96 kHz |
| GPIO | 31 configurable pins (12x 16-bit ADC, 2x 12-bit DAC) |

Rev 7 noise floor: ~15 dB worse than Rev 4. Use `--features=seed_1_2` with daisy-embassy.

## Memory Map

| Region | Address | Size | Wait States | Use |
|--------|---------|------|:-----------:|-----|
| ITCM | `0x0000_0000` | 64 KB | 0 (instr) | Code hot paths |
| DTCM | `0x2000_0000` | 128 KB | 0 (data) | Audio buffers, stack, hot DSP state |
| AXI SRAM | `0x2400_0000` | 512 KB (480 KB usable) | 0-1 | Delay lines, reverb buffers, heap |
| D2 SRAM1 | `0x3000_0000` | 128 KB | 1-2 | DMA buffers (SAI audio) |
| D2 SRAM2 | `0x3002_0000` | 128 KB | 1-2 | DMA buffers |
| D2 SRAM3 | `0x3004_0000` | 32 KB | 1-2 | Small peripheral buffers |
| D3 SRAM4 | `0x3800_0000` | 64 KB | 1-2 | Low-power domain |
| SDRAM | `0xC000_0000` | 64 MB | 4-8 | Long delay lines (>500ms), loopers |

Total internal SRAM: 1 MB. DTCM is fastest but only 128 KB.

## ADR-029 Constraints

All hardware decisions must serve and empower the kernel architecture -- not the reverse.

- **ADC -> `from_knobs()`**: The canonical hardware->DSP bridge. Every knob/CV input maps to a normalized `KernelParams` via `from_knobs(adc_0..adc_5)`. No HAL types cross this boundary.
- **Audio format**: SAI delivers `u32` (24-bit left-justified). Hardware examples convert at the boundary; `DspKernel::process_stereo()` always receives `f32` pairs.
- **Sample rate**: Hardware determines it; kernels accept it at init. Never hardcode.
- **No HAL types in kernel code**: `DspKernel`, `KernelParams`, and all DSP math are `no_std`, HAL-free. Hardware code imports kernel code; kernel code never imports hardware code.
- **Smoothing belongs to the platform layer**: `KernelAdapter` owns `SmoothedParam`. Direct kernel use (embedded) skips smoothing -- ADCs are hardware-filtered. See ADR-028.

## Control/Audio Architecture

```
hothouse_control_task (50 Hz)  --ControlBuffer-->  Audio Callback (1500 Hz)
  ADC blocking_read (6 knobs)     (lock-free)        read_knob() / read_toggle()
  GPIO toggle reads                                   read_footswitch()
  GPIO footswitches                                   write_led() (LED bridge)
  IIR smoothing (alpha=0.1)    <--LED bridge--
  LED GPIO output
```

- **ControlBuffer**: Lock-free atomics, IIR smoothing (alpha=0.1 at 50Hz), change detection (eps 3e-4), LED bridge
- **HothouseBuffer** = `ControlBuffer<6, 3, 2, 2>` (6 knobs, 3 toggles, 2 footswitches, 2 LEDs)
- **ADC**: Uniform `blocking_read()` for all 6 knobs (~48us/cycle, 0.24% CPU). No DMA needed.
- **Audio DMA**: DMA1_CH0/CH1 for SAI TX/RX; control path uses no DMA
- **Toggle encoding**: 0=UP, 1=MID, 2=DN (standardized across all examples)

## Library Modules

| Module | Purpose |
|--------|---------|
| `controls.rs` | `ControlBuffer<KNOBS,TOGGLES,FS,LEDS>` -- lock-free shared state with IIR smoothing, change detection, LED bridge |
| `hothouse.rs` | `HothouseControls` (knobs + GPIO), `hothouse_control_task` (50Hz polling), `hothouse_pins!` macro, `decode_toggle` |
| `embedded_adapter.rs` | `EmbeddedAdapter<K>` -- zero-smoothing `Effect + ParameterInfo` for `DspKernel` |
| `param_map.rs` | `adc_to_param()` -- scale-aware ADC->parameter conversion with STEPPED rounding |

## Feature Flags

| Feature | Enables | Required By |
|---------|---------|-------------|
| *(none)* | Core library: audio, controls, hothouse, LED, RCC | Simple examples (blinky, passthrough) |
| `alloc` | `EmbeddedAdapter`, `adc_to_param`, DSP-dependent modules | morph_pedal, bench_kernels |
| `platform` | `HothousePlatform` (`PlatformController` impl) + implies `alloc` | Future platform integration |

## ADC/Pin Mapping

- Knob pins: `AnyAdcChannel<'static, ADC1>` (type-erased via `degrade_adc()` in `hothouse_pins!` macro)
- `hothouse_pins!` macro: extracts control pins from Embassy peripherals (call BEFORE `AudioPeripherals`)
- Pin mapping verified against official Cleveland Music Co. hothouse.cpp: D16-D21 = KNOB_1-KNOB_6

## Example Tier Map

| Tier | Example | What It Validates | Hardware |
|:----:|---------|-------------------|----------|
| 1 | `blinky_bare.rs` | Toolchain, flash, BOOT_SRAM path | Seed + USB |
| 1 | `blinky.rs` | Embassy runtime + clock init | Seed + USB |
| 1 | `heap_test.rs` | SRAM clock enable + heap allocation | Seed + USB |
| 2 | `bench_mini.rs` | Single kernel DWT cycle benchmark | Seed + USB |
| 2 | `bench_kernels.rs` | All 19 kernels cycle counts | Seed + USB |
| 3 | `silence.rs` | Codec/SAI/DMA init -- digital silence | Hothouse |
| 3 | `tone_out.rs` | DAC signal path (440 Hz sine) | Hothouse |
| 3 | `square_out.rs` | DAC health (1 kHz full-scale square) | Hothouse |
| 3 | `passthrough.rs` | Codec, DMA, audio passthrough | Seed + audio I/O |
| 3 | `passthrough_blink.rs` | Audio + LED heartbeat coexistence | Hothouse |
| 3 | `hothouse_diag.rs` | All hardware (knobs, toggles, FS, temp) | Hothouse |
| 4 | `single_effect.rs` | Real-time DSP, ADC param mapping | Hothouse |
| 5 | `morph_pedal.rs` | EmbeddedAdapter + ProcessingGraph + A/B morph | Hothouse |

## Embassy Patterns

- **Clock**: `sonido_daisy::rcc_config(ClockProfile::Performance)` (480 MHz) or `::Efficient` (400 MHz)
- **Audio**: `AudioPeripherals` + `start_callback()` -- async loop, yields every DMA transfer (~0.667 ms at 48 kHz)
- **LED**: `sonido_daisy::heartbeat` -- shared lub-dub blink task. Every binary: `spawner.spawn(heartbeat(UserLed::new(p.PC7))).unwrap();`
- **StaticCell**: Used for USB buffers -- avoids `static mut` and `unsafe`
- **Task return type**: `async fn task(...) { }` (implicit `()` return), not `-> !`
- **Audio callback is real-time**: NEVER block. No ADC reads, no USB, no allocation. Only pure DSP math + lock-free ControlBuffer reads.
- **D-cache timing**: Enable D-cache AFTER SAI DMA is running. Use `deferred_dcache()` task with ~500ms delay.

## DFU Flashing

**Press RESET** (never "hold BOOT" alone). LED pulses sinusoidally for 2.5s. Flash within this window:

```bash
cd crates/sonido-daisy
cargo objcopy --example <name> --release -- -O binary -R .sram1_bss <name>.bin
dfu-util -a 0 -s 0x90040000:leave -D <name>.bin
```

"Error during download get_status" is **normal** (`:leave` flag resets device).

## Compile Commands

```bash
# Basic cross-compile check (no alloc-gated examples)
cargo check --target thumbv7em-none-eabihf --examples \
    --manifest-path crates/sonido-daisy/Cargo.toml

# Alloc-gated examples (morph_pedal)
cargo check --target thumbv7em-none-eabihf \
    --example morph_pedal --features alloc \
    --manifest-path crates/sonido-daisy/Cargo.toml
```

## Key Files

`crates/sonido-daisy/` -- src/lib.rs (firmware + heartbeat), src/controls.rs (lock-free ControlBuffer), src/hothouse.rs (control task + pins), src/embedded_adapter.rs (EmbeddedAdapter), src/param_map.rs (adc_to_param), src/rcc.rs (clocks), src/audio.rs (PCM3060), src/sdram.rs (64MB FMC+MPU), src/adc.rs, src/led.rs, memory.x

See `docs/EMBEDDED.md` for complete details including memory layout, SAI configuration, morph pedal architecture, and troubleshooting.
