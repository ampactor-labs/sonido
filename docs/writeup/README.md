# Sonido, from the top

Sonido is a set of 36 audio effects that I wrote once, in Rust, and run in three places: a DAW plugin, a web page, and a guitar pedal.

**One DSP codebase runs as a DAW plugin and bare-metal on an STM32.** The same source for every effect compiles for a laptop and for the 480 MHz Cortex-M7 inside the pedal, where each sample gets 10,000 clock cycles.

![The node-graph editor with a Distortion, Delay and Reverb chain wired between the input and output nodes, six macro knobs, and the A/B morph strip](../img/editor.png)

This page is written in layers. Each layer is complete on its own and points down to the next one, so you can stop wherever you have what you came for.

1. [What it is](#1-what-it-is): one minute
2. [Try it](#2-try-it): five minutes
3. [How it works](#3-how-it-works): twenty minutes
4. [What it does not do](#4-what-it-does-not-do)
5. [The long write-ups](#5-the-long-write-ups): a build log for people who build hardware, and decision notes for people who design DSP
6. [The source documents](#6-the-source-documents): every claim on this page, traced to a file

---

## 1. What it is

Sonido is my DSP project, and I'm the only person working on it. It holds 36 effects: distortion, a preamp, an amp and a cabinet, tape, delay, four reverbs, chorus, flanger, phaser, vibrato, tremolo, wah, filters and EQs, dynamics processors, a bitcrusher and a ring modulator, pitch shifting, granular and drone effects, stereo tools, a looper and a tuner. Each effect is a small piece of code I call a kernel, and the same kernels run in three places:

- **In a DAW.** One `sonido.clap` bundle holds 37 plugins: the 36 effects, plus a graph player that runs a whole rig as one plugin. A `sonido.vst3` shim offers the same set to VST3 hosts.
- **In a web browser.** A node-graph editor at <https://ampactor.dev/sonido/> lets you wire effects together and hear them, with nothing to install.
- **In a guitar pedal.** An Electrosmith Daisy Seed (an STM32H750 microcontroller) sits in a Cleveland Music Co. Hothouse enclosure with six knobs, three toggles, two footswitches, two LEDs and no screen. The pedal plays rigs.

A *rig* is a set of effects wired into a graph, with six macro knobs and an A/B morph between two settings of every parameter. A rig you build in the editor plays the same way in the graph-player plugin and on the pedal, because both run it through the same runtime code.

The code is open source under AGPL-3.0, with a commercial license for anyone shipping it closed-source ([LICENSING.md](../../LICENSING.md)).

## 2. Try it

**In the browser.** Open <https://ampactor.dev/sonido/>, add effects from the palette, and press Play. The page plays a built-in signal generator or a file you load. It runs the same effect code as the plugins, compiled to WebAssembly.

**In a DAW.** Building the plugins needs Rust 1.85 or later. On Linux it also needs the X11 and OpenGL development packages listed in the main [README](../../README.md#plugins).

```sh
git clone https://github.com/ampactor-labs/sonido
cd sonido
make plugins          # builds and installs ~/.clap/sonido.clap
make plugins-vst3     # also builds the VST3 shim (needs a C++ compiler and cmake)
```

Rescan plugins in your host. Every effect draws the same cabinet-style UI with a live scope of its own signal.

**On the command line.** Generate a tone, then run it through the distortion:

```sh
cargo run -p sonido-cli -- generate tone tone.wav --freq 220 --duration 2
cargo run -p sonido-cli -- process tone.wav out.wav --effect distortion --param drive=15
```

**On a pedal.** You need a Daisy Seed, a Hothouse, and a soldering iron. The [build log](build-log.md#building-and-flashing-one) has the parts, the cost and the flashing steps.

## 3. How it works

Each part below is one paragraph. Each one ends with a link to the full explanation.

### Every effect is three layers

```
XxxParams            typed parameters in real units (dB, ms, %)
    │
XxxKernel            the DSP: process_stereo(left, right, &params) -> (left, right)
    │                no allocation, no parameter storage, no smoothing
Adapter<K, Policy>   per-parameter smoothing and the plugin-facing interface
```

The kernel is the part every target shares. It holds only DSP state (filter memory, delay lines, oscillator phases) and receives its parameters by reference on every call. The adapter around it smooths parameter changes over 5 to 50 ms and gives hosts a uniform interface. The plugins, the editor and the pedal all build effects the same way, through one registry, with the same adapter. The parameter struct is also the preset format, so morphing between two presets interpolates between two structs, parameter by parameter (geometrically for frequencies, with switches snapping over at the midpoint). Full write-up: [KERNEL_ARCHITECTURE.md](../KERNEL_ARCHITECTURE.md).

### A rig is a graph

Effects are nodes and audio connections are edges, so parallel paths and merges are ordinary topology. When a rig changes, the graph is compiled once, off the audio thread, into a flat list of steps: a topological sort orders the nodes, and a liveness analysis reuses buffers the way a compiler reuses registers. A 20-effect chain runs on two buffers. Full write-up: [ADR-025](../DESIGN_DECISIONS.md#adr-025-dag-routing-engine).

### Every effect plays at the loudness of its input

At its default settings, every effect (apart from two utilities, the looper and the tuner) plays at the same integrated loudness (LUFS) as the dry signal, and turning its mix knob does not change that loudness. I don't set these levels by ear. A calibration program plays a synthetic strummed guitar through every effect, measures the result, and writes a table of trims into the source. A test holds each of those effects within 0.5 dB of the dry signal when fully wet, and within 1.5 dB across its mix range. This matters most on the pedal: when you switch effects mid-song, a level mismatch is a volume jump. Full write-up: [ADR-031](../DESIGN_DECISIONS.md#adr-031-one-measured-level-contract), and [the level section of the decision notes](decisions.md#6-level-measured-not-chosen).

### A knob at noon selects the default

Each parameter's default value is its voiced sweet spot. On the pedal's jam pages, a knob at 12 o'clock selects exactly that default: the left half of the knob's travel covers the minimum up to the default, and the right half covers the default up to the maximum. A jam effect with every knob at noon and a freshly opened plugin therefore start out identical. Full write-up: [ADR-030](../DESIGN_DECISIONS.md#adr-030-noon-mapping-belongs-in-the-embedded-layer).

### One CPU rule, measured on the chip

At 48 kHz the pedal processes audio in blocks of 32 samples, so each block has 0.67 ms of compute time, or 320,000 clock cycles. I measured every effect's worst block on a Daisy Seed. A rig runs on the pedal only if its effects' worst blocks, plus 400 cycles per node, add up to no more than 80% of that budget. The editor applies the same rule while you build a rig. The pedal applies it again before it loads one. Full write-up: [BENCHMARKS.md](../BENCHMARKS.md#cortex-m7-measured-on-a-daisy-seed).

### One control rule on the pedal

The pedal has no screen, so the footswitches mean the same thing in every rig: the left one turns the pedal on and off, the right one sets tap tempo, and holding either one changes rig. In the jam and the factory rigs, the top row of knobs holds levels and extras, with the top-right knob always a volume, and the bottom row shapes the sound. A rig you build in the editor maps its knobs however you set them. Running `sonido pedal controls` opens an illustrated guide to the pedal, drawn from the same code the firmware runs. Full write-up: [ADR-032](../DESIGN_DECISIONS.md#adr-032-one-control-rule-for-the-pedal).

## 4. What it does not do

Some of these are decisions, and the rest are gaps. I list them both because a list of features says little without its limits.

**On purpose:**

- **No oversampling on any shipping path.** The distortion, amp and tape kernels use antiderivative anti-aliasing (ADAA) instead. An oversampling wrapper exists in the code, but at 4× it costs nearly four times as much CPU, and the pedal can't spare that. [Why](decisions.md#2-aliasing-adaa-instead-of-oversampling).
- **No automatic gain control.** Levels are measured offline and fixed, because a per-sample loudness tracker costs CPU on the pedal.
- **No 64-bit floats.** Everything runs in `f32`, which halves the memory of every delay line and keeps to the Cortex-M7's faster single-precision arithmetic.
- **No screen on the pedal, and no footswitch whose job depends on a mode.** One control rule replaces them.
- **No footswitch gesture that restarts into the bootloader during play.** A firmware update starts from a power-on gesture or a USB request from the computer.

**Not yet:**

- **No MIDI and no expression pedal.** A USB-MIDI parser and expression smoothing exist as library code, but nothing routes them to parameters.
- **The pedal is mono in.** It sums its input to mono, and its converter clips at 0 dBFS, so a pedal and a plugin set to the same values can still sound different at the edges.
- **The jam offers 8 of the 36 effects.** A rig from the editor can use any effect that fits the CPU rule.
- **The cabinet IRs are synthesized.** The code generates them as exponentially decaying filtered noise; they are not measurements of real cabinets. You can't load your own IR yet.
- **Pitch shifting is granular**, with two or four overlapping grains. It is not a phase vocoder, and there is no harmonizer.
- **The distortion's dynamics control changes the drive, not the shape of the clipping curve.** Level-dependent waveshaping, which is what makes a guitar's volume knob clean up an analog circuit, is on the roadmap.
- **Hardware testing is manual.** CI builds, lints and size-checks the firmware, but only I test it, by flashing it and playing. Two of the earlier per-effect VST3 shims passed Steinberg's validator (47/47) once, by hand; the current shim hasn't been through it. Only a manually run CI workflow validates the CLAP plugins.
- **Not a product.** The API is unstable and nothing is on crates.io. Tagged releases on GitHub carry the desktop builds, the plugins and the pedal firmware; v0.2.0 (October 2026) is the latest.

## 5. The long write-ups

- **[Build log: a guitar pedal that runs my plugin code](build-log.md).** For people who build hardware. It covers the parts and what they cost, what happens between a knob and the speaker, where the memory and the CPU go, a dated list of what went wrong and how I found it, and how to build and flash one yourself.
- **[Decision notes: what I built, what I tried, what it cost](decisions.md).** For people who design DSP and pedals. Each section covers one algorithm or system and gives the choice, the alternatives I tried first, and the measured cost.

## 6. The source documents

| If you want | Read |
|---|---|
| The whole project on one page, with build and test commands | [README.md](../../README.md) |
| The three-layer kernel design, and how to add an effect | [KERNEL_ARCHITECTURE.md](../KERNEL_ARCHITECTURE.md), [EFFECT_SDK.md](../EFFECT_SDK.md) |
| All 32 architecture decision records | [DESIGN_DECISIONS.md](../DESIGN_DECISIONS.md) |
| Every effect, every parameter, every default | [EFFECTS_REFERENCE.md](../EFFECTS_REFERENCE.md) |
| The Daisy Seed, the Hothouse, memory maps, flashing, the pedal firmware | [EMBEDDED.md](../EMBEDDED.md) |
| Desktop and Cortex-M7 costs for all 36 effects | [BENCHMARKS.md](../BENCHMARKS.md) |
| Distortion THD and reverb RT60, regenerated by a script | [DSP_MEASUREMENTS.md](../DSP_MEASUREMENTS.md) |
| How the tests are built | [TESTING.md](../TESTING.md) |

### The numbers on this page

| Number | What it is | Where it comes from |
|---|---|---|
| 36 | effects | the registry's own inventory test, `crates/sonido-registry/src/lib.rs` |
| 37 | plugins in one bundle (36 effects and the graph player) | `crates/sonido-plugin/src/bundle.rs` |
| 480 MHz, 10,000 cycles per sample | the pedal's clock, divided by 48,000 samples per second | [EMBEDDED.md](../EMBEDDED.md#daisy-seed) |
| 32 samples, 0.67 ms, 320,000 cycles | one audio block on the pedal | `BLOCK_SIZE` in `crates/sonido-daisy/src/lib.rs` |
| 80% | the most of a block a rig's measured worst blocks may add up to | `CHAIN_FRACTION` in `crates/sonido-platform/src/cycle_table.rs` |
| 74.7% mean, 84–85% slowest | the whole pedal, measured in a Hothouse with a guitar, running distortion, reverb and ring modulator | [EMBEDDED.md](../EMBEDDED.md#chain-configurations), 2026-10-22 |
| 0.5 dB | the most a calibrated effect, fully wet, may differ from the dry guitar's loudness | `crates/sonido-effects/tests/level_contract.rs` |
| 2,011 | `#[test]` functions in the tree | `git grep -ho '#\[test\]' -- crates \| wc -l`, 2026-10-29 |
