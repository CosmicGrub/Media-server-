# 04 — Platform Strategy ("methods of application")

One shared Rust core. Native shells. This document is the per-platform implementation plan, the honest capability
matrix, and the build/distribution mechanics.

**Current priority: Windows first, with Linux support kept for the distros it actually ships to
(Steam Deck/SteamOS-style handhelds, Arch- and Ubuntu-derived desktops).** macOS/Apple platforms
are out of scope for the actively-built desktop player and its CI for now — the sections below
that cover them are the original, broader strategic exploration and are kept as reference for a
possible future revisit, not a description of what is currently being built or tested.

## 1. The sharing model

```
                    ┌─────────────────────────────┐
                    │   lumen-core  (Rust)        │  ~70% of all non-UI logic
                    └──────────────┬──────────────┘
        ┌──────────────┬───────────┼───────────┬──────────────┐
     UniFFI         UniFFI      napi-rs     wasm-bindgen    REST
    (Kotlin)        (Swift)    (Node/Tauri)   (browser)    (anything)
        │              │            │             │            │
    Android/ATV   iOS/tvOS/mac   Desktop        Web        TV platforms
    Kotlin+Compose  Swift+SwiftUI  Tauri v2     React        Tizen/webOS/Roku
```

**Why not one UI framework for everything?** Because the two hardest requirements — GPU-composited HDR video with a
custom render context, and OS-level exclusive audio with bitstream passthrough — are exactly the things every
cross-platform UI framework abstracts away from you. You would spend more time fighting the framework's platform
channels than you'd save. The core-sharing model gets you ~70% code reuse on the parts where reuse actually matters
(library logic, matching, sync, the playback ladder) and 0% on the 30% that must be native anyway.

**If you must pick a single-framework shortcut**, the least-bad option is **Flutter + `media_kit`**, because
`media_kit` is itself a libmpv wrapper — same engine, unified Dart API across Android, iOS, macOS, Windows, Linux and
(via HTML5 `<video>`) the browser, with 80%+ of its implementation in Dart FFI so behaviour is consistent across
platforms. You give up: fine-grained audio passthrough control, TV-platform leanback idioms, and some HDR display-mode
switching. Reasonable for a solo developer; wrong for a product that promises bit-perfect remux playback.

## 2. Desktop — Windows, macOS, Linux

**Recommendation: Tauri v2.**

| Aspect | Plan |
|---|---|
| Shell | Tauri v2 — Rust backend (which *is* `lumen-core`), system WebView for UI |
| UI | React + TypeScript, shared design system and ~80% of components with the web PWA |
| Video | libmpv rendered into a **native child window / GPU layer beneath the WebView**, with a transparent WebView region for the OSD. Tauri v2 supports multiple windows and native child surfaces; on Windows use a D3D11 swapchain child HWND, on macOS a `CAMetalLayer`-backed `NSView`, on Linux a Vulkan/GL subsurface. |
| Audio | WASAPI exclusive (Win) / CoreAudio (mac) / ALSA-PipeWire (Linux) via mpv's `ao` |
| Binary size | ~15–40 MB app + ~40–70 MB native AV stack. Compare Electron's 150 MB+ baseline. |
| Idle RAM | Target < 200 MB with UI open |

**The one hard part:** compositing a WebView over a hardware video surface without tearing or a 1-frame lag. Two
strategies, prototype both in week 1 (this is the highest-risk desktop spike):
- **A — Native-under-web:** video in a child window below the transparent WebView. Best performance, fiddly z-order
  and input routing, no rounded corners or blur over video.
- **B — Web-over-texture:** render mpv to an offscreen FBO, hand the texture to a custom Tauri renderer. More
  flexible visually, costs a copy, harder on macOS.

Start with A; it's what Kodi and Plex Desktop effectively do.

**Alternatives if Tauri's video compositing proves untenable:**
- **Qt 6 / QML** — the best "real app" TV/desktop UI toolkit, first-class GL/Vulkan item integration with libmpv
  (`MpvItem` patterns are well documented), used by Kodi-adjacent projects. Costs: C++/QML, licensing care (LGPL Qt is
  fine if dynamically linked), no code sharing with web.
- **Avalonia (.NET)** — good XAML story, working libmpv bindings exist for .NET with OpenGL and software render
  fallbacks across Windows/macOS/Linux. Reasonable if the team is .NET-shaped.
- **Slint** — Rust-native, small, growing; less mature ecosystem.
- Do **not** use Electron. The RAM cost on a NAS-adjacent always-on app is indefensible.

**Distribution:** MSI/NSIS + winget + MS Store (optional); DMG + notarization + Homebrew cask; Flatpak (primary on
Linux — solves the FFmpeg/mpv dependency mess), AppImage, `.deb`/`.rpm`, AUR. Auto-update via Tauri updater with
signed manifests.

## 3. Android + Android TV + Fire TV

**Recommendation: native Kotlin, Jetpack Compose + Compose for TV.**

| Aspect | Plan |
|---|---|
| Core | `lumen-core` via **UniFFI** → generated Kotlin bindings; `.so` per ABI (arm64-v8a primary, armeabi-v7a for old TV boxes, x86_64 for emulators/Chromebooks) |
| Player | libmpv built with the NDK, rendering to a `SurfaceView` (never `TextureView` — you lose HDR and zero-copy) |
| HW decode | `mediacodec` via mpv; query `MediaCodecList` for real profile/level support per device |
| Audio | `AudioTrack` in passthrough mode. **Query `AudioManager.getDevices(GET_DEVICES_OUTPUTS)` → `AudioDeviceInfo.getEncodings()`** on the *current* device and re-query on `AudioDeviceCallback`. Maintain a device-quirks table (NVIDIA Shield firmware regressions are a known, recurring class of bug). |
| Secondary path | Media3/ExoPlayer for Cast sender, DRM (if ever), and as a fallback player for devices where libmpv+mediacodec misbehaves |
| UI | Two form factors, one codebase: Compose for phone/tablet, **Compose for TV** (`androidx.tv`) for leanback. Shared ViewModels over the core. |
| TV specifics | D-pad focus management, `MediaSession` + `MediaBrowserService` for Google TV integration, channel/row publishing to the home screen (`TvProvider`), Watch Next, voice search intents, HDMI-CEC awareness, display-mode switching via `Display.Mode` for frame-rate matching |
| Background audio | `MediaSessionService`, foreground notification, Android Auto for the music side |
| Storage | SAF for user-picked folders; direct paths where permitted; `MANAGE_EXTERNAL_STORAGE` avoided (Play Store policy risk) |

**Alternative:** Flutter + `media_kit` if you need Android and iOS from one team fast. You will re-do the audio
passthrough layer natively anyway.

**Distribution:** Play Store (phone/tablet/TV), Amazon Appstore (Fire TV — a large and under-served audience),
direct APK + F-Droid (F-Droid requires fully FOSS deps, which the LGPL build satisfies).

## 4. Apple — iOS, iPadOS, tvOS, macOS

**Recommendation: native Swift + SwiftUI, `lumen-core` via UniFFI as an XCFramework.**

| Aspect | Plan |
|---|---|
| Core | Rust → static lib → **XCFramework** (device + simulator, arm64 + x86_64 sim). UniFFI generates the Swift API. |
| Player | libmpv as an XCFramework, rendering via **Metal through libplacebo**. AVPlayer as a secondary path. |
| When to use AVPlayer | AirPlay 2, PiP with system controls, Spatial Audio, FairPlay (n/a for you), and battery-optimal playback of natively supported HEVC/H.264 in MP4/MOV. Also the *only* way to get Dolby Atmos on tvOS (E-AC3 JOC / AC-4). |
| When to use libmpv | Everything else: MKV, DTS family, TrueHD (decoded), VP9/AV1 on older hardware, ASS subtitles, PGS, shaders. |
| tvOS specifics | `AVDisplayManager.preferredDisplayCriteria` for HDR + frame-rate matching, focus engine, Top Shelf extension, Siri Remote gesture handling, 4 GB app-size limit (on-demand resources for anything large) |
| iOS specifics | Background audio (`AVAudioSession` `.playback`), PiP, Files app integration, Shortcuts/App Intents, Live Activities for downloads, Handoff |
| macOS | Either the Tauri desktop shell **or** a native SwiftUI Mac app sharing ~90% with iPadOS. Recommend: ship Tauri first (cross-platform parity), add a native Mac app later if it's a priority audience. |

### 4.1 The iOS problem — read [`08-legal-licensing.md`](08-legal-licensing.md) before writing code

GPL and the App Store are genuinely incompatible: App Store terms impose non-transferable licenses and DRM, which
conflicts with the GPL's redistribution freedoms — this is why GPL VLC was pulled in 2011 and why VideoLAN pursued
relicensing to LGPL. Even LGPL is contested, because LGPL §6 requires users be able to relink against a modified
library, which a locked-down store arguably prevents.

Practical mitigations, in order of preference:
1. **Build the entire native stack LGPL-only** (FFmpeg `--disable-gpl`, mpv `-Dgpl=false`, libplacebo is LGPLv2.1+),
   **dynamically linked** as frameworks inside the app bundle.
2. **Publish complete corresponding source** for the LGPL components plus **object files / a build script** allowing
   relinking, at a stable public URL, referenced in the app's About screen.
3. **Own 100% of your own code's copyright** so you retain the option to license it however you need.
4. **Get a written legal opinion** before submission. Budget for it. Precedent exists in both directions.
5. **Fallback:** if counsel says no, ship iOS via **AltStore / EU alternative app marketplaces / TestFlight for a
   community build**, or use **VLCKit** (VideoLAN ships it under LGPL and maintains App Store presence themselves,
   which is the strongest existence proof available).

Enforce #1 mechanically:

```bash
# CI gate — fails the build if any GPL config leaks in
grep -R -- '--enable-gpl\|--enable-version3\|--enable-nonfree' native/ && exit 1
ffmpeg -version | grep -q 'enable-gpl' && exit 1
```

**Distribution:** App Store (iOS/iPadOS/tvOS/Mac), TestFlight for betas, notarized DMG direct for macOS, and
EU alternative marketplaces where they help.

## 5. Web

**Recommendation: React + TypeScript PWA**, sharing the design system and most components with the Tauri desktop UI.

| Aspect | Plan |
|---|---|
| Playback | `<video>` + **MSE** with CMAF/fMP4; **LL-HLS** for live and for iOS Safari (which requires HLS) |
| Direct Play | Attempt native playback of MP4/H.264/AAC and (browser-dependent) HEVC/VP9/AV1/Opus/FLAC before falling back to transcode |
| Advanced path | **WebCodecs** + WebGL/WebGPU custom renderer to Direct Play formats the `<video>` element refuses; own audio via Web Audio. Treat as progressive enhancement. |
| Subtitles | **libass compiled to WASM** (JASSUB lineage) for correct ASS; native `<track>` for WebVTT |
| Offline | PWA with a Service Worker; downloads to OPFS with encryption; installable |
| Casting | Chromecast sender (Cast SDK), AirPlay via `<video>` attributes |
| Constraints to surface in UI | No lossless audio passthrough. No HDR on most browsers (some Chromium builds do HDR video with the right flags). Limited codec set. |

Also build a **Cast receiver app** (Chromecast, CCwGTV) — an HTML receiver reusing the web player. Cheap, high value.

## 6. Living-room platforms (Phase 3+)

| Platform | Approach | Effort | Worth it? |
|---|---|---|---|
| **Fire TV** | Android app (already covered) | ~0 extra | ✅ Yes, day one |
| **Google TV / Android TV** | Android app | ~0 extra | ✅ Yes, day one |
| **Apple TV** | tvOS app | included above | ✅ Yes |
| **Samsung Tizen** | Web app packaged with Tizen Studio; reuse the web player | Medium | ✅ Phase 3 — large installed base |
| **LG webOS** | Web app; reuse the web player | Medium | ✅ Phase 3 |
| **Roku** | **BrightScript + SceneGraph only.** Full rewrite. No libmpv. Codec support is whatever Roku's decoder does. | High | ⚠️ Phase 4. Huge US installed base but a separate product. |
| **Xbox** | UWP/web app, or Android-ish | Medium | Phase 4 |
| **PlayStation** | Media app via web/Discover | Low priority | ✗ |
| **Steam Deck / SteamOS** | Linux desktop build + Flatpak + gamepad UI mode | Low | ✅ Phase 2 — cheap win, enthusiast audience |
| **Raspberry Pi / LibreELEC-style appliance** | Linux ARM build + a kiosk shell | Medium | Phase 3 |
| **DLNA/UPnP renderers, smart TVs generally** | Serve as a **DLNA/UPnP MediaServer** so any TV can browse it | Low | ✅ Phase 2 — enormous compatibility reach for little work |

## 7. Capability matrix — set expectations in the product, not the forum

| Capability | Win | mac | Linux | Android/TV | iOS/tvOS | Web |
|---|:--:|:--:|:--:|:--:|:--:|:--:|
| MKV / any container | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ remux |
| HEVC / AV1 / VP9 HW decode | ✅ | ✅ | ✅ | ✅ | ⚠️ device | ⚠️ browser |
| Hi10P H.264 | ✅ SW | ✅ SW | ✅ SW | ⚠️ SW | ⚠️ SW | ✗ |
| HDR10 / HLG passthrough | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| HDR10+ dynamic | ✅ | ✅ | ✅ | ✅ | ⚠️ | ✗ |
| Dolby Vision P5 / P8 | ✅ tonemap | ✅ tonemap | ✅ tonemap | ✅ (native on some) | ✅ native | ✗ |
| Dolby Vision P7 FEL | ⚠️ base layer | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ✗ |
| TrueHD / Atmos passthrough | ✅ | ❌ | ✅ | ✅ | ❌ | ✗ |
| DTS-HD MA / DTS:X passthrough | ✅ | ❌ | ✅ | ✅ | ❌ | ✗ |
| Atmos via E-AC3 JOC | ✅ | ✅ | ✅ | ✅ | ✅ | ✗ |
| Lossless decode → LPCM 7.1 | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| Bit-perfect / exclusive audio | ✅ | ✅ | ✅ | ⚠️ | ✗ | ✗ |
| DSD native | ✅ | ⚠️ | ✅ | ⚠️ USB | ✗ | ✗ |
| ASS subtitles (libass fidelity) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ wasm |
| PGS / VobSub | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| User shaders (Anime4K etc.) | ✅ | ✅ | ✅ | ✅ | ⚠️ perf | ✗ |
| Frame-rate / display-mode match | ✅ | ✅ | ✅ | ✅ | ✅ tvOS | ✗ |
| BDMV / ISO / VIDEO_TS | ✅ | ✅ | ✅ | ✅ | ✅ | ✗ |
| Offline downloads | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ PWA |

Publish this table **in the app**. Nobody else does, and it converts support tickets into informed users.

## 8. Build & CI

| Concern | Approach |
|---|---|
| **Native AV stack builds** | A single `native/` recipe system (recommend **`vcpkg` overlay ports** or a hand-rolled meson/ninja + cross-file setup, orchestrated by `cargo-xtask`). Produce reproducible, versioned prebuilt artifacts per (os, arch, abi) pushed to a package registry — do **not** rebuild FFmpeg on every CI run. |
| **Toolchains** | Linux: containerized cross-compile (`cross`). Android: NDK r27+. Apple: macOS runners, XCFramework assembly. Windows: MSVC + clang-cl for the AV stack. |
| **Rust core** | `cargo test` + `cargo clippy -D warnings` + `cargo deny` (license + advisory gates) + `cargo fuzz` on parsers |
| **License gate** | `cargo-deny` for Rust deps; the `--enable-gpl` grep gate for native; SPDX SBOM generated per release (CycloneDX) |
| **Playback conformance** | The 20-file corpus from [`03-playback-engine.md`](03-playback-engine.md) §10, run headless on Linux/Win/mac and on a small physical device rack (one Android TV box, one Shield, one Fire TV, one Apple TV, two phones) via a self-hosted runner |
| **Perf regression** | Startup-to-first-frame, seek latency, scan throughput, idle RSS tracked per commit with a hard fail on >10% regressions |
| **Release** | Single `cargo xtask release` producing all platform artifacts, signed, with SBOM + changelog. Cadence: monthly stable, weekly beta, nightly. |

## 9. Testing strategy beyond conformance

- **Property tests** on the filename parser and the playback ladder (`proptest`) — the ladder must never produce a
  plan the client can't play, for any capability set.
- **Fuzzing** on NFO parsing, playlist parsing, subtitle parsing, and the API. These take untrusted input.
- **Golden-file tests** on metadata matching against a labelled corpus of ~500 real-world filenames.
- **Soak tests**: 24 h continuous playback, 100k-item library scan, 50 concurrent streams.
- **Chaos**: kill the transcoder mid-stream, yank the network mount, fill the disk, corrupt the DB — each must
  degrade gracefully with a clear error, never lose user data.

## Sources
- [Compose Multiplatform / KMP production readiness 2026](https://www.kmpship.app/blog/is-kotlin-multiplatform-production-ready-2026)
- [Kotlin Multiplatform vs Flutter vs React Native 2026](https://www.dualmedia.com/kotlin-multiplatform-2026/)
- [media_kit — cross-platform libmpv player for Flutter](https://pub.dev/packages/media_kit)
- [LibMpv-OpenGL — cross-platform libmpv for .NET/Avalonia](https://github.com/mysteryx93/LibMpv-OpenGL)
- [The GPL and the iOS App Store — Michel Fortin](https://michelf.ca/blog/2011/gpl-ios-app-store/)
- [FSF — VLC and App Store DRM enforcement](https://www.fsf.org/blogs/licensing/vlc-enforcement)
- [LGPL and app stores — LWN](https://lwn.net/Articles/526355/)
