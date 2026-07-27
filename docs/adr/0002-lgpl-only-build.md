# ADR-0002 — Build the entire native AV stack LGPL-only, dynamically linked

**Status:** Proposed (pending spike S4 and legal review S8)
**Date:** 2026-07-27

## Context

FFmpeg is dual-licensed: **LGPL v2.1+** for most of the core (`libavformat`, `libavcodec`, `libavutil`), with parts
available only under **GPL v2+**. Building with `--enable-gpl` (required for `libx264`, `libx265`, and certain
filters) makes the entire FFmpeg build GPL, which makes the whole combined work GPL. Building with `--enable-nonfree`
(e.g. `libfdk-aac`) makes the result **undistributable**. FFmpeg is not available under any other terms, at any price.

mpv is historically GPLv2+, with an LGPL relicensing effort producing an LGPLv2.1+ build option (`-Dgpl=false`).
libplacebo is LGPLv2.1+, libass is ISC, libbluray is LGPLv2.1+. libdvdnav/libdvdread are GPLv2+.

Meanwhile, GPL and the Apple App Store are genuinely incompatible: App Store terms impose non-transferable licenses
and FairPlay DRM, which conflict with the GPL's redistribution and modification freedoms. This is why GPL-licensed
VLC was pulled from the App Store in 2011 and why VideoLAN pursued LGPL relicensing.

## Decision

1. **Build FFmpeg with `--disable-gpl --disable-nonfree`** (and without `--enable-version3`).
2. **Build mpv with `-Dgpl=false`** (LGPLv2.1+).
3. **Link all LGPL components dynamically** (shared libraries on desktop/Android, frameworks inside the app bundle
   on Apple).
4. **Use hardware encoders (NVENC, QSV, VAAPI, AMF, VideoToolbox) plus permissively-licensed software encoders
   (`libaom`, `SVT-AV1`, `libvpx`) for server-side transcoding. Do not ship x264/x265.**
5. **License our own code MPL-2.0** (core and shells) and **Apache-2.0** (plugin SDK and WIT interfaces).
6. **Enforce all of the above with a CI gate that fails the build** on any GPL/nonfree flag or license string.
7. **Meet LGPL obligations concretely**: publish complete corresponding source for LGPL components, publish object
   files and a working build script enabling relinking, generate an SBOM per release, and auto-generate the in-app
   Legal screen from it.

## Rationale

- It is the **only** path that keeps App Store distribution possible at all, and the only path that keeps
  non-GPL licensing of our own code available.
- The cost is small and specific: no x264/x265 software encoding, no libdvdnav DVD menus, and a handful of GPL-only
  mpv filters and video outputs. **Server transcoding wants hardware encoders anyway** — a hardware-first transcoder
  sidesteps the GPL encoder problem entirely rather than trading it off.
- Retrofitting this later is a rewrite. Adopting it on day one is a build-flag decision and a CI script.

## Consequences

**Positive**
- iOS/tvOS/macOS App Store distribution stays on the table.
- Future licensing flexibility (dual-licensing, commercial support, an open-core model) stays on the table.
- Clean, defensible compliance story; SBOM and attribution are automated rather than archaeological.

**Negative**
- No software x264/x265 encoding. Servers without a supported GPU fall back to `libaom`/`SVT-AV1`/`libvpx` or to
  H.264 via `libavcodec`'s native (LGPL) `mpeg4`/`h263` — meaningfully worse for CPU-only H.264 output. 🔴 Validate
  in spike S4 whether this is acceptable; if not, offer x264 as a **separately-distributed, user-installed, GPL**
  component invoked over a process boundary.
- DVD menu navigation (libdvdnav) is GPL-only; either drop menus (play the main title) or isolate it as an optional
  separately-licensed component.
- Dynamic linking adds packaging complexity on Apple platforms and increases bundle size slightly.
- Ongoing discipline: every new native dependency needs a license check. That's what `cargo-deny` and the gate are for.

**Note this ADR does not resolve patents.** Codec patent licensing (H.264, HEVC, AAC, Dolby, DTS) is independent of
copyright licensing and is addressed separately in [`../08-legal-licensing.md`](../08-legal-licensing.md) §2.

## Enforcement

```bash
# ci/license-gate.sh — blocking on every native build
set -euo pipefail
BAD='--enable-gpl|--enable-nonfree|--enable-version3|--enable-libx264|--enable-libx265|--enable-libfdk-aac'
grep -REn "$BAD" native/ && exit 1
"$FFMPEG_BIN" -version | grep -Eq 'enable-gpl|enable-nonfree' && exit 1
"$FFMPEG_BIN" -version | grep -q 'libavutil license: LGPL' || exit 1
cargo deny check licenses
```

## References
- [FFmpeg License and Legal Considerations](https://www.ffmpeg.org/legal.html)
- [FFmpeg commercial license guide](https://32blog.com/en/ffmpeg/ffmpeg-commercial-license-guide)
- [mpv — licensing history](https://en.wikipedia.org/wiki/Mpv_(media_player))
- [The GPL and the iOS App Store](https://michelf.ca/blog/2011/gpl-ios-app-store/)
- [FSF — VLC and App Store DRM enforcement](https://www.fsf.org/blogs/licensing/vlc-enforcement)
- [LGPL and app stores — LWN](https://lwn.net/Articles/526355/)
