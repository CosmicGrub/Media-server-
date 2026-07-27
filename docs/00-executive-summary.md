# 00 — Executive Summary & Recommended Stack

> Codename used throughout these docs: **Lumen**. Placeholder — rename freely.

## 1. The one-sentence recommendation

Build **one native playback core** (mpv/libmpv + FFmpeg + libplacebo, LGPL-only build) wrapped in **one shared Rust
domain layer**, and put **thin, genuinely native shells** on top of it per platform — rather than trying to find a
single cross-platform UI framework that can also do frame-accurate HDR video and lossless audio passthrough. No such
framework exists.

## 2. Reality check on scope (read this first)

You are asking for the union of four mature products that have, collectively, ~60 years of engineering behind them:

| Product | Age | Est. cumulative engineering |
|---|---|---|
| VLC / VideoLAN | since 2001 | thousands of contributor-years |
| Kodi / XBMC | since 2002 | thousands of contributor-years |
| Plex | since 2008, ~200+ employees | hundreds of engineer-years, funded |
| Jellyfin | since 2018 fork of Emby | hundreds of contributor-years |

A credible estimate for "all of the above, production-ready, on PC + Android + iOS + web, with a plugin ecosystem and
an AI agent" is **8–15 engineer-years** of focused work, and that assumes you reuse mpv/FFmpeg wholesale rather than
writing a demuxer or decoder. It is not a solo six-month project. That is not a reason not to start — it is a reason
to sequence it so that **every phase ships something people would actually use**, starting with the player.

The good news: your instinct — *"player first, library second"* — is exactly right, and it is the single biggest
strategic difference between this and Plex/Jellyfin (server-first, player as an afterthought) and the reason those
products still lose to mpv/VLC on hard files. Lead with the player.

See [`09-roadmap.md`](09-roadmap.md) for the phasing that makes this tractable.

## 3. Recommended stack at a glance

| Layer | Recommendation | Why | Main alternative |
|---|---|---|---|
| **Decode / render / AV core** | **libmpv** (LGPL build) + FFmpeg (LGPL, `--disable-gpl`) + **libplacebo** (`gpu-next`) | Best-in-class codec coverage, HDR/DV tone mapping, shader pipeline, subtitle rendering via libass, hardware decode on every platform. Already does what VLC and Kodi do. | libVLC (VLCKit) — simpler API, weaker HDR/tone-mapping and shader story |
| **Shared domain layer** | **Rust**, exposed via **UniFFI** (Kotlin/Swift), **napi-rs** (desktop/Node), **wasm-bindgen** (web) | Library model, matching, sync, playback state machine, device-profile negotiation written once, run everywhere. Memory-safe against hostile media metadata. | C++ core (Kodi's model) — more platform pain, less safe |
| **Server** | **Rust + axum + SQLite (WAL) → optional PostgreSQL** | Single language with the core; low idle footprint (matters on a NAS); great FFI story | Go + Echo (faster hiring, GC pauses irrelevant here); .NET 9 (Jellyfin's path, best if you want to fork/borrow) |
| **Desktop shell (Win/mac/Linux)** | **Tauri v2** (Rust backend + web UI) with libmpv rendering into a native child surface | One UI codebase shared with web; native window/GPU access for video; small binaries | Avalonia (.NET), Qt 6 (best native TV UI, heaviest), Electron (avoid — RAM) |
| **Android + Android TV** | **Kotlin + Jetpack Compose / Compose for TV**, core via UniFFI, libmpv via NDK | Only way to get real `AudioTrack` passthrough and TV leanback UX | Flutter + `media_kit` (also libmpv — good fallback if team is Flutter-native) |
| **iOS / iPadOS / tvOS** | **Swift + SwiftUI**, core via UniFFI, libmpv as an XCFramework, AVPlayer as a *secondary* path | AVPlayer needed for AirPlay 2, FairPlay, Spatial Audio, and battery-optimal native formats; libmpv needed for everything else | VLCKit (proven on App Store, weaker HDR) |
| **Web** | **React + TypeScript PWA**, MSE/EME + WebCodecs, HLS/CMAF fallback from server | Only viable target; shares design system with the Tauri desktop UI | Svelte/SolidJS (fine, smaller ecosystem) |
| **Plugins** | **WebAssembly Component Model** via **Wasmtime**, capability-scoped WIT interfaces + a small *trusted-native* tier | Safe, cross-platform, cross-language, works identically on a NAS and a phone. Kodi's Python addons are its biggest security and portability liability — don't repeat it | Extism (thin wrapper over the same idea, faster to adopt); Deno/QuickJS sandbox (worse isolation) |
| **AI agent** | Separate opt-in process speaking **MCP** to the server; local model (llama.cpp/Ollama) by default, cloud (Claude API) opt-in | Keeps the agent out of the media path entirely; user can delete it and lose nothing | In-process agent (rejected — blast radius, coupling, memory on a NAS) |
| **Streaming protocols** | **CMAF/fMP4 + LL-HLS** primary, DASH secondary, raw HTTP range for Direct Play | LL-HLS is required for iOS/tvOS; CMAF lets one segment set serve both | HLS TS (legacy, worse) |

## 4. The five decisions that actually matter

Everything else is a detail. These five determine whether the product is good.

### D1 — Playback core: libmpv, not "a video player widget"
Every cross-platform UI toolkit ships a video widget. All of them are wrappers over `AVPlayer`/`ExoPlayer`/`MediaFoundation`,
and all of them fail on the files you care about: DTS-HD MA in MKV, Dolby Vision Profile 7, VC-1 interlaced, PGS
subtitles, 100-track anime remuxes, BDMV folders. libmpv is the only embeddable engine that handles all of it and
gives you `gpu-next`/libplacebo tone mapping and user shaders on top. See [`03-playback-engine.md`](03-playback-engine.md).

### D2 — LGPL-only build discipline, enforced in CI
The moment someone builds FFmpeg with `--enable-gpl` (for `libx264`, `libx265`, or a GPL filter) the whole product
becomes GPL and the iOS App Store — and any proprietary licensing you might want later — is off the table. Build
LGPL-only, dynamically linked, from day one, and **fail the build** if `--enable-gpl` appears. This is a one-line CI
check that saves a rewrite. See [`08-legal-licensing.md`](08-legal-licensing.md).

### D3 — Direct Play is the product; transcoding is the failure mode
Plex and Jellyfin transcode when they shouldn't, silently, and users find out when their remux turns to mush. Invert
it: never transcode without an explicit, on-screen, human-readable reason ("Your TV's HDMI sink reports no DTS-HD
support, so the DTS-HD MA 7.1 track was converted to 5.1 AC-3"). Make that explainer a headline feature. This requires
real **sink capability probing**, not a static device profile. See [`03-playback-engine.md`](03-playback-engine.md) §6
and [`05-server-library.md`](05-server-library.md) §7.

### D4 — Library identity must survive renames, moves, and remounts
The universal complaint about every one of these products is "I moved my files and lost all my watch state." Solve it
with content-derived identity (size + xxh3 of head/tail/middle chunks) as a stable secondary key alongside path, and
never key user data on path. See [`05-server-library.md`](05-server-library.md) §3.

### D5 — The plugin boundary is a security boundary
If plugins can run arbitrary code, your media server is a remote-code-execution appliance sitting on a home LAN with
access to a NAS. WASM with explicit capability grants, a signed registry, and a permission prompt at install is the
only defensible design. See [`06-plugin-system.md`](06-plugin-system.md).

## 5. What to steal from whom

| Source | Steal | Don't copy |
|---|---|---|
| **mpv** | The entire AV pipeline, `gpu-next`, user shaders, profile/conf system, precise seeking, `--audio-exclusive` | Its CLI-first UX |
| **VLC** | Format tolerance ("plays broken files"), codec breadth, the *idea* that it always just works | Its UI, its module system's complexity |
| **Kodi** | NFO/sidecar metadata model, skinning engine concept, BDMV/ISO/disc-structure playback via libbluray, PVR abstraction | Python addon sandbox (none), C++ build system, database schema |
| **Plex** | Onboarding polish, remote access without port-forwarding, unified watch-state sync, Watch Together, the "it just works for my family" bar | Cloud lock-in, closed plugin story, opaque transcode decisions |
| **Jellyfin** | `DeviceProfile` negotiation concept, REST API shape, hardware-accel matrix docs, plugin manifest/repo model | .NET scanner architecture (being rewritten for a reason), single-process coupling |
| **Emby/Infuse** | Infuse's "player-first, server-optional" positioning is the closest existing analogue to what you described | — |

## 6. Document map

| Doc | Contents |
|---|---|
| [`01-competitive-analysis.md`](01-competitive-analysis.md) | Feature-by-feature teardown of Plex/Jellyfin/Kodi/VLC/Infuse, with the specific gaps to exploit |
| [`02-architecture.md`](02-architecture.md) | System architecture, module boundaries, data flow, deployment topologies |
| [`03-playback-engine.md`](03-playback-engine.md) | The player core. Codecs, containers, HDR/DV, **remux & lossless audio deep dive**, subtitles, shaders |
| [`04-platform-strategy.md`](04-platform-strategy.md) | Per-platform implementation plans, capability matrices, build/CI/distribution |
| [`05-server-library.md`](05-server-library.md) | Scanner, matcher, metadata, artwork, schema, transcode pipeline, streaming |
| [`06-plugin-system.md`](06-plugin-system.md) | WASM plugin architecture, WIT interfaces, registry, security model |
| [`07-ai-agent.md`](07-ai-agent.md) | Optional AI agent: architecture, tools, guardrails, local vs cloud, killer features |
| [`08-legal-licensing.md`](08-legal-licensing.md) | FFmpeg/mpv licensing, codec patents, App Store conflicts, metadata provider ToS, DRM |
| [`09-roadmap.md`](09-roadmap.md) | Phasing, milestones, team shape, effort estimates, kill criteria |
| [`10-research-plan.md`](10-research-plan.md) | The open questions, spikes to run, sources to read, conformance corpus |
| [`11-compatibility-charter.md`](11-compatibility-charter.md) | **The Universal Play Guarantee.** Playback tiers, the complete container/codec/subtitle matrix, the full quality and resolution spectrum, and the honest list of what cannot work |
| [`12-container-conformance.md`](12-container-conformance.md) | **"Every MP4 and MKV plays perfectly."** The exhaustive Matroska and ISOBMFF feature surface, track auto-selection rules, and the universal recovery ladder |
| [`13-remux-transcode-matrix.md`](13-remux-transcode-matrix.md) | Remux legality per codec×container, transcode decision matrices for video/audio/subtitles, segmented delivery, offline optimize jobs |
| [`../conformance/`](../conformance/) | The machine-readable corpus that proves 11–13, with per-platform expected tiers |
| [`adr/`](adr/) | Architecture Decision Records for the load-bearing choices |

## 7. Honest risks

1. **iOS is the hardest target and may be legally constrained.** LGPL + App Store is workable but contested; see §8.
   Budget for legal review before you write iOS code, not after.
2. **Lossless audio passthrough is a per-OS minefield** and macOS effectively cannot do TrueHD/DTS-HD bitstreaming at
   all. Set expectations in the product, not in a support forum.
3. **Dolby Vision Profile 7 FEL** cannot be fully reconstructed by any open-source renderer today; the honest answer
   is base-layer HDR10 fallback. Ship that clearly labelled rather than pretending.
4. **Metadata provider terms are commercial tripwires.** TMDB and TheTVDB both require paid licenses above revenue
   thresholds. Design the provider layer so providers are swappable plugins from day one.
5. **Scope.** The most likely failure mode of this project is not technical — it's building 40% of ten features
   instead of 100% of three. The roadmap is structured to prevent that.
