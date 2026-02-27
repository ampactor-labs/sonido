#!/usr/bin/env bash
# generate_demos.sh -- Generate demo WAV files showcasing Sonido's DSP capabilities.
#
# Usage: ./scripts/generate_demos.sh
#
# Requires: cargo build -p sonido-cli --release (or set SONIDO env var)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SONIDO="${SONIDO:-$PROJECT_DIR/target/release/sonido}"
DEMOS="$PROJECT_DIR/demos"

# Build if needed
if [ ! -f "$SONIDO" ]; then
    echo "Building sonido-cli (release)..."
    cargo build -p sonido-cli --release --manifest-path "$PROJECT_DIR/Cargo.toml"
fi

mkdir -p "$DEMOS"

echo "=== Generating source signals ==="

# 1. Sine tone at 440 Hz (2 seconds)
echo "  [1/5] Sine tone 440 Hz..."
"$SONIDO" generate tone "$DEMOS/src_sine_440.wav" \
    --freq 440.0 --duration 2.0 --amplitude 0.8

# 2. Saw chord pad (C major: MIDI 60,64,67)
echo "  [2/5] Saw chord pad (C major)..."
"$SONIDO" generate chord "$DEMOS/src_saw_chord.wav" \
    --notes "60,64,67" --waveform saw --duration 3.0 --amplitude 0.5 \
    --filter-cutoff 3000.0 --attack 50.0 --release 800.0

# 3. Short percussion ADSR envelope
echo "  [3/5] Percussion ADSR..."
"$SONIDO" generate adsr "$DEMOS/src_perc_adsr.wav" \
    --attack 5.0 --decay 80.0 --sustain 0.1 --release 150.0 \
    --freq 220.0 --gate-duration 0.3 --amplitude 0.9

# 4. Frequency sweep 20-20kHz
echo "  [4/5] Frequency sweep..."
"$SONIDO" generate sweep "$DEMOS/src_sweep.wav" \
    --start 20.0 --end 20000.0 --duration 3.0 --amplitude 0.7

# 5. White noise
echo "  [5/5] White noise..."
"$SONIDO" generate noise "$DEMOS/src_noise.wav" \
    --duration 2.0 --amplitude 0.5

echo ""
echo "=== Processing through effects ==="

# Demo table: stem source effect params_csv
DEMOS_TABLE=(
    "distortion_soft    src_sine_440   distortion     drive=12,waveshape=softclip,tone=4000,level=-6"
    "distortion_hard    src_sine_440   distortion     drive=20,waveshape=hardclip,tone=3000,level=-8"
    "compressor         src_perc_adsr  compressor     threshold=-18,ratio=4,attack=10,release=100,makeup=6"
    "gate               src_sine_440   gate           threshold=-20,attack=1,release=50,hold=20"
    "chorus             src_saw_chord  chorus         rate=1.5,depth=0.6,mix=0.5"
    "flanger            src_sine_440   flanger        rate=0.4,depth=0.7,feedback=0.6,mix=0.5"
    "phaser             src_saw_chord  phaser         rate=0.3,depth=0.8,stages=6,feedback=0.7,mix=0.5"
    "tremolo            src_sine_440   tremolo        rate=6,depth=0.8,waveform=sine"
    "multivibrato       src_saw_chord  multivibrato   depth=0.6"
    "filter_lowpass     src_sweep      filter         cutoff=2000,resonance=4"
    "wah                src_saw_chord  wah            frequency=800,resonance=5,sensitivity=0.7,mode=auto"
    "parametric_eq      src_sweep      eq             mid_freq=1000,mid_gain=8,mid_q=2"
    "reverb_room        src_perc_adsr  reverb         type=room,decay=0.4,damping=0.5,mix=0.4"
    "reverb_hall        src_perc_adsr  reverb         type=hall,decay=0.8,damping=0.3,mix=0.5"
    "delay              src_perc_adsr  delay          time=375,feedback=0.5,mix=0.4"
    "tape_saturation    src_saw_chord  tape           drive=8,saturation=0.6"
    "preamp             src_sine_440   preamp         gain=8"
    "limiter            src_saw_chord  limiter        threshold=-12,ceiling=-0.5,release=80,lookahead=5"
    "bitcrusher         src_sine_440   bitcrusher     bit_depth=6,downsample=4,mix=1"
    "ringmod            src_sine_440   ringmod        frequency=220,depth=1,waveform=sine,mix=0.6"
    "stage              src_saw_chord  stage          width=160,haas_delay=12"
)

TOTAL=${#DEMOS_TABLE[@]}
IDX=0

for row in "${DEMOS_TABLE[@]}"; do
    read -r stem source effect params_csv <<< "$row"
    IDX=$((IDX + 1))

    # Build --param flags from comma-separated k=v pairs
    PARAM_FLAGS=""
    IFS=',' read -ra PAIRS <<< "$params_csv"
    for pair in "${PAIRS[@]}"; do
        PARAM_FLAGS="$PARAM_FLAGS --param $pair"
    done

    echo "  [$IDX/$TOTAL] $stem..."
    # shellcheck disable=SC2086
    "$SONIDO" process "$DEMOS/${source}.wav" "$DEMOS/fx_${stem}.wav" \
        --effect "$effect" $PARAM_FLAGS
done

# Full effect chain: preamp -> distortion -> chorus -> delay -> reverb
echo "  [chain] Full effect chain..."
"$SONIDO" process "$DEMOS/src_saw_chord.wav" "$DEMOS/fx_full_chain.wav" \
    --chain "preamp:gain=6|distortion:drive=10,waveshape=softclip,level=-6|chorus:rate=1,depth=0.4,mix=0.3|delay:time=300,feedback=0.3,mix=0.25|reverb:type=hall,decay=0.6,mix=0.3"

echo ""
echo "=== Done ==="
echo "Generated $(ls -1 "$DEMOS"/*.wav 2>/dev/null | wc -l) WAV files in $DEMOS/"
ls -lh "$DEMOS"/*.wav
