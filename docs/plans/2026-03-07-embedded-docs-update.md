# EMBEDDED.md Accuracy Update — Bare Seed + USB Walkthrough

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix gaps in EMBEDDED.md so a developer with a bare Daisy Seed + USB cable has a crystal-clear walkthrough with zero gotchas.

**Architecture:** In-place edits to `docs/EMBEDDED.md`. No new files, no code changes.

**Tech Stack:** Markdown only.

---

### Task 1: Fix Prerequisites Section

**Files:**
- Modify: `docs/EMBEDDED.md:119-157`

**Step 1: Add cargo-binutils prerequisite**

After the `rustup target add` step (item 4), add a new item 5 for `cargo-binutils`:

```markdown
5. **cargo-binutils** (provides `cargo objcopy` for creating flashable binaries):

   ```bash
   cargo install cargo-binutils
   rustup component add llvm-tools
   ```
```

**Step 2: Fix probe-rs description**

Change item 5 (now item 6) from:

```markdown
5. **probe-rs** *(optional — needed for Phase 2 defmt output):*
```

to:

```markdown
6. **probe-rs** *(optional — only needed for defmt RTT debug output via SWD probe):*
```

This fixes the misleading claim that probe-rs is needed for Phase 2. bench_kernels outputs via USB serial.

**Step 3: Commit**

```bash
git add docs/EMBEDDED.md
git commit -m "docs(embedded): add cargo-binutils prereq, fix probe-rs description"
```

---

### Task 2: Add Bootloader Behavior and LED Guide

**Files:**
- Modify: `docs/EMBEDDED.md` — insert new subsection after "### Phase 1: Validate Hardware" header (before Option A)

**Step 1: Add bootloader behavior callout**

Insert before Option A:

```markdown
#### Bootloader Behavior

The Electrosmith bootloader lives in the STM32's internal flash (128 KB). On every
power-on or reset, it runs for a **2.5-second grace period**:

- **LED pulses sinusoidally** — bootloader is alive and listening for DFU/media
- **BOOT button extends grace period** — hold to keep listening (acknowledged by rapid blinks)
- After grace period, bootloader jumps to user program (if one is stored in QSPI)
- **No program stored** — stays in grace period indefinitely until DFU flash
- **SOS blink pattern** (3 short, 3 long, 3 short) — invalid binary detected

To enter DFU mode for flashing:

1. **Hold BOOT** button
2. **Press and release RESET** button
3. **Release BOOT** button
4. LED should pulse — bootloader is in DFU mode

> **First-time Daisy:** The bootloader comes pre-flashed from the factory.
> If your Seed has never been used, it will sit in the grace period with
> a pulsing LED — this is normal and means it's ready for DFU.
```

**Step 2: Add "what you should see" callouts to Option A and Option B**

After Option A step 5 ("LED blinks = hardware works"), add:

```markdown
   > **What you see:** Steady on/off blink (~1 Hz). This is the factory blink
   > program, not the bootloader pulse. If you see this, your hardware is good.
```

After Option B's `dfu-util` command and "LED blinks" line, add:

```markdown
> **What you see:** The `dfu-util` output should end with something like:
> ```
> Downloading element to address = 0x90040000, size = XXXX
> Download done.
> File downloaded successfully
> dfu-util: Error during download get_status
> ```
> The "Error during download get_status" is **normal** — it's the `:leave` flag
> causing the device to reset out of DFU mode. After reset, the bootloader copies
> the binary from QSPI to SRAM and jumps. LED blinks = success.

```

**Step 3: Commit**

```bash
git add docs/EMBEDDED.md
git commit -m "docs(embedded): add bootloader behavior guide and LED expectations"
```

---

### Task 3: Improve Phase 2 USB Serial Instructions

**Files:**
- Modify: `docs/EMBEDDED.md:203-233`

**Step 1: Add udev rule for USB serial and expected dfu-util output**

After the Phase 2 `dfu-util` flash command, add:

```markdown
> **dfu-util output:** Same as Phase 1 — "Error during download get_status" is normal.

After flashing, the Daisy resets and runs benchmarks (~1 second), then enumerates
as a USB serial device (CDC ACM). You may need a udev rule for non-root access:

```bash
# If /dev/ttyACM0 shows "permission denied":
sudo tee /etc/udev/rules.d/50-daisy-cdc.rules << 'EOF'
SUBSYSTEMS=="usb", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", \
    MODE="0666", GROUP="plugdev", TAG+="uaccess"
EOF
sudo udevadm control --reload-rules && sudo udevadm trigger
```
```

**Step 2: Add troubleshooting note about ttyACM detection**

After the `cat /dev/ttyACM0` instructions, add:

```markdown
> **Device not appearing?** After flashing, the Daisy needs ~2 seconds to run
> benchmarks and initialize USB. Check with `dmesg | tail` — you should see
> `cdc_acm` and a `/dev/ttyACM*` assignment. If nothing appears, unplug and
> replug USB (the board resets on reconnect).
```

**Step 3: Commit**

```bash
git add docs/EMBEDDED.md
git commit -m "docs(embedded): improve Phase 2 USB serial instructions and troubleshooting"
```

---

### Task 4: Add Troubleshooting Section

**Files:**
- Modify: `docs/EMBEDDED.md` — insert new section before "## Memory Budget"

**Step 1: Add troubleshooting section**

```markdown
---

## Troubleshooting

### USB Cable

The single most common issue. **Charge-only cables have 2 wires (power only)**;
data cables have 4 wires (power + D+/D-). If `lsusb` shows nothing after
entering DFU mode, try a different cable.

### DFU Device Not Detected

```bash
lsusb | grep "0483:df11"
```

Should show `STMicroelectronics STM Device in DFU Mode`. If not:

1. Verify DFU entry: hold BOOT, press/release RESET, release BOOT
2. Try a different USB cable (charge-only cables won't work)
3. Check udev rules (see Prerequisites)
4. Try a different USB port (avoid hubs)

### "Invalid DFU suffix signature" Warning

```
Warning: Invalid DFU suffix signature
A valid DFU suffix will be required in a future dfu-util release!!!
```

This warning is **benign** — `cargo objcopy` outputs raw binaries without DFU
suffix metadata. The flash still works. Ignore it.

### "Error during download get_status"

```
dfu-util: Error during download get_status
```

This is **normal** when using the `:leave` flag in the DFU address. It means
the device reset out of DFU mode after flashing — which is what you want.

### LED Shows SOS Pattern

Three short blinks, three long, three short = the bootloader found an invalid
binary. Common causes:

- Flashed a debug build (too large for 480 KB SRAM limit) — use `--release`
- Wrong linker script (binary targets internal flash instead of SRAM)
- Corrupted flash — re-flash with DFU

### No USB Serial After Flashing bench_kernels

1. Wait 2-3 seconds after reset for USB enumeration
2. Check `dmesg | tail` for `cdc_acm` messages
3. Unplug and replug USB to force re-enumeration
4. Verify the correct device: `ls /dev/ttyACM*`

### Breadboarding Without Soldered Headers

If your Seed has headers but you're not on a carrier board: **DGND and AGND
must be connected to each other**, even when powered only via USB. Without this
connection, the analog ground plane floats and the codec may not initialize.
On a bare Seed with no breakout, this isn't an issue — the PCB connects them
internally.
```

**Step 2: Commit**

```bash
git add docs/EMBEDDED.md
git commit -m "docs(embedded): add troubleshooting section for bare Seed + USB workflow"
```

---

### Task 5: Final Review and Single Squash Commit

**Step 1: Read the full updated EMBEDDED.md end-to-end**

Verify:
- Prerequisites list is complete and correctly numbered (1-6)
- Phase 1 and Phase 2 have "what you see" callouts
- Troubleshooting section appears before "Memory Budget"
- No broken markdown links
- No contradictions with existing content

**Step 2: If individual commits were made, optionally squash into one clean commit**

```bash
git add docs/EMBEDDED.md
git commit -m "docs(embedded): fix prereqs, add bootloader guide, LED expectations, troubleshooting

- Add cargo-binutils as prerequisite (cargo objcopy needs it)
- Fix probe-rs description (not needed for Phase 2, only SWD debug)
- Add bootloader behavior section (grace period, LED patterns, SOS)
- Add 'what you see' callouts to Phase 1 and Phase 2
- Add USB serial udev rule and detection troubleshooting for Phase 2
- Add troubleshooting section (cables, DFU, warnings, SOS, serial)"
```
