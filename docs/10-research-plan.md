# 10 — Research Plan: Open Questions, Spikes & Reading List

The research that still needs doing, organized so it can be assigned. Each item has a question, a method, and a
decision it unblocks.

## 1. Must-answer-before-committing (blocking)

| # | Question | Method | Unblocks |
|---|---|---|---|
| R1 | Which mpv/FFmpeg features are lost in an **LGPL-only** build for the exact target versions? | Build both configurations for all 6 targets; diff `--list-options`, filter lists, `ffmpeg -codecs/-filters`; run the conformance corpus against each | The entire license posture (ADR-0002) and iOS viability |
| R2 | Can a **WebView composite cleanly over a hardware video surface** in Tauri v2 on Win/mac/Linux? | Spike S1: build all three, measure frame pacing with a high-speed capture or `PresentMon`/`Metal System Trace` | Desktop shell choice (ADR-0004) |
| R3 | Is **TrueHD/Atmos + DTS-HD MA passthrough** reliably achievable on Windows/Linux/Android with libmpv? | Spike S5 against a real AVR; test 6+ Android devices incl. Shield, Fire TV, Chromecast GTV, a Sony/Samsung TV | The headline remux promise; the capability matrix |
| R4 | Does 🔴 **App Store + LGPL** survive legal review? | Counsel; study VideoLAN's current App Store presence and their published licensing posture | Whether iOS ships at all, and how |
| R5 | What does **Dolby/DTS decoder distribution** cost or require for a free vs. paid product? | Contact Dolby and Xperi licensing directly; counsel | Business model; whether decode-to-PCM ships by default |
| R6 | What is realistic **scanner throughput** on a spinning-rust NAS over SMB with 50k files? | Spike S6 with synthetic corpora at 1k/10k/50k | Perf targets; whether the six-stage design needs revisiting |
| R7 | Is the **Wasm plugin** round-trip fast enough for metadata (target: < 50 ms overhead per call)? | Spike S7; benchmark Wasmtime cold start, instance pooling, host-call overhead | ADR-0003; whether to pool instances or spawn per call |

## 2. Should-answer-during-P1 (shaping)

| # | Question | Method |
|---|---|---|
| R8 | Best **filename parsing** approach: port `guessit`, port `anitomy`, or write a layered tokenizer in Rust? | Build a labelled corpus of 1,000 real filenames (scene, P2P, anime, foreign, self-ripped); benchmark all three for accuracy |
| R9 | How accurate is **runtime-based match disambiguation**? | Measure: given title+year ambiguity, does adding runtime proximity from the probe resolve it? Hypothesis: it's the strongest single signal and nobody uses it |
| R10 | Which **intro-detection** method wins: chromaprint cross-episode correlation, audio-fingerprint hashing, or visual? | Implement chromaprint first (cheapest); evaluate against 200 hand-labelled episodes |
| R11 | Practical **WebCodecs Direct Play** coverage across Chrome/Firefox/Safari in 2026? | Build a capability probe page; test the real matrix (HEVC, AV1, VP9, Opus, FLAC, multichannel) |
| R12 | **Sink capability probing** APIs per OS — what's actually queryable vs. what must be user-configured? | Write a probe tool per platform: Android `AudioDeviceInfo.getEncodings()`, WASAPI `IsFormatSupported`, ALSA ELD parsing, CoreAudio `AudioStreamBasicDescription` enumeration |
| R13 | Do **hardware tone-mapping** paths (Vulkan/libplacebo, OpenCL) hold real-time 4K on a $100 Arc A310 / a 5-year-old iGPU? | Benchmark transcode with HDR→SDR on representative hardware |
| R14 | What does the **display-mode switching** API look like on each platform, and how reliable is it? | Per-platform spike: `AVDisplayManager` (tvOS), `Display.Mode` (Android), DXGI/`SetDisplayConfig` (Win), DRM/KMS + Wayland (Linux) |
| R15 | Which **embedding model** gives the best quality/size for subtitle semantic search on CPU? | Benchmark `bge-small`, `all-MiniLM-L6-v2`, `gte-small` on a labelled scene-retrieval set; measure latency on a Raspberry Pi 5 and an N100 |

## 3. Should-answer-before-P3 (server)

| # | Question |
|---|---|
| R16 | SQLite vs. Postgres crossover point for library size and concurrent users — measure, don't guess |
| R17 | Optimal CMAF segment duration for the seek/latency/overhead tradeoff on LL-HLS with 100 Mbps sources |
| R18 | Transcode session isolation: process-per-session vs. a worker pool — measure memory and startup cost at 20 concurrent sessions |
| R19 | CRDT library choice (`automerge`, `yrs`, or a hand-rolled LWW+G-counter) for watch state — the hand-rolled option is probably right; prove it |
| R20 | Relay architecture: WebRTC data channels, QUIC, or plain TLS forwarding? Zero-knowledge requirement constrains this |
| R21 | How much of the **Jellyfin API surface** must be implemented for the top 10 third-party clients and the *arr stack to work? Enumerate by instrumenting a real Jellyfin server |

## 4. Ongoing research tracks

- **Device quirks database.** Start it in P0. Every Android TV box, every AVR, every TV that misbehaves. This becomes
  a genuine competitive moat — it's the thing you cannot copy from a repo.
- **Codec landscape.** VVC/H.266 decoder maturity in FFmpeg, AV2 progress, LCEVC, APV. Revisit quarterly.
- **HDR standards.** Dolby Vision profile evolution, HDR10+ adoption, the `dovi_tool` ecosystem for P7→P8
  conversion, libplacebo release notes.
- **Platform policy.** App Store guidelines, Play Store storage policy, DMA alternative-marketplace rules. These
  change and can invalidate a shipping plan.
- **Wasm.** WASI Preview 3 (async `future`/`stream` across the component boundary) landing — it changes the plugin
  interface design.

## 5. Reading list

### Codebases to study (in priority order)
| Repo | What to read | Why |
|---|---|---|
| `mpv-player/mpv` | `libmpv/client.h`, `render.h`, `player/`, `video/out/gpu_next/` | The core you're embedding |
| `mpv-player/mpv-examples` | `libmpv/` — all of it | Reference embeddings for exactly your use case |
| `haasn/libplacebo` | Tone-mapping and gamut-mapping implementations, `docs/` | The HDR pipeline |
| `mpv-android/mpv-android` | JNI bridge, `SurfaceView` handling, build scripts | Android integration, solved |
| `IINA/IINA` | `MPVController.swift`, render layer | macOS/Swift integration, solved |
| `jellyfin/jellyfin` | `Emby.Server.Implementations/Library/`, `MediaBrowser.Providers/`, `MediaBrowser.MediaEncoding/` | Scanner, providers, and the transcode decision logic — including its problems |
| `jellyfin/jellyfin-meta` discussion #125 | Scanner refactoring | Learn from their retrospective before designing yours |
| `xbmc/xbmc` | `xbmc/video/`, `xbmc/filesystem/`, `xbmc/pvr/`, NFO handling | Disc structures, PVR abstraction, sidecar metadata |
| `videolan/vlc` | `modules/demux/`, `modules/codec/` | Format-tolerance techniques |
| `media-kit/media-kit` | Dart FFI over libmpv | If you consider the Flutter path |
| `bytecodealliance/wasmtime` + `component-model` book | Host embedding, WIT, resource limits | Plugin host |
| `quodlibet/mutagen`, `beetbox/beets` | Music metadata handling | The music side is deceptively deep |
| `guessit-io/guessit`, `dbr/tvnamer`, `anitomy` | Filename parsing | R8 |
| `Radarr/Sonarr` | Quality profiles, release parsing, naming | The parsing logic and the ecosystem you'll integrate with |

### Specifications & references
- **Containers:** Matroska specification (matroska.org), ISO/IEC 14496-12 (ISOBMFF), ISO/IEC 23000-19 (CMAF),
  MPEG-TS (ISO/IEC 13818-1), Blu-ray BDMV/`.mpls` structure (community documentation)
- **Video:** ITU-T H.264/H.265/H.266, AV1 spec (AOMedia), ITU-R BT.709/BT.2020/BT.2100, SMPTE ST 2084 (PQ),
  SMPTE ST 2086 (mastering metadata), SMPTE ST 2094-10/-40 (dynamic metadata), ITU-R BT.2390 (tone mapping)
- **Audio:** IEC 60958 / **IEC 61937** (compressed audio over S/PDIF & HDMI — the passthrough spec),
  Dolby TrueHD/MLP, Dolby Atmos over MAT & E-AC-3 JOC, AC-4, DTS-HD/DTS:X, EBU R128 (loudness), ITU-R BS.1770
- **Streaming:** RFC 8216 (HLS), Apple LL-HLS spec, ISO/IEC 23009-1 (DASH), RFC 8216bis
- **Subtitles:** ASS/SSA spec (informal, libass is the reference), CEA-708, IMSC 1.1/TTML2, W3C WebVTT
- **Discovery:** RFC 6762/6763 (mDNS/DNS-SD), UPnP AV/DLNA guidelines
- **Wasm:** WASI Preview 2/3, Component Model & WIT specs (bytecodealliance)
- **MCP:** Model Context Protocol specification (for the agent tool surface)

### Communities worth reading regularly
r/jellyfin, r/PleX, r/htpc, r/mpv, the Kodi forums (especially the audio and Android sub-forums), AVSForum's HTPC and
Blu-ray sections, doom9 (still the best place for codec and remux depth), the mpv and Jellyfin GitHub issue trackers
(sorted by 👍 — this is a free prioritized feature backlog), and MakeMKV's forum for disc-structure edge cases.

## 6. Competitive intelligence to gather (a weekend of work, high value)

1. **Install and instrument all four competitors.** Watch what Plex and Jellyfin actually send over the wire during
   a playback session; capture their transcode decisions on the conformance corpus. You will learn more in two days
   than from a month of reading.
2. **Feed all 20 conformance files to Plex, Jellyfin, Kodi, VLC, and Infuse** on each platform. Record: Direct Play or
   not, audio path taken, subtitle correctness, HDR handling, time-to-first-frame. **This table is your product
   spec** — every ✗ in a competitor's column that you can turn into a ✓ is a reason for someone to switch.
3. **Read the top 100 upvoted open issues** on jellyfin, jellyfin-androidtv, jellyfin-web, and xbmc. That's your
   feature backlog, pre-validated by real users.
4. **Survey the Kodi addon and Jellyfin plugin repos** for what people actually build — it tells you which plugin
   interfaces matter.

## 7. Deliverables from this research phase

- [ ] Conformance corpus assembled (20 clips + manifests) and committed
- [ ] Competitor comparison matrix filled in for all 20 files × 5 products × 4 platforms
- [ ] LGPL feature-delta report (R1)
- [ ] Sink-probing capability report per OS (R12)
- [ ] Device quirks database, initial entries
- [ ] Legal opinions on R4 and R5
- [ ] Spike reports S1–S8 with go/no-go recommendations
- [ ] Filename-parser benchmark against a 1,000-file labelled corpus (R8)
- [ ] ADRs updated with the spike outcomes
