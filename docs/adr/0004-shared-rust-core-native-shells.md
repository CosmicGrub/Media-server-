# ADR-0004 — One shared Rust core, thin native shells per platform

**Status:** Proposed (pending spikes S1–S3)
**Date:** 2026-07-27

## Context

The product targets PC (Windows/macOS/Linux), Android (phone + TV), iOS/iPadOS/tvOS, and the web. The obvious
instinct is to pick one cross-platform UI framework and write the app once.

But the two hardest requirements in this product are exactly the things every cross-platform UI framework abstracts
away:

1. **GPU-composited HDR video with a host-owned render context** (libmpv's render API into D3D11/Metal/Vulkan/GLES,
   with display-mode switching for frame-rate matching).
2. **OS-level exclusive audio with compressed-bitstream passthrough** (WASAPI exclusive + IEC 61937 on Windows,
   ALSA `hw:` on Linux, `AudioTrack` with `ENCODING_DOLBY_TRUEHD`/`ENCODING_DTS_HD` on Android — each requiring
   direct, per-platform API access and per-device capability probing).

Meanwhile the platforms have genuinely different UX idioms — a D-pad leanback TV interface, a touch phone interface,
a pointer desktop interface — and users notice when an app fights its platform.

## Decision

**Write the domain logic once in Rust (`lumen-core`) and bind it into thin, genuinely native shells.**

| Binding | Target |
|---|---|
| **UniFFI** → Kotlin | Android, Android TV, Fire TV |
| **UniFFI** → Swift | iOS, iPadOS, tvOS, (optional native macOS) |
| **Native (Tauri v2 is Rust)** | Windows, macOS, Linux desktop |
| **wasm-bindgen** / REST | Web PWA |
| **REST** | Tizen, webOS, Roku, third-party clients |

`lumen-core` owns: the playback session state machine, track selection, the **playback decision ladder**, device and
sink capability modelling, the library data model and queries, file identity, the scanner pipeline, matching,
metadata provider orchestration, sync/CRDT, the plugin host, and the source VFS. That is ~70% of the non-UI code and
100% of the logic where cross-platform divergence would produce bugs users experience as "it works on my phone but
not my TV."

Shells own: layout, navigation, input handling, platform integrations (MediaSession, Top Shelf, Cast, PiP, widgets),
and the platform-specific audio/video output plumbing.

## Rationale

1. **Reuse lands where it matters.** The playback ladder in particular *must* be one implementation — if the server
   and five clients each have their own version, they will disagree, and users will experience that as random
   transcoding.
2. **Native shells cost less than fighting a framework.** The 30% that must be native (video surface, audio
   passthrough, TV idioms) is exactly the 30% a cross-platform framework makes hardest.
3. **Memory safety at the hostile-input boundary.** The core parses filenames, NFO files, subtitles, playlists, and
   provider responses — all attacker-influenced. Rust removes an entire bug class here.
4. **UniFFI is mature** and generates idiomatic Kotlin and Swift, including async and error types.
5. **Platform-native UI is a differentiator** in a category where the incumbents' clients feel like ports.

## Alternatives rejected

| Alternative | Why rejected |
|---|---|
| **Flutter everywhere** (with `media_kit`) | Genuinely the best single-framework option — `media_kit` is itself a libmpv wrapper with a unified Dart API across Android, iOS, macOS, Windows, Linux, and the browser, with 80%+ of its implementation in Dart FFI. Rejected because it costs fine-grained audio passthrough control, TV leanback idioms, and display-mode switching — the exact things this product promises. **Documented as the fallback if the team is too small for native shells.** |
| **Kotlin Multiplatform + Compose Multiplatform everywhere** | Compose for iOS went Stable in CMP 1.8.0 (May 2025) and is in production at scale; a strong option if the team is Kotlin-shaped. Rejected because the AV core is C and the natural FFI language for it is Rust, and because CMP's desktop story is JVM-based (heavier than Tauri) with Web/Wasm still beta. |
| **React Native everywhere** | New Architecture removed the bridge bottleneck, but the native module work for libmpv + audio passthrough is the same as writing native shells, without the benefit. |
| **Electron + web everywhere** | Memory cost is indefensible for an always-on app; no mobile story; no TV story. |
| **C++ core (Kodi's model)** | Same reuse, worse safety at the parsing boundary, harder cross-platform builds, no UniFFI equivalent. |

## Consequences

**Positive**
- One implementation of the logic users would notice diverging.
- Native feel and native platform integration on every target.
- Rust's safety at the untrusted-input boundary.
- The core is testable in isolation, headlessly, in CI — no device needed for most of the test suite.

**Negative**
- Four UI codebases to maintain. Feature parity requires deliberate process (a shared feature matrix, a
  "no feature ships until it's on desktop + Android" rule for Phase 1–3).
- FFI boundaries need care: UniFFI's type system constrains the core's public API shape, so the core needs a
  deliberately designed facade rather than exposing internal types.
- Larger team, or a longer timeline, than a single-framework approach. See [`../09-roadmap.md`](../09-roadmap.md) §9.
- Debugging spans Rust ↔ Kotlin/Swift/TS.

## Escape hatch

If the team is smaller than ~6 engineers, adopt **Flutter + `media_kit`** for mobile and desktop, keep `lumen-core`
in Rust behind `flutter_rust_bridge`, and accept a documented capability gap on audio passthrough and TV idioms.
Decide this in Phase 0, not in Phase 3.

## References
- [media_kit](https://pub.dev/packages/media_kit)
- [Compose Multiplatform / KMP production readiness 2026](https://www.kmpship.app/blog/is-kotlin-multiplatform-production-ready-2026)
- [KMP vs Flutter vs React Native 2026](https://www.dualmedia.com/kotlin-multiplatform-2026/)
