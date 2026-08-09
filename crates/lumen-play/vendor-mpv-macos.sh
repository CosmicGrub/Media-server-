#!/usr/bin/env bash
# Vendor a system mpv and its Homebrew dylibs into an existing bundle directory.
#
#   ./vendor-mpv-macos.sh <bundle-dir>
#
# Shared by `package-macos.sh` (local/offline builds) and `release.yml` (CI). See
# `package-macos.sh`'s header for the reasoning: only Homebrew-prefixed dylibs (the codec/format/
# subtitle layer) are vendored; system frameworks stay external because they track the OS.
#
# Requires: a system mpv already installed via Homebrew (to vendor *from*). `otool` and
# `install_name_tool` ship with Xcode Command Line Tools, already present on GitHub's macOS runners.

set -euo pipefail

OUT="${1:?usage: vendor-mpv-macos.sh <bundle-dir>}"
[ -d "$OUT" ] || { echo "ERROR: $OUT does not exist" >&2; exit 1; }

SYS_MPV=$(command -v mpv || true)
[ -n "$SYS_MPV" ] || { echo "ERROR: no system mpv to vendor from" >&2; exit 1; }
SYS_MPV=$(python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "$SYS_MPV")

mkdir -p "$OUT/lib"
cp "$SYS_MPV" "$OUT/mpv"
chmod u+w "$OUT/mpv"

vendor_deps() {
    local target="$1"
    otool -L "$target" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        case "$dep" in
            /opt/homebrew/*|/usr/local/*|/opt/local/*) echo "$dep" ;;
        esac
    done
}

# Breadth-first: vendoring a dylib can introduce dependencies of its own, so this expands the queue
# until nothing new turns up rather than assuming mpv's own direct deps are the whole tree.
QUEUE=("$OUT/mpv")
SEEN=()
while [ "${#QUEUE[@]}" -gt 0 ]; do
    current="${QUEUE[0]}"
    QUEUE=("${QUEUE[@]:1}")
    while IFS= read -r dep; do
        [ -n "$dep" ] || continue
        base=$(basename "$dep")
        [[ " ${SEEN[*]} " == *" $base "* ]] && continue
        SEEN+=("$base")
        real=$(python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "$dep")
        [ -f "$real" ] || { echo "  WARNING: $dep not found on disk, skipping" >&2; continue; }
        cp "$real" "$OUT/lib/$base"
        chmod u+w "$OUT/lib/$base"
        QUEUE+=("$OUT/lib/$base")
    done < <(vendor_deps "$current")
done

# `-id` sets what a library calls itself; `-change` rewrites what a specific dependent calls it.
# Both need doing, or the copies exist but nothing points at them.
for lib in "$OUT"/lib/*; do
    [ -f "$lib" ] || continue
    install_name_tool -id "@executable_path/lib/$(basename "$lib")" "$lib"
done
for bin in "$OUT/mpv" "$OUT"/lib/*; do
    [ -f "$bin" ] || continue
    otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
        base=$(basename "$dep")
        [ -f "$OUT/lib/$base" ] || continue
        install_name_tool -change "$dep" "@executable_path/lib/$base" "$bin"
    done
    codesign --force --sign - "$bin" 2>/dev/null || true
done

STILL_HOMEBREW=$(otool -L "$OUT/mpv" "$OUT"/lib/* 2>/dev/null | grep -E '/opt/homebrew|/usr/local|/opt/local' || true)
if [ -n "$STILL_HOMEBREW" ]; then
    echo "ERROR: still references a Homebrew path after rewriting:" >&2
    echo "$STILL_HOMEBREW" >&2
    exit 1
fi

# Proof, not assumption: a real decode with DYLD_LIBRARY_PATH unset, so only the rewritten
# @executable_path/lib references can resolve anything.
PROBE=$(mktemp -u).mkv
env -i PATH=/usr/bin:/bin "$OUT/mpv" "av://lavfi:testsrc2=size=160x90:rate=8:duration=2" \
    --audio-file="av://lavfi:sine=frequency=440:duration=2" \
    --o="$PROBE" --ovc=libx264 --ovcopts=preset=ultrafast --oac=aac --msg-level=all=error
[ -f "$PROBE" ] || { echo "ERROR: vendored mpv could not even encode a probe file" >&2; exit 1; }
env -i PATH=/usr/bin:/bin "$OUT/mpv" "$PROBE" --vo=null --ao=null --msg-level=all=error
rm -f "$PROBE"
echo "OK -- vendored mpv played a real file with DYLD_LIBRARY_PATH unset, install-name rewrite only"
echo "bundled: $(du -sh "$OUT/lib" | cut -f1) across $(ls "$OUT/lib" | wc -l) libraries"
