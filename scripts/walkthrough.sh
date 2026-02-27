#!/usr/bin/env bash
# walkthrough.sh -- Unified guided tour of the Sonido DSP framework.
#
# Demonstrates the full Sonido experience: CLI signal generation, all 19 effects
# (22 demos including variants), analysis tools, A/B comparison, presets, and
# GUI guidance.
#
# Usage:
#   ./scripts/walkthrough.sh            # Full walkthrough with narration
#   ./scripts/walkthrough.sh --verify   # Verify existing builds/demos work
#   ./scripts/walkthrough.sh --clean    # Remove generated files and start fresh
#
# Requires: Rust toolchain (cargo)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SONIDO="$PROJECT_DIR/target/release/sonido"
DEMOS="$PROJECT_DIR/demos"
START_TIME=$(date +%s)

# --- Color helpers (with fallback for non-color terminals) ---

if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null)" -ge 8 ]; then
    GREEN="\033[0;32m"
    RED="\033[0;31m"
    YELLOW="\033[1;33m"
    CYAN="\033[0;36m"
    BOLD="\033[1m"
    RESET="\033[0m"
else
    GREEN="" RED="" YELLOW="" CYAN="" BOLD="" RESET=""
fi

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

trap 'echo -e "\n${RED}[ERR]${RESET} Unexpected error at line $LINENO"; FAIL_COUNT=$((FAIL_COUNT + 1))' ERR

pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}[PASS]${RESET} $1"
}

fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}[FAIL]${RESET} $1"
}

skip() {
    SKIP_COUNT=$((SKIP_COUNT + 1))
    echo -e "  ${YELLOW}[SKIP]${RESET} $1"
}

header() {
    echo ""
    echo -e "${YELLOW}=== $1 ===${RESET}"
}

cmd_note() {
    echo -e "  ${CYAN}\$ $1${RESET}"
}

# Verify a WAV file exists and is non-empty
verify_wav() {
    local file="$1"
    local label="${2:-$file}"
    if [ -f "$file" ] && [ -s "$file" ]; then
        pass "$label"
        return 0
    else
        fail "$label (file missing or empty)"
        return 1
    fi
}

# --- Mode handling ---

MODE="full"
if [ "${1:-}" = "--verify" ]; then
    MODE="verify"
elif [ "${1:-}" = "--clean" ]; then
    MODE="clean"
elif [ -n "${1:-}" ]; then
    echo "Usage: $0 [--verify | --clean]"
    exit 1
fi

if [ "$MODE" = "clean" ]; then
    echo "Cleaning generated demo files..."
    rm -f "$DEMOS"/src_*.wav "$DEMOS"/fx_*.wav "$DEMOS"/preset_*.wav
    echo "Done. Run $0 to regenerate."
    exit 0
fi

echo -e "${BOLD}Sonido DSP Framework -- Guided Walkthrough${RESET}"
echo "Mode: $MODE"
echo ""

# ============================================================================
# Section 1: Build & Smoke Test
# ============================================================================

header "1. Build & Smoke Test"

if [ "$MODE" = "full" ]; then
    cmd_note "cargo build --release -p sonido-cli"
    if cargo build --release -p sonido-cli --manifest-path "$PROJECT_DIR/Cargo.toml" 2>&1; then
        pass "CLI build succeeded"
    else
        fail "CLI build failed"
        echo "Cannot continue without a working CLI. Exiting."
        exit 1
    fi
else
    if [ -f "$SONIDO" ]; then
        pass "CLI binary exists"
    else
        fail "CLI binary not found at $SONIDO (run without --verify to build)"
    fi
fi

# Version check
if [ -f "$SONIDO" ]; then
    cmd_note "sonido --version"
    VERSION=$("$SONIDO" --version 2>&1 || true)
    if [ -n "$VERSION" ]; then
        pass "Version: $VERSION"
    else
        fail "sonido --version returned empty"
    fi

    # Effect count check
    cmd_note "sonido effects"
    EFFECT_COUNT=$("$SONIDO" effects 2>&1 | grep -c "│" || true)
    if [ "$EFFECT_COUNT" -ge 19 ]; then
        pass "All 19 effects registered ($EFFECT_COUNT found)"
    elif [ "$EFFECT_COUNT" -gt 0 ]; then
        fail "Expected 19 effects, found $EFFECT_COUNT"
    else
        fail "Could not parse effect list"
    fi
fi

# ============================================================================
# Section 2: Demo Generation & Verification
# ============================================================================

header "2. Demo Generation & Verification"

# Expected outputs: 5 source signals + 22 effect demos
EXPECTED_WAVS=(
    "src_sine_440.wav:Sine tone 440 Hz"
    "src_saw_chord.wav:Saw chord pad (C major)"
    "src_perc_adsr.wav:Percussion ADSR"
    "src_sweep.wav:Frequency sweep 20-20 kHz"
    "src_noise.wav:White noise"
    "fx_distortion_soft.wav:Distortion (soft clip)"
    "fx_distortion_hard.wav:Distortion (hard clip)"
    "fx_compressor.wav:Compressor"
    "fx_gate.wav:Noise Gate"
    "fx_chorus.wav:Chorus"
    "fx_flanger.wav:Flanger"
    "fx_phaser.wav:Phaser"
    "fx_tremolo.wav:Tremolo"
    "fx_multivibrato.wav:Multi Vibrato"
    "fx_filter_lowpass.wav:Low Pass Filter"
    "fx_wah.wav:Auto-Wah"
    "fx_parametric_eq.wav:Parametric EQ"
    "fx_reverb_room.wav:Reverb (room)"
    "fx_reverb_hall.wav:Reverb (hall)"
    "fx_delay.wav:Delay"
    "fx_tape_saturation.wav:Tape Saturation"
    "fx_preamp.wav:Clean Preamp"
    "fx_limiter.wav:Limiter"
    "fx_bitcrusher.wav:Bitcrusher"
    "fx_ringmod.wav:Ring Modulator"
    "fx_stage.wav:Stage (stereo utility)"
    "fx_full_chain.wav:Full effect chain (5 effects)"
)

if [ "$MODE" = "full" ]; then
    echo "  Generating source signals and effect demos via generate_demos.sh..."
    cmd_note "./scripts/generate_demos.sh"
    if "$SCRIPT_DIR/generate_demos.sh"; then
        pass "generate_demos.sh completed"
    else
        fail "generate_demos.sh failed"
    fi
    echo ""
fi

echo "  Verifying ${#EXPECTED_WAVS[@]} expected WAV files..."
IDX=0
for entry in "${EXPECTED_WAVS[@]}"; do
    IDX=$((IDX + 1))
    FILE="${entry%%:*}"
    LABEL="${entry#*:}"
    verify_wav "$DEMOS/$FILE" "[$IDX/${#EXPECTED_WAVS[@]}] $LABEL"
done

# ============================================================================
# Section 3: Analysis Demos
# ============================================================================

header "3. Analysis Demos"

if [ "$MODE" = "full" ] && [ -f "$SONIDO" ]; then
    # Spectrum analysis on distortion output
    if [ -f "$DEMOS/fx_distortion_soft.wav" ]; then
        echo ""
        echo -e "  ${BOLD}Spectrum analysis${RESET} (distortion output -- shows added harmonics):"
        cmd_note "sonido analyze spectrum fx_distortion_soft.wav"
        if "$SONIDO" analyze spectrum "$DEMOS/fx_distortion_soft.wav" 2>&1; then
            pass "Spectrum analysis"
        else
            fail "Spectrum analysis"
        fi
    fi

    # Dynamics analysis on compressor output
    if [ -f "$DEMOS/fx_compressor.wav" ]; then
        echo ""
        echo -e "  ${BOLD}Dynamics analysis${RESET} (compressor output -- shows reduced dynamic range):"
        cmd_note "sonido analyze dynamics fx_compressor.wav"
        if "$SONIDO" analyze dynamics "$DEMOS/fx_compressor.wav" 2>&1; then
            pass "Dynamics analysis"
        else
            fail "Dynamics analysis"
        fi
    fi

    # THD measurement on distortion output
    if [ -f "$DEMOS/fx_distortion_soft.wav" ]; then
        echo ""
        echo -e "  ${BOLD}Distortion analysis${RESET} (THD measurement):"
        cmd_note "sonido analyze distortion fx_distortion_soft.wav --fundamental 440"
        if "$SONIDO" analyze distortion "$DEMOS/fx_distortion_soft.wav" --fundamental 440 2>&1; then
            pass "THD measurement"
        else
            fail "THD measurement"
        fi
    fi
else
    skip "Analysis demos (run in full mode)"
fi

# ============================================================================
# Section 4: A/B Comparison
# ============================================================================

header "4. A/B Comparison"

if [ "$MODE" = "full" ] && [ -f "$SONIDO" ]; then
    if [ -f "$DEMOS/src_sine_440.wav" ] && [ -f "$DEMOS/fx_distortion_soft.wav" ]; then
        echo -e "  Comparing clean sine vs. soft-clipped distortion:"
        cmd_note "sonido compare src_sine_440.wav fx_distortion_soft.wav"
        if "$SONIDO" compare "$DEMOS/src_sine_440.wav" "$DEMOS/fx_distortion_soft.wav" 2>&1; then
            pass "A/B comparison"
        else
            fail "A/B comparison"
        fi
    fi
else
    skip "A/B comparison (run in full mode)"
fi

# ============================================================================
# Section 5: Preset Demos
# ============================================================================

header "5. Preset Demos"

if [ "$MODE" = "full" ] && [ -f "$SONIDO" ]; then
    cmd_note "sonido presets list"
    "$SONIDO" presets list 2>&1 || true
    echo ""

    cmd_note "sonido presets show guitar_crunch"
    "$SONIDO" presets show guitar_crunch 2>&1 || true
    echo ""

    echo "  Processing source through each preset..."
    for preset in "$PROJECT_DIR/presets"/*.toml; do
        if [ -f "$preset" ]; then
            name=$(basename "$preset" .toml)
            outfile="$DEMOS/preset_${name}.wav"
            cmd_note "sonido process src_saw_chord.wav preset_${name}.wav --preset $name"
            if "$SONIDO" process "$DEMOS/src_saw_chord.wav" "$outfile" --preset "$preset" 2>&1; then
                verify_wav "$outfile" "Preset: $name"
            else
                fail "Preset: $name (command failed)"
            fi
        fi
    done
else
    # Verify mode: check preset outputs
    for preset in "$PROJECT_DIR/presets"/*.toml; do
        if [ -f "$preset" ]; then
            name=$(basename "$preset" .toml)
            verify_wav "$DEMOS/preset_${name}.wav" "Preset: $name"
        fi
    done
fi

# ============================================================================
# Section 6: GUI & TUI
# ============================================================================

header "6. GUI & TUI"

echo ""
echo -e "  ${BOLD}GUI (graphical real-time processor):${RESET}"

# Check for display server
if [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
    echo "  Display server detected. You can launch the GUI with:"
else
    echo "  No display server detected (headless). GUI requires a display."
    echo "  On a graphical system, launch with:"
fi

echo ""
echo -e "    ${CYAN}cargo run -p sonido-gui --release${RESET}"
echo ""
echo "  Device selection flags:"
echo "    --input <device>       Input audio device"
echo "    --output <device>      Output audio device"
echo "    --buffer-size <N>      Audio buffer size (default: 256)"
echo ""
echo -e "  ${BOLD}TUI (terminal preset editor):${RESET}"
echo ""
echo -e "    ${CYAN}sonido tui --preset guitar_crunch${RESET}"
echo ""
echo "  The TUI is a preset parameter editor. For live audio processing,"
echo "  use the GUI or CLI real-time mode."

pass "GUI/TUI guidance printed"

# ============================================================================
# Section 7: Summary
# ============================================================================

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

header "7. Summary"

TOTAL=$((PASS_COUNT + FAIL_COUNT + SKIP_COUNT))
WAV_COUNT=$(find "$DEMOS" -name "*.wav" 2>/dev/null | wc -l)

echo ""
echo -e "  Results: ${GREEN}$PASS_COUNT passed${RESET}, ${RED}$FAIL_COUNT failed${RESET}, ${YELLOW}$SKIP_COUNT skipped${RESET} ($TOTAL total)"
echo "  WAV files in demos/: $WAV_COUNT"
echo "  Elapsed: ${ELAPSED}s"
echo ""

if [ "$FAIL_COUNT" -eq 0 ]; then
    echo -e "  ${GREEN}${BOLD}All checks passed!${RESET}"
else
    echo -e "  ${RED}${BOLD}$FAIL_COUNT check(s) failed.${RESET} Review output above."
fi

echo ""
echo -e "  ${BOLD}What to listen for -- cheat sheet:${RESET}"
echo "  ---------------------------------------------------------------"
echo "  Distortion (soft)  | Warm saturation, odd harmonics"
echo "  Distortion (hard)  | Harsh buzz, square-wave character"
echo "  Compressor         | Tighter dynamics, louder sustain"
echo "  Gate               | Clean silence between signal bursts"
echo "  Chorus             | Shimmer, widened stereo image"
echo "  Flanger            | Jet-plane swoosh, comb filtering"
echo "  Phaser             | Sweeping notch pattern"
echo "  Tremolo            | Pulsing amplitude, vintage vibe"
echo "  Vibrato            | Pitch wobble, seasick detuning"
echo "  Low Pass Filter    | Muffled sound, resonant peak"
echo "  Auto-Wah           | Quacky envelope filter"
echo "  Parametric EQ      | Boosted/cut frequency band"
echo "  Reverb (room)      | Tight reflections, small space"
echo "  Reverb (hall)      | Long, spacious tail"
echo "  Delay              | Rhythmic echoes"
echo "  Tape Saturation    | Analog warmth, gentle compression"
echo "  Preamp             | Clean volume boost"
echo "  Limiter            | Transparent peak control, brickwall ceiling"
echo "  Bitcrusher         | Lo-fi crunch, retro digital artifacts"
echo "  Ring Modulator     | Metallic, inharmonic sidebands"
echo "  Stage              | Width adjustment, Haas stereo enhancement"
echo "  Full Chain         | Layered: gain -> dirt -> width -> echo -> space"
echo "  ---------------------------------------------------------------"
echo ""
echo "  Demo files: $DEMOS/"
echo "  Play with:  aplay $DEMOS/fx_chorus.wav  (Linux)"
echo "              afplay $DEMOS/fx_chorus.wav  (macOS)"
