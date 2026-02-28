#!/usr/bin/env bash
# smoke_test.sh -- Exhaustive CLI smoke test for all effects and graph topologies.
#
# Processes demos/piano.wav through every effect at mild + heavy settings,
# all waveshape/waveform variants, complex chains, and graph topologies.
# Validates: exit code, file existence, non-silence, no clipping.
#
# Usage:
#   ./scripts/smoke_test.sh           # Full run (all 5 tiers)
#   ./scripts/smoke_test.sh --quick   # Skip graph + edge case tiers
#   ./scripts/smoke_test.sh --clean   # Remove demos/smoke/ and exit
#
# All output goes to demos/smoke/ (gitignored).
# Requires: cargo build -p sonido-cli --release

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SONIDO="$PROJECT_DIR/target/release/sonido"
INPUT="$PROJECT_DIR/demos/piano.wav"
SMOKE_DIR="$PROJECT_DIR/demos/smoke"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# ── Arg parsing ───────────────────────────────────────────────────────

QUICK=0
CLEAN=0
for arg in "$@"; do
    case "$arg" in
        --quick) QUICK=1 ;;
        --clean) CLEAN=1 ;;
        *) echo "Unknown flag: $arg"; echo "Usage: $0 [--quick] [--clean]"; exit 1 ;;
    esac
done

if [[ $CLEAN -eq 1 ]]; then
    if [[ -d "$SMOKE_DIR" ]]; then
        rm -rf "$SMOKE_DIR"
        echo "Removed demos/smoke/"
    else
        echo "demos/smoke/ does not exist"
    fi
    exit 0
fi

# ── Setup ─────────────────────────────────────────────────────────────

if [[ ! -f "$SONIDO" ]]; then
    echo "Building sonido-cli (release)..."
    cargo build -p sonido-cli --release --manifest-path "$PROJECT_DIR/Cargo.toml"
fi

if [[ ! -f "$INPUT" ]]; then
    echo -e "${RED}Error: demos/piano.wav not found${RESET}"
    echo "Generate it with: ./scripts/generate_demos.sh"
    exit 1
fi

mkdir -p "$SMOKE_DIR"

# ── Results tracking ──────────────────────────────────────────────────

PASS_COUNT=0
FAIL_COUNT=0
TOTAL_COUNT=0
FAIL_NAMES=()
FAIL_REASONS=()

record_pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    echo -e "  ${GREEN}PASS${RESET}  $1"
}

record_fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    FAIL_NAMES+=("$1")
    FAIL_REASONS+=("$2")
    echo -e "  ${RED}FAIL${RESET}  $1 — $2"
}

# ── Core test runner ──────────────────────────────────────────────────
# Usage: run_test "test_name" [--allow-silence] <sonido process args...>
#
# Runs: sonido process demos/piano.wav demos/smoke/<test_name>.wav <args>
# Validates:
#   1. Exit code 0
#   2. Output file exists and non-empty
#   3. Peak ≤ 0.0 dBFS (no clipping)
#   4. Peak > -96.0 dBFS (not silent) — unless --allow-silence

run_test() {
    local name="$1"
    shift
    local allow_silence=0
    if [[ "${1:-}" == "--allow-silence" ]]; then
        allow_silence=1
        shift
    fi

    local outfile="$SMOKE_DIR/${name}.wav"

    # Run sonido process
    if ! "$SONIDO" process "$INPUT" "$outfile" "$@" > /dev/null 2>&1; then
        record_fail "$name" "process exited non-zero"
        return
    fi

    # Output file exists and is non-empty
    if [[ ! -s "$outfile" ]]; then
        record_fail "$name" "output missing or empty"
        return
    fi

    # Analyze dynamics — extract peak dBFS
    local dyn_output
    dyn_output=$("$SONIDO" analyze dynamics "$outfile" 2>&1) || true
    local peak
    peak=$(echo "$dyn_output" | grep -oP 'Peak level:\s+\K[-\d.]+' || echo "")

    if [[ -z "$peak" ]]; then
        record_fail "$name" "could not parse peak level"
        return
    fi

    # No clipping: peak ≤ 0.0 dBFS
    if (( $(echo "$peak > 0.0" | bc -l) )); then
        record_fail "$name" "clipping: peak ${peak} dBFS"
        return
    fi

    # Not silent: peak > -96.0 dBFS
    if [[ $allow_silence -eq 0 ]]; then
        if (( $(echo "$peak < -96.0" | bc -l) )); then
            record_fail "$name" "silent: peak ${peak} dBFS"
            return
        fi
    fi

    record_pass "$name"
}

# ── Tier 1: Every effect at mild + heavy settings (38 tests) ─────────

tier_1() {
    echo -e "\n${BOLD}=== Tier 1: Effects at mild + heavy settings ===${RESET}"

    run_test "preamp_mild"    --effect preamp --param gain=3
    run_test "preamp_heavy"   --effect preamp --param gain=18 --param headroom=1

    run_test "distortion_mild"  --effect distortion --param drive=8 --param waveshape=softclip
    run_test "distortion_heavy" --effect distortion --param drive=38 --param waveshape=foldback --param foldback=0.3 --param tone=10

    run_test "compressor_mild"  --effect compressor --param threshold=-12 --param ratio=2 --param attack=20
    run_test "compressor_heavy" --effect compressor --param threshold=-40 --param ratio=20 --param attack=0.1 --param release=10 --param knee=0

    run_test "gate_mild"  --effect gate --param threshold=-30 --param attack=5
    run_test "gate_heavy" --effect gate --param threshold=-6 --param attack=0.1 --param hold=100 --param release=200

    run_test "eq_mild"  --effect eq --param mid_gain=3 --param mid_freq=1000
    run_test "eq_heavy" --effect eq --param low_gain=12 --param mid_gain=12 --param high_gain=12 --param low_q=8 --param mid_q=8 --param high_q=8

    run_test "wah_mild"  --effect wah --param frequency=600 --param resonance=3
    run_test "wah_heavy" --effect wah --param frequency=2000 --param resonance=10 --param sensitivity=1.0 --param mode=auto

    run_test "chorus_mild"  --effect chorus --param rate=0.5 --param depth=0.3 --param mix=0.3
    run_test "chorus_heavy" --effect chorus --param rate=8 --param depth=1.0 --param mix=1.0

    run_test "flanger_mild"  --effect flanger --param rate=0.2 --param depth=0.4 --param mix=0.3
    run_test "flanger_heavy" --effect flanger --param rate=5 --param depth=1.0 --param feedback=0.9 --param tzf=1 --param mix=1.0

    run_test "phaser_mild"  --effect phaser --param rate=0.2 --param depth=0.5 --param mix=0.3
    run_test "phaser_heavy" --effect phaser --param rate=8 --param depth=1.0 --param stages=12 --param feedback=0.95 --param mix=1.0

    run_test "tremolo_mild"  --effect tremolo --param rate=4 --param depth=0.5 --param waveform=sine
    run_test "tremolo_heavy" --effect tremolo --param rate=20 --param depth=1.0 --param waveform=samplehold --param spread=1.0

    run_test "multivibrato_mild"  --effect multivibrato --param depth=0.3
    run_test "multivibrato_heavy" --effect multivibrato --param depth=1.0 --param mix=1.0

    run_test "delay_mild"  --effect delay --param time=200 --param feedback=0.3 --param mix=0.3
    run_test "delay_heavy" --effect delay --param time=1500 --param feedback=0.9 --param mix=0.8 --param ping_pong=1

    run_test "filter_mild"  --effect filter --param cutoff=3000 --param resonance=1
    run_test "filter_heavy" --effect filter --param cutoff=200 --param resonance=10

    run_test "tape_mild"  --effect tape --param drive=4 --param saturation=0.3
    run_test "tape_heavy" --effect tape --param drive=18 --param saturation=1.0 --param hf_rolloff=3000 --param bias=0.2

    run_test "reverb_mild"  --effect reverb --param decay=0.3 --param mix=0.2
    run_test "reverb_heavy" --effect reverb --param decay=0.95 --param damping=0.9 --param predelay=80 --param mix=1.0 --param width=1.0

    run_test "limiter_mild"  --effect limiter --param threshold=-6 --param ceiling=-1
    run_test "limiter_heavy" --effect limiter --param threshold=-30 --param ceiling=-0.1 --param release=10 --param lookahead=10

    run_test "bitcrusher_mild"  --effect bitcrusher --param bit_depth=12 --param downsample=2
    run_test "bitcrusher_heavy" --effect bitcrusher --param bit_depth=2 --param downsample=16 --param jitter=1.0 --param mix=1.0

    run_test "ringmod_mild"  --effect ringmod --param frequency=200 --param depth=0.5 --param mix=0.3
    run_test "ringmod_heavy" --effect ringmod --param frequency=5000 --param depth=1.0 --param waveform=square --param mix=1.0

    run_test "stage_mild"  --effect stage --param width=120 --param balance=0.2
    run_test "stage_heavy" --effect stage --param width=200 --param haas_delay=20 --param phase_l=1 --param dc_block=1 --param bass_mono=1
}

# ── Tier 2: Waveshape/mode variants (8 tests) ────────────────────────

tier_2() {
    echo -e "\n${BOLD}=== Tier 2: Waveshape and waveform variants ===${RESET}"

    run_test "distortion_softclip"   --effect distortion --param drive=15 --param waveshape=softclip
    run_test "distortion_hardclip"   --effect distortion --param drive=15 --param waveshape=hardclip
    run_test "distortion_foldback"   --effect distortion --param drive=15 --param waveshape=foldback
    run_test "distortion_asymmetric" --effect distortion --param drive=15 --param waveshape=asymmetric

    run_test "tremolo_sine"       --effect tremolo --param rate=5 --param depth=0.8 --param waveform=sine
    run_test "tremolo_triangle"   --effect tremolo --param rate=5 --param depth=0.8 --param waveform=triangle
    run_test "tremolo_square"     --effect tremolo --param rate=5 --param depth=0.8 --param waveform=square
    run_test "tremolo_samplehold" --effect tremolo --param rate=5 --param depth=0.8 --param waveform=samplehold
}

# ── Tier 3: Effect chains (4 tests) ──────────────────────────────────

tier_3() {
    echo -e "\n${BOLD}=== Tier 3: Effect chains ===${RESET}"

    run_test "chain_short" \
        --chain "preamp:gain=2|distortion:drive=6|reverb:mix=0.3"

    run_test "chain_long" \
        --chain "preamp|compressor|eq|chorus|delay|reverb|limiter"

    run_test "chain_heavy" \
        --chain "distortion:drive=30|phaser:depth=1|delay:feedback=0.8|reverb:decay=0.9"

    run_test "chain_kitchen_sink" \
        --chain "preamp|distortion|compressor|gate|eq|wah|chorus|flanger|phaser|tremolo|delay|filter|multivibrato|tape|reverb|limiter|bitcrusher|ringmod|stage"
}

# ── Tier 4: Graph topologies (8 tests) ───────────────────────────────

tier_4() {
    echo -e "\n${BOLD}=== Tier 4: Graph topologies ===${RESET}"

    run_test "graph_simple_split" \
        --graph "split(distortion:drive=15; -)"

    run_test "graph_split_3way" \
        --graph "split(chorus; phaser; flanger) | limiter"

    run_test "graph_diamond_delay" \
        --graph "split(delay:time=200; delay:time=500) | reverb:mix=0.3"

    run_test "graph_nested_split" \
        --graph "split(split(distortion; chorus); reverb:mix=1.0) | limiter"

    run_test "graph_chain_in_split" \
        --graph "preamp | split(distortion:drive=20 | chorus; phaser | flanger) | reverb"

    run_test "graph_wetdry_reverb" \
        --graph "split(reverb:decay=0.9,mix=1.0; -) | limiter"

    run_test "graph_heavy_parallel" \
        --graph "split(compressor:ratio=20; tape:drive=15; bitcrusher:bits=4)"

    run_test "graph_full_topology" \
        --graph "preamp:gain=8 | split(distortion:drive=25 | chorus; delay:time=400) | reverb:decay=0.8 | limiter"
}

# ── Tier 5: Edge cases (4 tests) ─────────────────────────────────────

tier_5() {
    echo -e "\n${BOLD}=== Tier 5: Edge cases ===${RESET}"

    run_test "edge_max_output" \
        --chain "preamp:gain=18|distortion:drive=40"

    run_test "edge_gate_silence" --allow-silence \
        --effect gate --param threshold=0

    run_test "edge_extreme_resonance" \
        --effect filter --param cutoff=100 --param resonance=10

    run_test "edge_extreme_delay" \
        --effect delay --param time=50 --param feedback=0.95 --param mix=1.0
}

# ── Main ──────────────────────────────────────────────────────────────

START_TIME=$(date +%s)

echo -e "${BOLD}Sonido CLI Smoke Test${RESET}"
echo -e "${DIM}Input: demos/piano.wav${RESET}"
echo -e "${DIM}Output: demos/smoke/${RESET}"
if [[ $QUICK -eq 1 ]]; then
    echo -e "${DIM}Mode: quick (tiers 1-3 only)${RESET}"
fi

tier_1
tier_2
tier_3

if [[ $QUICK -eq 0 ]]; then
    tier_4
    tier_5
fi

# ── Summary ───────────────────────────────────────────────────────────

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

echo ""
echo -e "${BOLD}=== Smoke Test Summary ===${RESET}"
echo "$TOTAL_COUNT tests | $PASS_COUNT passed | $FAIL_COUNT failed | ${ELAPSED}s"

if [[ $FAIL_COUNT -gt 0 ]]; then
    echo ""
    echo -e "${RED}Failures:${RESET}"
    for i in "${!FAIL_NAMES[@]}"; do
        echo -e "  ${FAIL_NAMES[$i]} — ${FAIL_REASONS[$i]}"
    done
fi

if [[ -d "$SMOKE_DIR" ]]; then
    NFILES=$(find "$SMOKE_DIR" -name '*.wav' | wc -l)
    TOTAL_SIZE=$(du -sh "$SMOKE_DIR" 2>/dev/null | cut -f1)
    echo ""
    echo "Generated $NFILES WAV files in demos/smoke/ ($TOTAL_SIZE total)"
    echo "Listen:  ls -lhS demos/smoke/"
    echo "Play:    sonido play demos/smoke/<file>.wav"
    echo "Remove:  ./scripts/smoke_test.sh --clean"
fi

if [[ $FAIL_COUNT -gt 0 ]]; then
    exit 1
fi
