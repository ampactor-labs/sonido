#!/usr/bin/env bash
# measure_all.sh -- Automated measurement suite for all Sonido effects.
#
# Generates test signals, processes through every effect at defaults,
# measures dynamics + distortion, prints a summary table.
#
# Usage: ./scripts/measure_all.sh
#
# All output goes to demos/ (WAVs gitignored).
# Requires: cargo build -p sonido-cli --release

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SONIDO="$PROJECT_DIR/target/release/sonido"
OUT="$PROJECT_DIR/demos"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

# Build if needed
if [ ! -f "$SONIDO" ]; then
    echo "Building sonido-cli (release)..."
    cargo build -p sonido-cli --release --manifest-path "$PROJECT_DIR/Cargo.toml"
fi

mkdir -p "$OUT"

# ── Test signal generation ──────────────────────────────────────────────

echo -e "${BOLD}=== Generating test signals ===${RESET}"

echo "  Sine 440 Hz, 2s..."
"$SONIDO" generate tone "$OUT/ref_sine.wav" \
    --freq 440.0 --duration 2.0 --amplitude 0.8

echo "  Sweep 20-20kHz, 3s..."
"$SONIDO" generate sweep "$OUT/ref_sweep.wav" \
    --start 20.0 --end 20000.0 --duration 3.0 --amplitude 0.7

echo "  Chord (C major saw), 2s..."
"$SONIDO" generate chord "$OUT/ref_chord.wav" \
    --notes "60,64,67" --waveform saw --duration 2.0 --amplitude 0.5 \
    --filter-cutoff 3000.0 --attack 20.0 --release 500.0

# ── Effect list ─────────────────────────────────────────────────────────

EFFECTS=(
    distortion
    compressor
    chorus
    delay
    flanger
    phaser
    filter
    multivibrato
    tape
    preamp
    reverb
    tremolo
    gate
    wah
    eq
    limiter
    bitcrusher
    ringmod
    stage
)

# ── Parse helpers ───────────────────────────────────────────────────────

parse_peak() {
    # Extract "Peak level:     -2.1 dBFS" → "-2.1"
    grep -oP 'Peak level:\s+\K[-\d.]+' <<< "$1" || echo "N/A"
}

parse_rms() {
    grep -oP 'RMS level:\s+\K[-\d.]+' <<< "$1" || echo "N/A"
}

parse_crest() {
    grep -oP 'Crest factor:\s+\K[-\d.]+' <<< "$1" || echo "N/A"
}

parse_thd() {
    # Extract "THD:         2.3456% (-32.6 dB)" → "2.3456"
    grep -oP 'THD:\s+\K[\d.]+(?=%)' <<< "$1" || echo "N/A"
}

# ── Process + measure ───────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== Processing effects at defaults ===${RESET}"

# Table header
HEADER=$(printf "%-16s | %10s | %10s | %8s | %8s | %s" \
    "Effect" "Peak dBFS" "RMS dBFS" "Crest dB" "THD%" "Status")
SEP=$(printf "%-16s-|-%10s-|-%10s-|-%8s-|-%8s-|-%s" \
    "----------------" "----------" "----------" "--------" "--------" "------")

# Collect results
RESULTS=()
FAIL_COUNT=0

for fx in "${EFFECTS[@]}"; do
    echo -n "  $fx... "

    # Process all 3 test signals through this effect
    "$SONIDO" process "$OUT/ref_sine.wav"  "$OUT/${fx}_sine.wav"  --effect "$fx" > /dev/null 2>&1
    "$SONIDO" process "$OUT/ref_sweep.wav" "$OUT/${fx}_sweep.wav" --effect "$fx" > /dev/null 2>&1
    "$SONIDO" process "$OUT/ref_chord.wav" "$OUT/${fx}_chord.wav" --effect "$fx" > /dev/null 2>&1

    # Measure dynamics on sine output
    DYN=$("$SONIDO" analyze dynamics "$OUT/${fx}_sine.wav" 2>&1) || DYN=""
    PEAK=$(parse_peak "$DYN")
    RMS=$(parse_rms "$DYN")
    CREST=$(parse_crest "$DYN")

    # Measure distortion on sine output
    DIST=$("$SONIDO" analyze distortion "$OUT/${fx}_sine.wav" --fundamental 440.0 2>&1) || DIST=""
    THD=$(parse_thd "$DIST")

    # PASS/FAIL: peak must be ≤ -1 dBFS
    STATUS="PASS"
    if [[ "$PEAK" != "N/A" ]]; then
        # Compare: fail if peak > -1.0 (i.e. closer to 0 than -1)
        if (( $(echo "$PEAK > -1.0" | bc -l) )); then
            STATUS="FAIL"
            ((FAIL_COUNT++)) || true
        fi
    else
        STATUS="ERR"
        ((FAIL_COUNT++)) || true
    fi

    RESULTS+=("$(printf "%-16s | %10s | %10s | %8s | %8s | %s" \
        "$fx" "$PEAK" "$RMS" "$CREST" "$THD" "$STATUS")")

    if [[ "$STATUS" == "PASS" ]]; then
        echo -e "${GREEN}ok${RESET}"
    elif [[ "$STATUS" == "FAIL" ]]; then
        echo -e "${RED}FAIL (peak $PEAK dBFS)${RESET}"
    else
        echo -e "${YELLOW}ERR${RESET}"
    fi
done

# ── Summary table ───────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== Measurement Summary ===${RESET}"
echo ""
echo "$HEADER"
echo "$SEP"
for row in "${RESULTS[@]}"; do
    # Colorize status in terminal
    if [[ "$row" == *"FAIL"* ]]; then
        echo -e "${RED}${row}${RESET}"
    elif [[ "$row" == *"ERR"* ]]; then
        echo -e "${YELLOW}${row}${RESET}"
    else
        echo "$row"
    fi
done
echo ""

# Save plain text (no ANSI codes)
{
    echo "Sonido Effect Measurements — $(date '+%Y-%m-%d %H:%M:%S')"
    echo "All effects at default parameters, 440 Hz sine input at 0.8 amplitude"
    echo ""
    echo "$HEADER"
    echo "$SEP"
    for row in "${RESULTS[@]}"; do
        echo "$row"
    done
    echo ""
    if [[ $FAIL_COUNT -eq 0 ]]; then
        echo "Result: ALL PASS"
    else
        echo "Result: $FAIL_COUNT FAIL(s)"
    fi
} > "$OUT/measurements.txt"

echo "Results saved to demos/measurements.txt"
echo ""

# File listing
NFILES=$(find "$OUT" -name '*.wav' | wc -l)
echo "$NFILES WAV files in demos/"
echo "Listen to: *_chord.wav and *_sweep.wav for each effect"

if [[ $FAIL_COUNT -gt 0 ]]; then
    echo -e "${RED}${BOLD}$FAIL_COUNT effect(s) exceeded -1 dBFS peak${RESET}"
    exit 1
else
    echo -e "${GREEN}${BOLD}All effects passed${RESET}"
    exit 0
fi
