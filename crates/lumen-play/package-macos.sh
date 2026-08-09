#!/usr/bin/env bash
# Build the macOS release bundle.
#
#   ./crates/lumen-play/package-macos.sh            just lumen
#   ./crates/lumen-play/package-macos.sh --with-mpv also vendor mpv and its Homebrew dylibs
#
# The macOS analogue of `package-linux.sh` -- same reasoning, different tools. `otool -L` on a
# Homebrew mpv reports two kinds of dependency: **system frameworks**
# (`/usr/lib/...`, `/System/Library/Frameworks/...` -- libSystem, CoreFoundation, AppKit, Metal,
# OpenGL) which are part of the OS on every Mac that can run this build and are never vendored, and
# **Homebrew dylibs** (`/opt/homebrew/...` or `/usr/local/...` -- libavcodec, libx264, libass, and
# the rest of the codec/format/subtitle layer) which exist only because Homebrew put them there and
# are exactly what a machine without `brew install mpv` is missing. This vendors the second kind and
# leaves the first alone, for the same reason `package-linux.sh` leaves GL/Vulkan/audio-server/
# security libraries to the host: those categories have to track the OS, not a build-time snapshot.
#
# Needs a Mac (or macOS CI runner) with `brew install mpv` already done, to vendor *from*.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCH="$(uname -m)"
case "$ARCH" in
    arm64) NAME="macos-aarch64" ;;
    x86_64) NAME="macos-x86_64" ;;
    *) echo "ERROR: unrecognized arch $ARCH" >&2; exit 1 ;;
esac
OUT="$ROOT/dist/lumen-$NAME"
WITH_MPV=0
[ "${1:-}" = "--with-mpv" ] && WITH_MPV=1

say() { printf '\033[36m%s\033[0m\n' "$*"; }

say "== building =="
cargo build --release -p lumen-play --manifest-path "$ROOT/Cargo.toml"

BIN="$ROOT/target/release/lumen"
[ -f "$BIN" ] || { echo "build produced no binary at $BIN" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$BIN" "$OUT/"
cp "$ROOT/crates/lumen-play/README.md" "$OUT/README.md"
chmod +x "$OUT/lumen"

if [ "$WITH_MPV" = 1 ]; then
    say "== vendoring mpv =="
    "$ROOT/crates/lumen-play/vendor-mpv-macos.sh" "$OUT"
fi

cat > "$OUT/START-HERE.txt" <<'TXT'
lumen - media library player and test harness

./lumen doctor
./lumen scan  ~/Media
./lumen test  ~/Media --limit 5 --json report.json
./lumen play  ~/Media

If this bundle was built with --with-mpv, mpv is vendored beside the binary -- nothing to
install. It still uses this Mac's own GPU, display and audio, the same as any other app;
only the codec/format/subtitle layer (Homebrew's contribution) is bundled.

If not, mpv is the one prerequisite:
    brew install mpv

If mpv is somewhere unusual, point at it instead of moving it:
    export LUMEN_MPV=/path/to/mpv

Playback controls are mpv's own: space, arrows, f, q.
Exit codes: 0 all played, 1 something failed, 2 setup problem.
TXT

say "== packaging =="
cd "$ROOT/dist"
TAR="lumen-$NAME.tar.gz"
rm -f "$TAR"
tar czf "$TAR" "$(basename "$OUT")"

echo
say "== done =="
ls -la "$OUT"
echo
echo "  bundle: $ROOT/dist/$TAR"
