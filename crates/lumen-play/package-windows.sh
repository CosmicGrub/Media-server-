#!/usr/bin/env bash
# Build the Windows release bundle.
#
#   ./crates/lumen-play/package-windows.sh            just lumen.exe
#   ./crates/lumen-play/package-windows.sh --with-mpv also fetch mpv.exe into the bundle
#
# Cross-compiles from Linux with the MinGW toolchain, so no Windows machine is needed:
#
#   apt-get install -y gcc-mingw-w64-x86-64 mingw-w64-x86-64-dev
#   rustup target add x86_64-pc-windows-gnu
#
# The resulting lumen.exe imports only Windows system DLLs — no MinGW runtime to ship alongside.
# `--with-mpv` needs 7z or bsdtar to unpack the upstream archive; without it the bundle carries
# lumen.exe alone and `lumen setup` fetches mpv on the target machine instead.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET=x86_64-pc-windows-gnu
OUT="$ROOT/dist/lumen-windows-x86_64"
WITH_MPV=0
[ "${1:-}" = "--with-mpv" ] && WITH_MPV=1

say() { printf '\033[36m%s\033[0m\n' "$*"; }

say "== building =="
cargo build --release -p lumen-play --target "$TARGET" --manifest-path "$ROOT/Cargo.toml"

EXE="$ROOT/target/$TARGET/release/lumen.exe"
[ -f "$EXE" ] || { echo "build produced no exe at $EXE" >&2; exit 1; }

# The check that matters: a binary importing libgcc_s_seh-1.dll or libwinpthread-1.dll would need
# those shipped beside it, and would fail on the user's machine with a dialog rather than a message.
say "== verifying it is self-contained =="
if command -v x86_64-w64-mingw32-objdump >/dev/null; then
    IMPORTS=$(x86_64-w64-mingw32-objdump -p "$EXE" | sed -n 's/.*DLL Name: //p')
    echo "$IMPORTS" | sed 's/^/  /'
    if echo "$IMPORTS" | grep -qiE 'libgcc|libwinpthread|libstdc'; then
        echo "ERROR: links a MinGW runtime DLL; the bundle would need it shipped too" >&2
        exit 1
    fi
    echo "  OK — Windows system DLLs only"
fi

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$EXE" "$OUT/"
cp "$ROOT/crates/lumen-play/README.md" "$OUT/README.md"

cat > "$OUT/START-HERE.txt" <<'TXT'
lumen — media library player and test harness
=============================================

1. Open a terminal in this folder (Shift+right-click > "Open PowerShell window here").

2. Check the machine:

       .\lumen.exe doctor

   If it says mpv was not found, run:

       .\lumen.exe setup

   That downloads mpv.exe into this folder. Nothing is installed system-wide and
   no registry keys are written — deleting this folder undoes everything.

3. Look at your library without playing anything:

       .\lumen.exe scan  "D:\Media"

4. Test it — opens every file for 20 seconds and reports which ones fail:

       .\lumen.exe test  "D:\Media" --limit 5
       .\lumen.exe test  "D:\Media" --json report.json

   Start with --limit 5. Once that looks right, drop the limit.

5. Watch:

       .\lumen.exe play  "D:\Media"

   Playback controls are mpv's own: space, arrows, f for fullscreen, q to quit.

Exit codes: 0 everything played, 1 at least one file failed, 2 setup problem.

If mpv is somewhere else already, point at it instead of moving anything:

       set LUMEN_MPV=D:\path\to\mpv.exe
TXT

if [ "$WITH_MPV" = 1 ]; then
    say "== fetching mpv =="
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT

    # SourceForge, because that is what mpv.io itself links to for Windows builds. The version is
    # discovered rather than pinned, so the bundle does not quietly go stale — but a pinned
    # MPV_VERSION wins, which is what a reproducible release build wants.
    VER="${MPV_VERSION:-}"
    if [ -z "$VER" ]; then
        VER=$(curl -sSL "https://sourceforge.net/projects/mpv-player-windows/files/release/" \
            | grep -oE "mpv-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.7z" \
            | sed -E 's/mpv-(.*)-x86_64\.7z/\1/' | sort -V | tail -1)
    fi
    if [ -z "$VER" ]; then
        echo "  could not discover an mpv version; shipping without it" >&2
        echo "  (`lumen setup` will fetch it on the target machine instead)" >&2
    else
        URL="https://sourceforge.net/projects/mpv-player-windows/files/release/mpv-$VER-x86_64.7z/download"
        echo "  mpv $VER"
        curl -sSL -o "$TMP/mpv.7z" "$URL"

        # A truncated download extracts to nothing and would silently ship a bundle with no player.
        if ! file "$TMP/mpv.7z" | grep -q "7-zip"; then
            echo "ERROR: the download is not a 7-zip archive; refusing to ship a broken bundle" >&2
            exit 1
        fi

        mkdir -p "$TMP/x"
        if command -v 7z >/dev/null; then 7z x "$TMP/mpv.7z" -o"$TMP/x" -y >/dev/null
        elif command -v bsdtar >/dev/null; then bsdtar -xf "$TMP/mpv.7z" -C "$TMP/x"
        else echo "ERROR: need 7z or bsdtar to unpack" >&2; exit 1; fi

        FOUND=$(find "$TMP/x" -name mpv.exe | head -1)
        [ -n "$FOUND" ] || { echo "ERROR: no mpv.exe in the archive" >&2; exit 1; }
        cp "$FOUND" "$OUT/"
        # Shader compilation on older D3D paths wants this; it is in the upstream archive for a
        # reason and leaving it out breaks exactly the machines least able to diagnose it.
        D3D=$(find "$TMP/x" -name "d3dcompiler_*.dll" | head -1)
        [ -n "$D3D" ] && cp "$D3D" "$OUT/"
        echo "  bundled mpv.exe ($(du -h "$OUT/mpv.exe" | cut -f1))"
    fi
fi

say "== packaging =="
cd "$ROOT/dist"
ZIP="lumen-windows-x86_64.zip"
rm -f "$ZIP"
if command -v zip >/dev/null; then
    zip -qr "$ZIP" "$(basename "$OUT")"
else
    python3 -c "
import shutil,sys
shutil.make_archive('lumen-windows-x86_64','zip','.', '$(basename "$OUT")')"
fi

echo
say "== done =="
ls -la "$OUT"
echo
echo "  bundle: $ROOT/dist/$ZIP"
