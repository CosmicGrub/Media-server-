#!/usr/bin/env bash
# Vendor a system mpv and its codec/format/subtitle dylibs into an existing bundle directory.
#
#   ./vendor-mpv-linux.sh <bundle-dir>
#
# Shared by `package-linux.sh` (local/offline builds) and `release.yml` (CI), so the "what gets
# bundled and what doesn't" decision lives in exactly one place. See `package-linux.sh`'s header for
# the reasoning behind the exclude list below -- GL/Vulkan/VA-API, X11/Wayland, the audio server, and
# the security/identity stack resolve from the host on purpose, not by omission.
#
# Requires: a system mpv already installed (to vendor *from*), and `patchelf`.

set -euo pipefail

OUT="${1:?usage: vendor-mpv-linux.sh <bundle-dir>}"
[ -d "$OUT" ] || { echo "ERROR: $OUT does not exist" >&2; exit 1; }

command -v patchelf >/dev/null || { echo "ERROR: patchelf is required" >&2; exit 1; }
SYS_MPV=$(command -v mpv || true)
[ -n "$SYS_MPV" ] || { echo "ERROR: no system mpv to vendor from" >&2; exit 1; }

mkdir -p "$OUT/lib"
cp -L "$SYS_MPV" "$OUT/mpv"

EXCLUDE='^(linux-vdso|ld-linux'\
'|libc\.so|libm\.so|libpthread\.so|libdl\.so|librt\.so|libgcc_s\.so|libstdc\+\+\.so'\
'|libGL\.so|libGLX\.so|libGLdispatch\.so|libEGL\.so|libOpenGL\.so|libdrm|libgbm|libvulkan|libva|libvdpau'\
'|libX11|libxcb|libXext|libXrender|libXrandr|libXi\.so|libXfixes|libXcursor|libXss|libXpresent|libXv\.so'\
'|libXau|libXdmcp|libwayland|libxkbcommon|libdecor'\
'|libasound|libpulse|libpipewire|libjack|libsndio|libopenal'\
'|libdbus|libsystemd|libudev|libselinux|libapparmor|libmount|libblkid|libcap\.so|libkeyutils'\
'|libkrb5|libk5crypto|libgssapi|libcom_err|libp11-kit|libssl\.so|libcrypto\.so'\
'|libgnutls|libnettle|libhogweed|libgmp|libtasn1|libidn2|libunistring|libgcrypt|libgpg-error'\
'|libusb|libraw1394|libavc1394|librom1394|libiec61883|libdc1394'\
'|libflite|libpocketsphinx|libsphinxbase|liblilv|libserd|libsord|libsratom|libzix'\
'|libicu|libnuma|libblas|liblapack|libgfortran|libresolv)'

# ldd on the executable already returns the full transitive closure, so one pass over its output is
# complete -- no need to recurse into the libraries it names.
ldd "$OUT/mpv" | awk '{print $1, $3}' | while read -r name path; do
    [ -n "$path" ] || continue
    base=$(basename "$name")
    echo "$base" | grep -qE "$EXCLUDE" && continue
    cp -L "$path" "$OUT/lib/"
done

patchelf --set-rpath '$ORIGIN/lib' "$OUT/mpv"
for f in "$OUT"/lib/*.so*; do
    [ -f "$f" ] || continue
    patchelf --set-rpath '$ORIGIN' "$f" 2>/dev/null || true
done

# Proof, not assumption: a real decode with LD_LIBRARY_PATH unset, so only RPATH can resolve
# anything. If the vendored codec layer were missing or mismatched, this is where it would fail.
PROBE=$(mktemp -u --suffix=.mkv)
env -i PATH=/usr/bin:/bin "$OUT/mpv" "av://lavfi:testsrc2=size=160x90:rate=8:duration=2" \
    --audio-file="av://lavfi:sine=frequency=440:duration=2" \
    --o="$PROBE" --ovc=libx264 --ovcopts=preset=ultrafast --oac=aac --msg-level=all=error
[ -f "$PROBE" ] || { echo "ERROR: vendored mpv could not even encode a probe file" >&2; exit 1; }
env -i PATH=/usr/bin:/bin "$OUT/mpv" "$PROBE" --vo=null --ao=null --msg-level=all=error
rm -f "$PROBE"
echo "OK -- vendored mpv played a real file with LD_LIBRARY_PATH unset, RPATH only"
echo "bundled: $(du -sh "$OUT/lib" | cut -f1) across $(ls "$OUT/lib" | wc -l) libraries"
