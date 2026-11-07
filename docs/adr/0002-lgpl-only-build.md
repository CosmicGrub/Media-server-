# ADR-0002 — Build the entire native AV stack LGPL-only, dynamically linked

**Status:** Proposed (pending spike S4 and legal review S8) — **with a disclosed, temporary
exception for the Windows desktop bundle and a narrower, separate concern on the Linux desktop
bundle; see "Current exceptions" below.**
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

🔴 **Scope of this gate, as it exists today:** it scans `native/`'s own recipes and, when `FFMPEG_BIN` is supplied,
an FFmpeg binary built from them. It is wired up in CI (`rust.yml`'s `licence-gate` job) against exactly that —
`native/` and a caller-supplied FFmpeg. It does **not** run against, and cannot see, mpv/FFmpeg binaries fetched
or vendored at package/release time, which is what the Windows and Linux desktop bundles actually ship today. See
"Current exceptions" below.

## Current exceptions: Windows and Linux desktop bundles (as of this writing)

**This ADR's dynamic-linking, LGPL-only decision is not yet what the Windows and Linux desktop release bundles
ship**, and the two platforms diverge from it in different ways. `native/` currently holds only the LGPL-only
build *recipes* (`ffmpeg.config`, `mpv.config`, [`../../native/README.md`](../../native/README.md)) — spike S4
has not yet produced built artifacts from them. Pending that:

- **Windows — GPL, statically linked.** `crates/lumen-play/package-windows.sh --with-mpv` and `release.yml`'s
  "Bundle mpv (Windows)" step download a **pre-built, third-party mpv** from
  [`shinchiro/mpv-winbuild-cmake`](https://github.com/shinchiro/mpv-winbuild-cmake)'s releases (via SourceForge,
  with a GitHub-releases fallback) and bundle it as-is. That upstream build enables `--enable-gpl
  --enable-libx264 --enable-libx265` and statically links its FFmpeg — i.e. it is **GPLv2+, statically linked**,
  the precise combination this ADR forbids. [`../../DEVICE.md`](../../DEVICE.md) documents the resulting binary
  as "mpv.exe (statically linked FFmpeg and libplacebo)", and `release.yml` says so again at the Windows
  codec-coverage step.
- **Linux — dynamically linked, but from the distro's own (likely GPL) package, not from `native/`.**
  `crates/lumen-play/package-linux.sh --with-mpv` and `release.yml`'s "Bundle mpv (Linux)" step install mpv from
  Ubuntu's own `apt` repository and run `vendor-mpv-linux.sh`, which copies that system mpv and its
  codec/format/subtitle shared libraries beside the binary and patches `RPATH` — it does **not** statically link
  anything (`vendor-mpv-linux.sh`'s and `package-linux.sh`'s own headers say so explicitly: "nobody ships a
  static mpv for Linux") and it does **not** fetch anything from `shinchiro/mpv-winbuild-cmake` or SourceForge.
  So the Windows finding above — static linking, third-party SourceForge/GitHub provenance — does not apply to
  Linux. The linking *is* dynamic, which is what §1.2 of [`../08-legal-licensing.md`](../08-legal-licensing.md)
  asks for. The separate, narrower concern is license, not linkage: Ubuntu's distro FFmpeg (which mpv links
  against) is commonly built with `--enable-gpl`/`--enable-libx264`/`--enable-libx265` in current releases, which
  would make the vendored `libx264`/`libx265`/`libavcodec` shared libraries GPL regardless of dynamic linking —
  this has not been verified against the actual runner image and is flagged here rather than asserted.

Both are recorded as **conscious, disclosed exceptions**, not a silent gap:

- They apply only to the **Windows and Linux desktop bundles**. Neither changes the LGPL-only decision above,
  which remains the target architecture and already governs `native/`'s recipes and `cargo-deny`'s dependency
  policy.
- While the Windows exception stands, the Windows desktop release is **GPL-tainted as distributed**: it ships
  under GPLv2+, not LGPL. Treat any claim of an LGPL-only guarantee for that artifact as false until this
  exception is closed. The Linux bundle's status depends on the unverified distro-package question above — 🔴
  confirm whether the Ubuntu runner's `mpv`/`ffmpeg` packages enable `--enable-gpl` before treating Linux as
  clean.
- `ci/license-gate.sh` does not inspect either the downloaded Windows `mpv.exe` or the vendored Linux
  `mpv`/`lib/*.so*` — see "Enforcement" above. Nothing in CI currently blocks or even flags either bundle on
  license grounds.
- **Exit condition:** these exceptions close when spike S4 ships a built `native/` LGPL-only mpv/FFmpeg for
  Windows and Linux and the bundling steps switch to it, *or* when the team makes the "ship as GPL" decision in
  [`../08-legal-licensing.md`](../08-legal-licensing.md) §1.3 permanent and this ADR is revised accordingly
  instead of carrying an exception.
- Neither exception extends to App Store platforms (iOS/tvOS/macOS): App Store distribution of a GPL binary is
  independently blocked by store terms, so no GPL mpv may ship there regardless of these exceptions.

## References
- [FFmpeg License and Legal Considerations](https://www.ffmpeg.org/legal.html)
- [FFmpeg commercial license guide](https://32blog.com/en/ffmpeg/ffmpeg-commercial-license-guide)
- [mpv — licensing history](https://en.wikipedia.org/wiki/Mpv_(media_player))
- [The GPL and the iOS App Store](https://michelf.ca/blog/2011/gpl-ios-app-store/)
- [FSF — VLC and App Store DRM enforcement](https://www.fsf.org/blogs/licensing/vlc-enforcement)
- [LGPL and app stores — LWN](https://lwn.net/Articles/526355/)
