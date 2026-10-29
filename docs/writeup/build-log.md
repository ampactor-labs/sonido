# Build log: a guitar pedal that runs my plugin code

I write audio effects in Rust. The same code runs as plugins in a DAW and as firmware in a guitar pedal I built from a kit. This is the pedal's story so far: what's inside, what each part does, what broke and how I found out, and how to build one yourself.

The short version: the pedal is an Electrosmith Daisy Seed in a Cleveland Music Co. Hothouse enclosure. Its processor is an STM32H750, an ARM Cortex-M7 running at 480 MHz with no operating system. Every effect the pedal plays is compiled from the same source files as the plugins. At 48 kHz the clock gives each sample 10,000 cycles of work, and most of the problems in this log come back to that budget.

Contents:

- [What's in the box](#whats-in-the-box)
- [From a knob to the speaker](#from-a-knob-to-the-speaker)
- [Where the memory goes](#where-the-memory-goes)
- [Where the CPU goes](#where-the-cpu-goes)
- [What went wrong, in order](#what-went-wrong-in-order)
- [Playing it](#playing-it)
- [Making your own rigs](#making-your-own-rigs)
- [Building and flashing one](#building-and-flashing-one)
- [What it doesn't do yet](#what-it-doesnt-do-yet)

---

## What's in the box

| Part | What it is | Cost (checked 2026-10-29) |
|---|---|---|
| Electrosmith Daisy Seed, 65 MB version | STM32H750 Cortex-M7 at 480 MHz, 64 MB SDRAM, 8 MB QSPI flash, PCM3060 audio codec (24-bit stereo) | sold separately: Electro-Smith lists it at $28.49 each in a 100-piece case, and single boards cost more at retailers |
| Cleveland Music Co. Hothouse kit | The pedal: main board, 6 knobs, 3 three-way toggles, 2 footswitches, 2 LEDs, stereo 1/4" jacks, enclosure | $119 list (sold out the day I checked); pre-assembled is $40 more |

With the Hothouse kit you solder the pots, switches, LEDs, jacks and ribbon cables yourself; the main board comes with its parts already on it. The Seed plugs into the board. The Hothouse hardware is open source (CC BY-SA 4.0), and Cleveland Music Co.'s build guide is on its [GitHub wiki](https://github.com/clevelandmusicco/HothouseExamples/wiki).

One thing to know before you buy: the current Seed (Rev 7, with the PCM3060 codec) has a noise floor about 15 dB worse than the older Rev 4 (225 µV rms against 40 µV rms). Electrosmith has not published a fix. My firmware has its own codec driver for the Rev 7 board.

You also need a micro-USB cable that carries data. A charge-only cable has two wires instead of four, and the Seed won't show up on your computer.

## From a knob to the speaker

Two things run on the chip at different rates:

```
┌──────────────────────────┐   shared buffer   ┌──────────────────────────────┐
│ control task, 100 Hz     │ ────────────────→ │ audio callback, 1,500 Hz     │
│ reads 6 knobs (ADC),     │   (lock-free)     │ 32 samples in, 32 samples    │
│ 3 toggles, 2 footswitches│ ←──────────────── │ out, every 0.67 ms           │
│ drives the 2 LEDs        │   LED state       │ runs the rig                 │
└──────────────────────────┘                   └──────────────────────────────┘
```

The **control task** reads each knob 100 times a second. A knob is a 10 kΩ pot feeding one of the STM32's analog-to-digital converters, which turns its position into a number between 0 and 1. The toggles and footswitches are plain digital pins. The task writes what it read into a buffer that the audio code can read without waiting.

The **audio callback** runs every time 32 new samples arrive from the codec, which at 48 kHz is every 0.67 ms. The codec's samples move to memory by DMA (direct memory access: a hardware block copies data without the processor's help). DMA fills one half of a buffer while the processor works on the other half. The callback reads the latest knob values, runs the rig on 32 samples, and hands them back to the DMA, which sends them out to the codec.

The callback has to finish inside 0.67 ms, every time. At 480 MHz that is 320,000 clock cycles per block, or 10,000 per sample. If it runs late, the codec plays a gap, and you hear a click. Much of the firmware's design follows from that deadline.

## Where the memory goes

The chip has several kinds of memory, and they run at different speeds:

| Memory | Size | Speed | What I keep there |
|---|---|---|---|
| AXI SRAM | 512 KB (480 KB usable) | no wait states | the program itself |
| DTCM | 128 KB | no wait states | the stack and global variables |
| D2 SRAM | 288 KB | 1 to 2 wait states | the DMA audio buffers |
| SDRAM (external chip) | 64 MB | 4 to 8 wait states | every delay line, reverb tank and looper buffer |
| QSPI flash (external chip) | 8 MB | slow, for storage | the program at rest, 15 rig slots, the saved state |

At power-on the Electrosmith bootloader copies the program from QSPI flash into SRAM and runs it from there. That puts a hard limit on the program's size: 491,520 bytes. The current firmware is 477,944 bytes, about 13 KB under the limit, and CI fails any build that doesn't fit.

The effects' memory lives in the 64 MB SDRAM. A two-second stereo delay takes 776 KB, the reverb 118 KB, and the looper 23 MB (60 seconds of stereo). SDRAM is slow, but the processor's data cache helps a lot here, because a delay line reads and writes memory in order. In practice memory is never the limit on this pedal. CPU time is.

## Where the CPU goes

I measured every effect on the Seed itself. A benchmark firmware runs each effect for 128 blocks and reads the Cortex-M7's cycle counter around each block. The number that matters is the worst block, not the average, because one late block is enough to click. These are the eight effects the pedal's jam mode offers:

| Effect | Worst block (cycles) | Share of the 320,000-cycle budget |
|---|---:|---:|
| reverb | 197,300 | 61.7% |
| distortion | 40,023 | 12.5% |
| delay | 33,003 | 10.3% |
| chorus | 30,392 | 9.5% |
| phaser | 27,301 | 8.5% |
| filter | 14,082 | 4.4% |
| ringmod | 13,642 | 4.3% |
| bitcrusher | 13,151 | 4.1% |

The full table for all 36 effects is in [BENCHMARKS.md](../BENCHMARKS.md#cortex-m7-measured-on-a-daisy-seed). The heaviest effect overall is the tuner, at 63.7%.

The rule: a rig fits if its effects' worst blocks, plus 400 cycles for each node, add up to no more than 80% of the budget. I kept 20% back because the rest of the pedal (DMA, the rig player, the controls) needs time too, and because a real callback's slowest moment runs above its average. I checked the margin by measuring the whole pedal in the enclosure, with a guitar plugged in. Distortion into reverb into ring modulator is estimated at 79.4% under the rule. It measured 74.7% on average and 84–85% at its slowest callbacks, and no block ran over.

The same rule runs in three places: the editor's status bar while you build a rig, the command-line tool before it sends one, and the pedal itself before it loads one.

## What went wrong, in order

These are the problems I hit, in order. Each one says what I saw or heard, what caused it, and what I changed.

**2026-03-06: first code on the chip.** The first firmware blinked an LED and timed the effects. Early on I switched to the bootloader's BOOT_SRAM mode, where the program runs from fast internal SRAM instead of directly from flash. That is why the 480 KB limit exists.

**2026-03-09: the wrong block size.** My code assumed 128-sample blocks. The audio driver I was using at the time had 32 hard-coded. I changed mine to 32, and later replaced that driver with my own.

**2026-03-10: everything was too slow.** A timing run of the 19 effects I had then added up to 791% of the budget, and the reverb alone took 372%. Three kinds of change fixed it. I turned on the Cortex-M7's instruction and data caches, told the compiler which CPU it was targeting, and replaced the slowest operations in the inner loops: a library `sin` with a fast approximation, cubic interpolation with linear in the modulated delay lines, and no interpolation at all in the lines that don't move. The reverb dropped from 1,191,763 cycles per block to 159,033, a 7.5× speedup, and the total dropped by 4.5×. After that, every effect fit the budget on its own.

**2026-03-10: turning on the cache in the wrong order.** With the data cache enabled while the audio DMA was starting up, the DMA fell behind and the codec reported overruns. The cache's start-up sequence stalls the memory bus, and the DMA controller couldn't get its turn. The fix was ordering. The firmware now turns the cache on immediately after setting up memory, before the audio starts.

**2026-03-13: reading the knobs crashed the chip.** My first pedal code read the knobs inside the audio callback. An analog-to-digital conversion waits for the converter to finish, and those waits pushed the callback past its deadline. The result was DMA overruns, and sometimes a hard fault that stopped the processor. The knob reads moved into their own task, which is the two-loop design shown above, and the audio callback only reads numbers the control task has already stored. The same change smoothed each knob reading with a low-pass filter set to match the default in the Daisy's C++ library. It took 460 ms to reach 90% of a knob turn, and I removed it five days later.

**2026-03-19: knobs that wouldn't take over.** When you switch rigs or sounds, each knob's position no longer matches the value it controls. My first approach locked each knob until you swept it to within 5% of the stored value, which is how many synths handle this. Without a screen, though, you can't see where that value is, so a locked knob seemed dead. In October I changed it: now a knob takes over once it has moved 2% of its travel.

**April to October: a footswitch gesture that could reboot the pedal.** From April the firmware had a gesture to restart into the bootloader for firmware updates: hold both footswitches for 1.5 seconds. Both footswitches together also meant bypass, and one bypass stomp lasted long enough to reboot the pedal mid-play. I raised the hold to 3 seconds, which only made it less likely, and later the same day I took the bootloader off the footswitches entirely. Now you reach it only by holding both footswitches while you power on, or by sending a request over USB.

**May: half of a knob did nothing.** The delay time runs from 1 ms to 2 s. The bottom half of the knob's travel was short delays that sounded like comb filtering, not echoes. On the pedal, the knob now starts at 30 ms. A plugin still offers the full range, because there you can type a value. In October the filter cutoff knob got a 150 Hz floor and the bitcrusher a 5-bit floor, for the same reason.

**October: a jittery knob cost CPU.** Pot readings jitter a little even when nobody touches them. Now each knob has a dead band: a reading has to move more than 0.15% of the knob's travel before the stored value changes, so a knob you aren't touching reads exactly the same value on every poll. This mattered for CPU, not only for feel. While a knob jitters, the parameter it drives keeps smoothing toward a new value and recalculating its coefficients, and one jittery knob kept a reverb rig about 8 points busier.

**October: the tuner skipped every 43 ms.** The tuner looks for pitch in a 2,048-sample window and updates every 1,024 samples. It did that whole analysis inside one audio block: 9,166,077 cycles, 29 times the budget, so every update was a dropout. It now spreads the work across the blocks between updates, one step per sample, and its worst block is 63.7%.

**October: every rig change dropped audio three or four times.** The firmware uses Embassy, a Rust framework that runs many tasks on one core. A task gives up the processor only when it reaches a point where it waits. Building a rig is a long stretch of work: up to 12 ms for one factory rig, and up to 5.7 ms for a single effect. With everything on one executor, the audio had about 0.6 ms of slack, so it waited behind the rig loader and missed blocks. I moved the audio task to its own interrupt-priority executor. It now interrupts anything else at any instruction. While a new rig is being built, the pedal plays the dry signal, and then it crossfades to the new rig.

**October: three smaller dropouts.** All three were long stretches of work running somewhere the audio was waiting on them:

- The cabinet simulator built its impulse response a second time inside its first audio block: 12,288 software sine and cosine calls, about 4 ms. It now keeps the response it was built with.
- Reading one 4 KB sector from flash blocked for 1.3 ms, so every save of the pedal's state dropped audio. Flash reads now go 256 bytes at a time, about 80 µs each, and let other work run in between.
- A newly loaded rig started every parameter at its default and smoothed toward the knob positions. Every filter coefficient was recalculated on every sample at once, and the load meter showed a 116% peak at boot. A new rig now starts at its values instead of sweeping to them.

After those fixes I ran two rounds through every factory rig and the jam, plus writing and erasing a rig over USB. The codec reported zero overruns, and the worst block was 91.4%.

**October: the bootloader trap.** The Seed has two USB bootloaders. They appear to the computer as the same USB device, but only the Electrosmith one can write to the QSPI flash where the firmware lives. If you hold BOOT while you release RESET, you get the STM32's built-in ROM bootloader instead, and flashing fails with a "not writeable" error. A plain RESET starts the Electrosmith bootloader. Also, one Seed arrived in October 2026 without the Electrosmith bootloader at all. The [embedded guide](../EMBEDDED.md#bootloader-behavior) shows how to tell which one you have.

## Playing it

The pedal has no screen, so every control does the same job in every rig:

| Control | What it does |
|---|---|
| Left footswitch, tap | On/off. Bypass fades to the dry signal over 5 ms while the rig keeps running. |
| Left footswitch, hold 0.4 s | Previous rig |
| Right footswitch, tap | Tap tempo. With the middle toggle in its middle position, it glides between sound A and sound B instead. |
| Right footswitch, hold 0.4 s | Next rig |
| Middle toggle (T2) | Up: sound A. Middle: morph. Down: sound B. |
| Top row of knobs | Levels. The top-right knob is always a volume. |
| Bottom row of knobs | The three controls that shape the sound most |
| Left LED | On while the pedal is on. It blinks the rig's number after a change, or an error code if a rig was refused. |
| Right LED | Flashes the tempo. In morph, it is dark at A, lit at B, and flickers while gliding. |

Holding a footswitch acts at 0.4 s, while your foot is still down, so you don't have to guess when to let go. Running `sonido pedal controls` opens an illustrated guide to the pedal in your browser: a drawing with every control labeled, generated from the same code the firmware's control loop calls.

The rig list wraps around. It starts with the **jam**: three slots you fill from eight effects using the knobs, wired in a line, in parallel, or as one effect feeding two parallel ones (set with T3). Next come up to 15 rigs you've sent from your computer, then six factory rigs:

| Rig | Signal path | Bottom-row knobs |
|---|---|---|
| Shimmer Cathedral | pitch shift into plate reverb, beside a delay | shimmer, wash, echo |
| Fuzz Lab | distortion and filter beside bitcrusher and ring modulator, into tape | fuzz, filter, crush |
| Space Echo | preamp, tape, delay, spring reverb | echo, spring, grit |
| Granular Drift | glitch beside time stretch and texture, into plate reverb | cloud, stretch, stutter |
| Harmonic Habitat | a pitch-following reverb, chorus, delay | habitat, chorus, echo |
| Amp Stack | noise gate, amp, cabinet | bass, mid, treble |

The pedal starts up in the rig you played last, and it saves its state to flash two seconds after you stop changing things.

## Making your own rigs

You build rigs in the node editor. It runs in a browser at <https://ampactor.dev/sonido/> or as a desktop app. Wire up effects, map any parameters to the six macro knobs, set an A and a B for the morph, and give the rig a name. The status bar applies the pedal's CPU rule as you build: green fits, yellow is close, red won't load.

To get a rig onto the pedal:

- **From the desktop editor:** Export ▸ Send to pedal. It replaces the rig with the same name, or takes the first empty slot.
- **From the command line:** `sonido pedal push my_rig.json`. Use `sonido pedal list` to see what's on the pedal, and `sonido pedal play "Space Echo"` to switch to a rig.
- **From the browser:** Send to pedal goes over Web Serial, in Chrome or Edge. I've built this but haven't yet tested it against a real pedal.

Over USB, the pedal shows up as a serial device named "Sonido Pedal". It checks every rig before storing it. If a rig is refused, the left LED blinks a code:

| Blinks | Meaning |
|---:|---|
| 2 | empty or corrupt slot |
| 3 | uses an effect this firmware doesn't have |
| 4 | too heavy for the CPU or memory |
| 5 | wiring that doesn't build |

## Building and flashing one

1. **Build the Hothouse kit** following Cleveland Music Co.'s guide, and seat the Daisy Seed.
2. **Check the bootloader.** Press RESET on the Seed. If the LED pulses slowly for about 2.5 seconds, the Electrosmith bootloader is installed. If not, start the chip's ROM bootloader (hold BOOT while you press and release RESET), open [flash.daisy.audio](https://flash.daisy.audio/) in Chrome, and use "Flash Latest Bootloader". After that, use a plain RESET.
3. **Get the firmware.** CI builds `sonido_pedal.bin` on every push and keeps it as a downloadable artifact named `sonido_pedal`. The release workflow attaches it to releases, but the one release so far (v0.1.0) came out before that was added. To build it yourself instead:

   ```sh
   rustup target add thumbv7em-none-eabihf
   cd crates/sonido-daisy
   cargo build --release --example sonido_pedal --features alloc,platform
   cargo objcopy --release --example sonido_pedal --features alloc,platform -- \
       -O binary -R .sram1_bss -R .sdram_bss sonido_pedal.bin
   ```

   (`cargo objcopy` comes from `cargo install cargo-binutils` plus `rustup component add llvm-tools`.)
4. **Flash it.** With `dfu-util` installed, press RESET and, within the 2.5-second window, run:

   ```sh
   dfu-util -a 0 -s 0x90040000:leave -D sonido_pedal.bin
   ```

   A message ending in `Error during download get_status` is normal: the `:leave` flag resets the Seed right after the transfer. Once the pedal is running Sonido, later updates don't need the RESET button. Run `sonido pedal update sonido_pedal.bin` and it asks the pedal to restart into the bootloader, then flashes.
5. **On Linux,** add a udev rule so you can flash without `sudo`. The rule and the rest of the setup are in the [embedded guide](../EMBEDDED.md#prerequisites).

If something doesn't work, the firmware crate also contains smaller test programs, ordered from simplest to fullest: an LED blink, audio passthrough, a hardware diagnostic that reports every knob, toggle and footswitch, then a single effect. Each one tests one more layer than the last. They're listed in the [embedded guide's tier table](../EMBEDDED.md#tier-system).

## What it doesn't do yet

- **It's mono in.** The pedal sums its stereo input to mono before the rig.
- **No MIDI and no expression pedal.** The parsing code exists but isn't connected to anything yet. The Hothouse has free pins for MIDI over UART and for an I2C display.
- **The jam offers 8 of the 36 effects.** Rigs from the editor can use any effect that fits the CPU rule.
- **Nothing tests the hardware automatically.** CI builds the firmware, lints it and checks its size, but nothing runs it. I test it by flashing it and playing. The CPU table is measured by hand, too: if I change an effect, the pedal's numbers stay stale until I re-run the benchmark. A rebuild alone can move an unchanged effect's timing by up to 8%, because the compiler lays the code out differently. That is part of why the rule keeps a 20% margin.
- **Browser-to-pedal sending is untested** against real hardware.

Everything above comes from files in this repository. The [embedded guide](../EMBEDDED.md) has the full technical detail, and [the decision notes](decisions.md) explain the DSP choices behind the effects.
