# Native AV Stack — LGPL-only Build Recipes

Spike **S4** from [`../docs/09-roadmap.md`](../docs/09-roadmap.md) §2: build FFmpeg + libplacebo +
mpv as an **LGPLv2.1+, dynamically linked** stack for all six targets, and measure exactly what that
costs in features.

This directory holds the recipes. It answers research question **R1** — *which mpv/FFmpeg features
are lost in an LGPL-only build?* — which gates ADR-0002, the iOS target, and the whole licensing
posture.

## Why this is the first spike

One `--enable-gpl` makes the entire combined work GPL. That forecloses App Store distribution and
any non-GPL licensing of our own code, **permanently and retroactively**. Getting it right is a
build-flag decision on day one; getting it wrong is a rewrite. See
[`../docs/adr/0002-lgpl-only-build.md`](../docs/adr/0002-lgpl-only-build.md).

## Component licences

| Component | Licence | Notes |
|---|---|---|
| **FFmpeg** | LGPL v2.1+ with `--disable-gpl --disable-nonfree` | Core (`libavcodec`, `libavformat`, `libavutil`) is LGPL. GPL only enters via opt-in flags. |
| **mpv** | LGPL v2.1+ with `-Dgpl=false` | 🔴 **R1 verifies this per release** — the set of LGPL-excluded components has changed over time. |
| **libplacebo** | LGPL v2.1+ | The HDR/tone-mapping pipeline. Clean. |
| **libass** | ISC | Clean. |
| **libbluray** | LGPL v2.1+ | Does **not** decrypt AACS/BD+ — keep it that way ([`../docs/08-legal-licensing.md`](../docs/08-legal-licensing.md) §4). |
| **dav1d** | BSD-2 | Clean. |
| **SVT-AV1 / libaom / libvpx** | BSD | The encoders that replace x264/x265. |
| ~~libx264 / libx265~~ | GPL | **Excluded.** Not needed — see below. |
| ~~libdvdnav / libdvdread~~ | GPL v2+ | **Excluded.** Costs DVD *menu* navigation; main-title playback is unaffected. |
| ~~libfdk-aac~~ | Non-distributable | **Excluded.** |

## The finding that makes this affordable

The usual objection to an LGPL-only build is "but we need x264/x265 to transcode." We do not:

- **Server-side transcoding wants hardware encoders anyway** — NVENC, QSV, VAAPI, AMF, VideoToolbox.
  All are LGPL-compatible and all are what you would actually deploy on a media server.
- **Software fallback is covered by permissive encoders** — SVT-AV1 (BSD) is competitive with x265
  at equal speed; libaom and libvpx are BSD.

So a **hardware-first transcoder sidesteps the GPL encoder problem entirely** rather than trading
against it. Research item **R25** measures whether the CPU-only H.264 path is good enough for the
minority of servers with no usable GPU; if not, ADR-0002's escape hatch is to ship x264 as a
*separately distributed, user-installed* GPL component invoked across a process boundary.

## Layout

```
native/
├─ README.md          ← this file
├─ ffmpeg.config      ← FFmpeg configure flags (the source of truth)
├─ mpv.config         ← mpv meson options
└─ build.sh           ← per-target orchestration
```

## Usage

```bash
# Build the stack for the host platform into native/out/<target>/
native/build.sh linux-x86_64

# All targets a CI runner can reach
native/build.sh linux-x86_64 linux-aarch64 windows-x86_64 android-arm64

# Verify the licence posture of what was produced (blocking in CI)
FFMPEG_BIN=native/out/linux-x86_64/bin/ffmpeg ci/license-gate.sh
```

Targets: `linux-x86_64` `linux-aarch64` `windows-x86_64` `macos-arm64` `macos-x86_64`
`ios-arm64` `tvos-arm64` `android-arm64` `android-armv7` `android-x86_64`.

## Build artifacts are cached, not rebuilt

Rebuilding FFmpeg on every CI run is minutes of wasted time per job. `build.sh` produces versioned,
reproducible artifacts keyed by `(component version, target, config hash)` and pushed to a package
registry; normal CI runs download them. Only a change to `ffmpeg.config`, `mpv.config`, or a pinned
version triggers a rebuild.

## S4 deliverable: the LGPL feature-delta report

The spike is not "it compiles". It is a written answer to R1. For each target, build **both**
configurations and diff them:

```bash
# The comparison the spike exists to produce
native/build.sh --variant lgpl  linux-x86_64
native/build.sh --variant gpl   linux-x86_64   # comparison only, never shipped

diff <(out/lgpl/bin/ffmpeg -hide_banner -codecs)  <(out/gpl/bin/ffmpeg -hide_banner -codecs)
diff <(out/lgpl/bin/ffmpeg -hide_banner -filters) <(out/gpl/bin/ffmpeg -hide_banner -filters)
diff <(out/lgpl/bin/mpv --list-options)           <(out/gpl/bin/mpv --list-options)
```

Then run the conformance corpus ([`../conformance/`](../conformance/)) against the LGPL build and
record any vector that regresses. **Go/no-go criterion:** every `severity: blocking` vector passes on
the LGPL build. If one does not, the ADR-0002 escape hatches apply in order — permissive substitute,
separate-process GPL component, or a conscious decision to ship the whole product as GPL and drop the
App Store.

Expected losses to confirm rather than assume:
- DVD menu navigation (libdvdnav) — main-title playback unaffected.
- A handful of GPL-only FFmpeg filters, none of which are on the playback path.
- Some GPL-only mpv video outputs and filters.
- Software x264/x265 encoding — replaced per above.

## 🔴 Blocking on legal review

Spike **S8** runs in parallel: written counsel opinions on (a) LGPL + App Store distribution, and
(b) Dolby/DTS decoder distribution for the intended business model. Neither is an engineering
question, and both can invalidate the iOS target regardless of what this spike finds. Do not write
iOS code before S8 reports.
