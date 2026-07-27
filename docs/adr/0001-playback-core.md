# ADR-0001 — Use libmpv as the playback core on all native platforms

**Status:** Proposed (pending spikes S1–S3)
**Date:** 2026-07-27
**Deciders:** TBD

## Context

The product's primary promise is that it plays anything — including Blu-ray remuxes with lossless audio, Dolby Vision,
Hi10P anime with ASS subtitles and attached fonts, VC-1 interlaced, PGS subtitles, and BDMV/ISO disc structures —
bit-perfectly where the hardware allows.

Every cross-platform UI framework ships a video widget, and all of them wrap `AVPlayer` (Apple), `ExoPlayer`/`MediaCodec`
(Android), or Media Foundation (Windows). Those platform players collectively fail on: MKV, DTS family, TrueHD,
Hi10P, PGS, ASS with attached fonts, VP9/WebM on iOS, and arbitrary container/codec combinations. On Android, hardware
decoder slots are scarce (roughly 5–16 depending on chipset) and profile support is inconsistent across devices.

## Decision

**Embed libmpv on every native platform (Windows, macOS, Linux, Android, iOS, tvOS), using the render API
(`mpv/render.h`) rather than window embedding, with `gpu-next`/libplacebo as the video output.**

Secondary players are used only where the platform requires them:
- **AVPlayer** on Apple for AirPlay 2, PiP with system controls, Spatial Audio, and Atmos via E-AC-3 JOC / AC-4 on tvOS.
- **Media3/ExoPlayer** on Android for the Cast sender and as a per-device fallback where libmpv+mediacodec misbehaves.
- The web player is a separate implementation (MSE/WebCodecs) — libmpv does not apply.

## Rationale

1. **Coverage.** libmpv, backed by FFmpeg, handles every container/codec/subtitle combination in the conformance
   corpus. Nothing else embeddable does.
2. **Quality ceiling.** `gpu-next` is built on libplacebo, which provides the best open-source HDR tone mapping
   (BT.2390, ST 2094-10/-40), gamut mapping, debanding, dithering, and a user-shader pipeline (Anime4K, FSRCNNX,
   NNEDI3, CAS). This is a capability no competitor in this space has, and it is free once libmpv is embedded.
3. **A stable, designed-for-embedding API.** The client API is thread-safe and versioned (currently
   `MPV_MAKE_VERSION(2, 5)`); the render API explicitly supports rendering into a host-owned GL/Vulkan/D3D11/Metal
   context and drawing a host OSD on top. The mpv project recommends the render API over window embedding due to
   platform-specific problems with the latter, particularly on macOS.
4. **Licensing.** libmpv can be built LGPLv2.1+ (`-Dgpl=false`), which is the only path that keeps App Store
   distribution and non-GPL application licensing available. See ADR-0002.
5. **Precedent.** IINA (macOS), mpv-android, Celluloid, Haruna, SMPlayer, and `media_kit` (which brings libmpv to
   Flutter across Android, iOS, macOS, Windows, Linux) all embed libmpv successfully in production.
6. **Cost of the alternative.** Building an equivalent on raw FFmpeg means reimplementing A/V sync, seek accuracy,
   hardware-decode fallback chains, filter graph management, and subtitle compositing — 2–4 engineer-years for a
   worse result.

## Consequences

**Positive**
- Format coverage and HDR quality are solved on day one.
- User shaders become a shipping feature at near-zero marginal cost.
- One engine means one set of playback behaviours to test and document across platforms.
- libass gives reference-quality ASS/SSA rendering everywhere.

**Negative**
- A large native dependency (~40–70 MB per platform) that must be cross-compiled for six targets and kept updated.
- libmpv's threading model constrains the architecture: the render context is pinned to the GPU thread; property
  access must never block it. The Rust binding must enforce this at the type level (`MpvHandle: Send+Sync`,
  `MpvRenderContext: !Send`).
- Some behaviour is configured through mpv's option system rather than a typed API, so the binding must validate and
  own a curated option surface rather than exposing raw strings to the UI.
- Debugging spans a Rust↔C boundary.
- Compositing a host UI over the video surface is non-trivial on each platform (spike S1).

## Fallbacks (decided now, not improvised later)

| If | Then |
|---|---|
| S1 fails (Tauri cannot composite over hardware video on ≥1 desktop) | Switch the desktop shell to Qt 6/QML, which has well-trodden libmpv integration. Keep libmpv. |
| S3 fails (libmpv unworkable on iOS/tvOS) | Use **VLCKit** on Apple — proven App Store presence, LGPL, weaker HDR. Accept a documented capability gap. |
| Team is too small for six native shells | Use **Flutter + `media_kit`** (also libmpv) and accept reduced control over audio passthrough and TV idioms. |

## References
- [libmpv API documentation](https://mpv-player-mpv.mintlify.app/embedding/libmpv)
- [mpv-examples/libmpv](https://github.com/mpv-player/mpv-examples/tree/master/libmpv)
- [media_kit](https://github.com/media-kit/media-kit)
- [Flutter video playback constraints — decoder slots, AVPlayer format gaps](https://verygood.ventures/blog/video-playback-flutter-feed/)
