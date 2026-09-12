#!/usr/bin/env bash
# Build the Linux release bundle.
#
#   ./crates/lumen-play/package-linux.sh            just lumen
#   ./crates/lumen-play/package-linux.sh --with-mpv also vendor mpv and its shared libraries
#
# `--with-mpv` needs a system mpv already installed (`apt install mpv` or equivalent) to vendor
# *from*, plus `patchelf`. It does not statically link mpv -- nobody ships a static mpv for Linux,
# because mpv's GPU rendering, windowing and audio have to be the same libraries the desktop
# actually has, not a foreign copy frozen at build time. What this script does instead is copy mpv
# and the codec/format/subtitle layer beside it, then patch RPATH so that layer is found next to the
# binary regardless of what is or is not installed system-wide -- while deliberately leaving
# GL/Vulkan/VA-API, X11/Wayland, the audio server, and the security/identity stack (D-Bus, systemd,
# PAM-adjacent, Kerberos, OpenSSL, glibc itself) to resolve from the host, because those categories
# are exactly the ones that must match the machine's own driver, compositor, audio routing and
# certificate store -- vendoring them doesn't remove a dependency, it ships a second, wrong copy
# that a driver update or a security patch never reaches.
#
# The result: no `apt install mpv` needed to run the bundle, but hardware decode, the display
# server and system audio still come from wherever they already do.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/dist/lumen-linux-x86_64"
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
    command -v patchelf >/dev/null || { echo "ERROR: patchelf is required for --with-mpv" >&2; exit 1; }
    "$ROOT/crates/lumen-play/vendor-mpv-linux.sh" "$OUT"
fi

cat > "$OUT/START-HERE.txt" <<'TXT'
lumen - media library player and test harness

./lumen doctor
./lumen scan  ~/Media
./lumen test  ~/Media --limit 5 --json report.json
./lumen play  ~/Media

If this bundle was built with --with-mpv, mpv is vendored beside the binary -- nothing to
install. It still uses your system's own GPU driver, display server and audio, the same as
any other app on this machine; only the codec/format/subtitle layer is bundled.

If not, mpv is the one prerequisite:
    apt install mpv     (or dnf / pacman / zypper)

If mpv is somewhere unusual, point at it instead of moving it:
    export LUMEN_MPV=/path/to/mpv

Playback controls are mpv's own: space, arrows, f, q.
Exit codes: 0 all played, 1 something failed, 2 setup problem.
TXT

say "== packaging =="
cd "$ROOT/dist"
TAR="lumen-linux-x86_64.tar.gz"
rm -f "$TAR"
tar czf "$TAR" "$(basename "$OUT")"

echo
say "== done =="
ls -la "$OUT"
echo
echo "  bundle: $ROOT/dist/$TAR"
