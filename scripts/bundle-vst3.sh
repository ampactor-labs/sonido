#!/usr/bin/env bash
# Bundle Sonido's CLAP plugins as VST3 shims via free-audio/clap-wrapper.
#
# This is clap-wrapper's documented "dynamic" deployment: the wrapper is a
# VST3 that, at load time, finds and hosts the .clap whose filename stem
# matches the wrapper's own. Because the lookup uses the wrapper's runtime
# filename (os::getBinaryName), ONE C++ build serves every plugin — each
# .vst3 produced here is a renamed copy of the same binary. A VST3 install
# is therefore the matching .clap install plus these shims.
#
# CLAP search locations the wrapper probes (clap-wrapper 0.15.1):
#   Linux:   /usr/lib/clap, ~/.clap
#   macOS:   ~/Library/Audio/Plug-Ins/CLAP, /Library/Audio/Plug-Ins/CLAP
#   Windows: %COMMONPROGRAMFILES%\CLAP, %LOCALAPPDATA%\Programs\Common\CLAP
# plus one level of vendor subdirectories. $CLAP_PATH is honored on Windows
# only — 0.15.1's POSIX branch has an inverted empty-check and ignores it.
#
# Plugin names are derived from the .clap files in --clap-dir (default
# dist-clap/ — run scripts/bundle-clap.sh first), so the .vst3 set mirrors
# the .clap set by construction; there is no second plugin list to drift.
#
# Requires: cmake (>= 3.21), a C++ toolchain, network on first run (the
# wrapper fetches the CLAP and VST3 SDKs at configure time). The wrapper
# checkout and build live under target/clap-wrapper/ and are reused across
# runs — CI caches that directory.
#
# Usage:
#   scripts/bundle-vst3.sh [--target <rust-triple>] [--clap-dir <dir>] [--out <dir>]
#
# --target is the Rust triple the release matrix builds; it only matters on
# macOS, where it selects the wrapper's architecture (x86_64 / arm64).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET=""
CLAP_DIR="$ROOT/dist-clap"
OUT="$ROOT/dist-vst3"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)   TARGET="$2";   shift 2 ;;
    --clap-dir) CLAP_DIR="$2"; shift 2 ;;
    --out)      OUT="$2";      shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v cmake >/dev/null || { echo "!! cmake not found — install cmake to build the VST3 wrapper" >&2; exit 1; }

shopt -s nullglob
CLAPS=("$CLAP_DIR"/*.clap)
shopt -u nullglob
if [[ ${#CLAPS[@]} -eq 0 ]]; then
  echo "!! no .clap files in $CLAP_DIR — run scripts/bundle-clap.sh first" >&2
  exit 1
fi

WRAPPER_TAG="${CLAP_WRAPPER_TAG:-v0.15.1}"
WRAPPER_SRC="$ROOT/target/clap-wrapper/$WRAPPER_TAG"
WRAPPER_BUILD="$WRAPPER_SRC/build"
GENERIC="sonido"

case "$(uname -s)" in
  Darwin) HOST_OS=mac ;;
  Linux)  HOST_OS=linux ;;
  MINGW*|MSYS*|CYGWIN*) HOST_OS=windows ;;
  *) echo "!! unsupported host: $(uname -s)" >&2; exit 1 ;;
esac

if [[ ! -d "$WRAPPER_SRC/.git" ]]; then
  echo ">> Fetching clap-wrapper $WRAPPER_TAG..."
  git clone --depth 1 --branch "$WRAPPER_TAG" \
    https://github.com/free-audio/clap-wrapper.git "$WRAPPER_SRC"
fi

CMAKE_ARGS=(
  -DCMAKE_BUILD_TYPE=Release
  -DCLAP_WRAPPER_DOWNLOAD_DEPENDENCIES=TRUE
  -DCLAP_WRAPPER_OUTPUT_NAME="$GENERIC"
  -DCLAP_WRAPPER_BUNDLE_IDENTIFIER="com.ampactorlabs.$GENERIC.vst3"
)
if [[ "$HOST_OS" == mac && -n "$TARGET" ]]; then
  case "$TARGET" in
    x86_64-apple-darwin)  CMAKE_ARGS+=(-DCMAKE_OSX_ARCHITECTURES=x86_64) ;;
    aarch64-apple-darwin) CMAKE_ARGS+=(-DCMAKE_OSX_ARCHITECTURES=arm64) ;;
  esac
fi

JOBS="$( (command -v nproc >/dev/null && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo 4 )"
echo ">> Building the CLAP-as-VST3 wrapper ($WRAPPER_TAG)..."
cmake -S "$WRAPPER_SRC" -B "$WRAPPER_BUILD" "${CMAKE_ARGS[@]}"
cmake --build "$WRAPPER_BUILD" --config Release --target "${GENERIC}_as_vst3" --parallel "$JOBS"

# The built bundle: a directory on Linux/macOS, a single file on Windows
# (CLAP_WRAPPER_WINDOWS_SINGLE_FILE defaults ON).
SRC_BUNDLE="$(find "$WRAPPER_BUILD" -name "$GENERIC.vst3" | head -1)"
[[ -n "$SRC_BUNDLE" ]] || { echo "!! wrapper build produced no $GENERIC.vst3" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"

# Portable in-place sed (BSD sed on macOS needs the temp-file form).
sed_file() {  # $1 file, then -e args
  local f="$1"; shift
  sed "$@" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
}

rename_bundle() {  # $1 plugin name
  local name="$1" dst="$OUT/$1.vst3"
  if [[ -f "$SRC_BUNDLE" ]]; then
    cp "$SRC_BUNDLE" "$dst"                     # Windows single-file
    return
  fi
  cp -R "$SRC_BUNDLE" "$dst"
  # Rename the inner module so the wrapper's runtime name matches the .clap.
  local module base ext=""
  module="$(find "$dst/Contents" -type f \( -name "$GENERIC.so" -o -name "$GENERIC" -o -name "$GENERIC.vst3" \) | head -1)"
  [[ -n "$module" ]] || { echo "!! no inner module in $dst" >&2; exit 1; }
  # Extension from the BASENAME only — the path always contains ".vst3", and
  # the macOS module ("Contents/MacOS/sonido") has no extension of its own.
  base="$(basename "$module")"
  [[ "$base" == *.* ]] && ext=".${base##*.}"
  mv "$module" "$(dirname "$module")/$name$ext"
  if [[ "$HOST_OS" == mac && -f "$dst/Contents/Info.plist" ]]; then
    sed_file "$dst/Contents/Info.plist" \
      -e "s|<string>$GENERIC</string>|<string>$name</string>|g" \
      -e "s|com\.ampactorlabs\.$GENERIC\.vst3|com.ampactorlabs.$name.vst3|g"
  fi
}

count=0
for clap in "${CLAPS[@]}"; do
  name="$(basename "$clap" .clap)"
  rename_bundle "$name"
  count=$((count + 1))
done

# A shim whose .clap is missing loads as an empty factory — the host shows
# nothing, silently. Ship the pairing rule next to the shims.
cat > "$OUT/README.txt" <<'NOTE'
These VST3s are clap-wrapper shims: each one loads the Sonido .clap of the
same name at run time. Install BOTH the .clap files (from the clap/ folder)
and these .vst3 bundles, or the VST3s will not appear in your host.

CLAP install locations the shims search:
  Linux:   /usr/lib/clap  or  ~/.clap
  macOS:   /Library/Audio/Plug-Ins/CLAP  or  ~/Library/Audio/Plug-Ins/CLAP
  Windows: C:\Program Files\Common Files\CLAP  or
           %LOCALAPPDATA%\Programs\Common\CLAP

VST3 install locations (standard):
  Linux:   ~/.vst3
  macOS:   /Library/Audio/Plug-Ins/VST3  or  ~/Library/Audio/Plug-Ins/VST3
  Windows: C:\Program Files\Common Files\VST3
NOTE

echo ">> Packaged $count VST3 shims into $OUT"
ls -1 "$OUT"
if [[ "$count" -ne "${#CLAPS[@]}" ]]; then
  echo "!! incomplete bundle: $count of ${#CLAPS[@]} shims packaged" >&2
  exit 1
fi
