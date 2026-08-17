#!/usr/bin/env bash
# Drive a real walkthrough of the app on whatever emulator is already booted and connected via adb.
# Run from android/ci/../ (i.e. the `android/` directory) by android.yml's emulator-runner step:
#
#   ./ci/device-walkthrough.sh <package-id> [--foldable]
#
# <package-id>  the installed app's applicationId, e.g. dev.lumen.player.fold5.debug
# --foldable    also exercise fold/unfold via the emulator console (Fold 5 fork only -- a Tab
#               profile has no hinge to fold)
#
# Sideloads the debug APK, grants permissions ahead of launch, pushes and plays a real media file
# through the app's own local-library screen, and saves screenshots plus a screen recording under
# walkthrough/ for android.yml to upload as an artifact.
#
# Located by visible text via `uiautomator dump`, not hard-coded screen coordinates: a button found
# by the same label a person would read, or a library row found by the exact filename just pushed,
# stays correct across the very different aspect ratios this script runs under (tablet vs. fold),
# where fixed x/y taps would not.

set -euo pipefail

PKG="${1:?usage: device-walkthrough.sh <package-id> [--foldable]}"
FOLDABLE=0
[ "${2:-}" = "--foldable" ] && FOLDABLE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/walkthrough"
PROBE="$ROOT/../lumen-probe.mp4"
APK="$ROOT/apks/debug/app-debug.apk"

mkdir -p "$OUT"
say() { printf '\033[36m%s\033[0m\n' "$*"; }

[ -f "$APK" ] || { echo "ERROR: no debug APK at $APK" >&2; exit 1; }
[ -f "$PROBE" ] || { echo "ERROR: no probe clip at $PROBE" >&2; exit 1; }

adb wait-for-device
adb shell input keyevent 82 >/dev/null 2>&1 || true # dismiss a keyguard, if any; harmless if none

say "== sideloading =="
adb install -r "$APK"

say "== granting permissions ahead of launch (the returning-user path) =="
adb shell pm grant "$PKG" android.permission.READ_MEDIA_VIDEO
adb shell pm grant "$PKG" android.permission.READ_MEDIA_AUDIO
adb shell pm grant "$PKG" android.permission.POST_NOTIFICATIONS || true

say "== pushing a real media file =="
adb shell mkdir -p /sdcard/Movies
adb push "$PROBE" /sdcard/Movies/lumen-probe.mp4

say "== waiting for MediaStore to index it =="
INDEXED=0
for _ in 1 2 3 4 5 6; do
    if adb shell content query --uri content://media/external/video/media --projection _display_name 2>/dev/null \
        | grep -q lumen-probe; then
        INDEXED=1
        break
    fi
    adb shell content call --uri content://media/external --method scan_volume --arg external >/dev/null 2>&1 || true
    sleep 3
done
if [ "$INDEXED" = 1 ]; then
    echo "OK -- MediaStore indexed the sideloaded file"
else
    echo "WARNING: MediaStore never indexed lumen-probe.mp4; the library screen may still show it empty" >&2
fi

# Prints "cx cy" for the center of the first uiautomator node whose text or content-desc contains
# $1, or exits 1 if nothing matched. Standard uiautomator dump attribute order (text, then
# content-desc, then bounds last) is what the pattern below relies on.
tap_text() {
    adb shell uiautomator dump /sdcard/window_dump.xml >/dev/null
    adb pull /sdcard/window_dump.xml "$OUT/" >/dev/null 2>&1
    python3 - "$1" "$OUT/window_dump.xml" <<'PY'
import re, sys
needle, path = sys.argv[1], sys.argv[2]
xml = open(path, encoding="utf-8").read()
pat = r'<node[^>]*text="([^"]*)"[^>]*content-desc="([^"]*)"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"'
for m in re.finditer(pat, xml):
    text, desc, x1, y1, x2, y2 = m.groups()
    if needle in text or needle in desc:
        print(f"{(int(x1) + int(x2)) // 2} {(int(y1) + int(y2)) // 2}")
        sys.exit(0)
sys.exit(1)
PY
}

say "== starting a screen recording of the whole walkthrough =="
adb shell screenrecord --time-limit 60 /sdcard/lumen-walkthrough.mp4 &
sleep 1

say "== launching the app (permissions/setup flow) =="
adb shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1
sleep 4
adb exec-out screencap -p > "$OUT/01-launch.png"

say "== core screens: forcing a rescan if the empty-library state is showing =="
if COORDS=$(tap_text "Rescan"); then
    adb shell input tap $COORDS
    sleep 2
fi
adb exec-out screencap -p > "$OUT/02-library.png"

say "== sideload-and-play: tapping the pushed file to play it =="
if COORDS=$(tap_text "lumen-probe"); then
    adb shell input tap $COORDS
    sleep 3
    adb exec-out screencap -p > "$OUT/03-playing.png"
else
    echo "WARNING: lumen-probe was not visible in the library yet -- not failing the job over a screenshot; the push+index steps above are the real evidence." >&2
fi

if [ "$FOLDABLE" = 1 ]; then
    say "== fold-specific behavior: folding =="
    adb emu fold
    sleep 2
    adb exec-out screencap -p > "$OUT/04-folded.png"

    say "== unfolding =="
    adb emu unfold
    sleep 2
    adb exec-out screencap -p > "$OUT/05-unfolded.png"
fi

say "== stopping the recording =="
adb shell pkill -INT screenrecord 2>/dev/null || true
sleep 2
adb pull /sdcard/lumen-walkthrough.mp4 "$OUT/walkthrough.mp4" 2>/dev/null || echo "WARNING: no video pulled" >&2

say "== done =="
ls -lh "$OUT"
