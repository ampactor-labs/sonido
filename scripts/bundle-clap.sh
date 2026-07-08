#!/usr/bin/env bash
# Bundle Sonido's CLAP plugins into loadable `.clap` artifacts.
#
# Each plugin is a `[[example]]` cdylib in `crates/sonido-plugin`. This script
# builds them in release mode and packages the resulting shared libraries into
# `.clap` files (Linux/Windows: a renamed shared lib; macOS: a proper bundle).
#
# Usage:
#   scripts/bundle-clap.sh [--target <triple>] [--out <dir>]
#
# Examples:
#   scripts/bundle-clap.sh                      # host build -> dist-clap/
#   scripts/bundle-clap.sh --out /tmp/clap      # custom output dir
#   scripts/bundle-clap.sh --target aarch64-apple-darwin
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET=""
OUT="$ROOT/dist-clap"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --out)    OUT="$2";    shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# The plugin examples (single-effect plugins + the graph player).
PLUGINS=(
  sonido-graph-player sonido-preamp sonido-distortion sonido-compressor
  sonido-gate sonido-eq sonido-wah sonido-chorus sonido-flanger
  sonido-phaser sonido-tremolo sonido-delay sonido-filter sonido-vibrato
  sonido-tape sonido-reverb sonido-harmonic-habitat sonido-limiter
  sonido-bitcrusher sonido-ringmod sonido-stage
)

echo ">> Building ${#PLUGINS[@]} CLAP plugins (release)..."
if [[ -n "$TARGET" ]]; then
  cargo build --release --target "$TARGET" -p sonido-plugin --examples
  BUILD_DIR="$ROOT/target/$TARGET/release/examples"
else
  cargo build --release -p sonido-plugin --examples
  BUILD_DIR="$ROOT/target/release/examples"
fi

rm -rf "$OUT"
mkdir -p "$OUT"

# Locate the built cdylib for a plugin (cargo turns `-` into `_` in filenames).
find_lib() {
  local under="${1//-/_}"
  for cand in "lib${under}.so" "lib${under}.dylib" "${under}.dll"; do
    if [[ -f "$BUILD_DIR/$cand" ]]; then echo "$BUILD_DIR/$cand"; return 0; fi
  done
  return 1
}

bundle_macos() {  # $1 plugin name, $2 lib path
  local name="$1" lib="$2"
  local app="$OUT/$name.clap"
  mkdir -p "$app/Contents/MacOS"
  cp "$lib" "$app/Contents/MacOS/$name"
  printf 'BNDL????' > "$app/Contents/PkgInfo"
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>$name</string>
  <key>CFBundleIdentifier</key><string>com.ampactorlabs.${name//-/.}</string>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
</dict>
</plist>
PLIST
}

count=0
for p in "${PLUGINS[@]}"; do
  if lib="$(find_lib "$p")"; then
    case "$lib" in
      *.dylib) bundle_macos "$p" "$lib" ;;          # macOS: bundle dir
      *)       cp "$lib" "$OUT/$p.clap" ;;           # Linux/Windows: flat .clap
    esac
    count=$((count + 1))
  else
    echo "!! missing build output for $p (looked in $BUILD_DIR)" >&2
  fi
done

echo ">> Packaged $count/${#PLUGINS[@]} plugins into $OUT"
ls -1 "$OUT"
