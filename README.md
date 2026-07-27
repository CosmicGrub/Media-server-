# Media Server — Architecture & Research

Research, recommendations, and an implementation plan for a multi-platform media **player first**, media **server**
second — combining what's best about mpv, VLC, Kodi, Jellyfin, and Plex, with a sandboxed plugin ecosystem and an
optional, off-by-default AI operator agent.

> Codename used in the docs: **Lumen** (placeholder).

## Start here

| # | Document | What's in it |
|---|---|---|
| **00** | [Executive Summary & Recommended Stack](docs/00-executive-summary.md) | **Read this first.** The recommended stack, the five decisions that matter, honest scope assessment |
| 01 | [Competitive Analysis](docs/01-competitive-analysis.md) | Teardown of Plex/Jellyfin/Kodi/VLC/mpv/Infuse; the ten gaps to exploit |
| 02 | [System Architecture](docs/02-architecture.md) | Component map, crate layout, data flows, deployment topologies, non-functional targets |
| 03 | [Playback Engine](docs/03-playback-engine.md) | Codecs, containers, HDR/Dolby Vision, **remux & lossless-audio deep dive**, subtitles, shaders, the decision ladder, conformance corpus |
| 04 | [Platform Strategy](docs/04-platform-strategy.md) | Per-platform plans for PC / Android / iOS / web / TV; capability matrix; build, CI, distribution |
| 05 | [Server, Library & Streaming](docs/05-server-library.md) | Scanner, file identity, matching, metadata, artwork, transcoding, sync, search, live TV, API |
| 06 | [Plugin System](docs/06-plugin-system.md) | WebAssembly component plugins, WIT interfaces, permissions, registry, developer experience |
| 07 | [The Optional AI Agent](docs/07-ai-agent.md) | MCP tool surface, guardrails, local-vs-cloud models, the features that justify it |
| 08 | [Legal & Licensing](docs/08-legal-licensing.md) | FFmpeg/mpv licensing, codec patents, App Store conflicts, metadata provider terms, compliance checklist |
| 09 | [Roadmap & Effort](docs/09-roadmap.md) | Phasing, spikes with kill criteria, honest effort estimates, team shape |
| 10 | [Research Plan](docs/10-research-plan.md) | Open questions, reading list, codebases to study, competitive intelligence to gather |
| **11** | [**Compatibility Charter**](docs/11-compatibility-charter.md) | **The Universal Play Guarantee** — playback tiers, the complete container/codec/subtitle matrix, the full quality and resolution spectrum, and the honest list of what cannot work |
| **12** | [**Container Conformance**](docs/12-container-conformance.md) | **"Every MP4 and MKV plays perfectly"** — the exhaustive Matroska and ISOBMFF feature surface, track auto-selection, the universal recovery ladder |
| **13** | [**Remux & Transcode Matrices**](docs/13-remux-transcode-matrix.md) | Remux legality per codec×container, transcode decisions for video/audio/subtitles, segmented delivery, offline optimize jobs |
| — | [Conformance Corpus](conformance/) | The machine-readable test corpus that proves 11–13, with per-platform expected tiers |

**Architecture Decision Records**
- [ADR-0001 — libmpv as the playback core](docs/adr/0001-playback-core.md)
- [ADR-0002 — LGPL-only native stack, dynamically linked](docs/adr/0002-lgpl-only-build.md)
- [ADR-0003 — Plugins as sandboxed WebAssembly components](docs/adr/0003-plugin-runtime.md)
- [ADR-0004 — Shared Rust core, thin native shells](docs/adr/0004-shared-rust-core-native-shells.md)

## The recommendation in one table

| Layer | Choice |
|---|---|
| AV core | **libmpv** (LGPL build) + FFmpeg (`--disable-gpl`) + **libplacebo** / `gpu-next` |
| Shared logic | **Rust** → UniFFI (Kotlin/Swift), Tauri (desktop), wasm-bindgen / REST (web) |
| Server | **Rust + axum + SQLite (WAL)**, Postgres optional |
| Desktop | **Tauri v2** + React (fallback: Qt 6) |
| Android / TV | **Kotlin + Compose / Compose for TV**, libmpv via NDK |
| Apple | **Swift + SwiftUI**, libmpv XCFramework, AVPlayer as a secondary path |
| Web | **React PWA**, MSE/CMAF + LL-HLS, WASM libass, WebCodecs enhancement path |
| Plugins | **Wasmtime + Component Model (WIT)**, capability-scoped, signed |
| AI agent | Separate opt-in process speaking **MCP**; local model default, cloud opt-in |

## The compatibility promise

Three guarantees, enforced by [the conformance corpus](conformance/) in CI on every platform — not marketing copy:

- **G0 — Universal Play.** If a file contains a stream FFmpeg can demux and decode, it plays. Container quirks,
  missing indexes, damaged headers, wrong extensions, and non-conformant muxing are the player's problem.
- **G1 — No Silent Degradation.** Any departure from bit-exact source reproduction is stated on screen, in plain
  language, with the specific cause.
- **G2 — No Refusal.** The player never says "unsupported format." Only DRM, physically-impossible hardware demands,
  and files with no decodable data may fail — each with a specific, actionable message.

Full specification in [11](docs/11-compatibility-charter.md), [12](docs/12-container-conformance.md), and
[13](docs/13-remux-transcode-matrix.md).

## The four things that make this different from what already exists

1. **Playback transparency.** Never transcode without an on-screen, human-readable reason. A one-key Playback Report
   showing the source streams, the chosen path, and every rejected path with its cause.
2. **Remux-grade correctness.** Real sink-level audio capability probing, TrueHD/Atmos and DTS-HD MA/DTS:X
   passthrough where the platform allows it, bit-perfect mode, BDMV/ISO/`.mpls` disc-structure fidelity — with an
   honest, in-app capability matrix instead of forum archaeology.
3. **mpv's quality ceiling in a mainstream product.** libplacebo tone mapping, and Anime4K / FSRCNNX / CAS shader
   packs as one-click per-library presets. No competitor has this.
4. **A library that survives you moving your files.** Content-derived identity as a stable key, so renames, moves,
   and remounts never cost watch state.

## Honest scope note

This is roughly **8+ engineer-years** to reach "all of the above, production-ready, on four platforms." The roadmap
is sequenced so each phase ships something usable on its own, starting with a desktop + Android player that beats VLC
on hard files. If resources are limited, stopping at Phase 2 or 3 and being excellent there is a real product;
40% of ten features on six platforms is not. See [docs/09-roadmap.md](docs/09-roadmap.md).

## Repository layout

| Path | Contents |
|---|---|
| [`docs/`](docs/) | Architecture, research, and the compatibility specification (00–13 + ADRs) |
| [`crates/`](crates/) | The shared Rust core — see below |
| [`conformance/`](conformance/) | The machine-readable corpus proving docs 11–13 |
| [`native/`](native/) | LGPL-only build recipes for FFmpeg / mpv / libplacebo (ADR-0002) |
| [`ci/`](ci/) | Blocking gates, incl. `license-gate.sh` |

## Status

**Phase 0 in progress.** Research and architecture complete; the shared core that everything else
depends on is implemented and tested.

```bash
cargo test --workspace     # 78 tests, incl. 8 property tests over the playback ladder
ci/license-gate.sh         # ADR-0002 licence posture
python3 conformance/runner/coverage.py
```

| Crate | What it is | Tests |
|---|---|---|
| [`lumen-model`](crates/lumen-model) | Containers, codecs, streams, colour/HDR, remux carriage rules | 24 |
| [`lumen-caps`](crates/lumen-caps) | Client and **sink-level** capability model | 5 |
| [`lumen-playback`](crates/lumen-playback) | **The playback decision ladder** and track auto-selection | 21 + 15 |
| [`lumen-identity`](crates/lumen-identity) | Move-surviving content sketch | 12 |

The ladder is deliberately the first thing built: ADR-0004 makes it the one piece that must never
diverge across clients, and its property tests found six real bugs on their first run.

Remaining Phase 0 spikes and their status: [docs/09-roadmap.md](docs/09-roadmap.md) §2.
See [CONTRIBUTING.md](CONTRIBUTING.md) for the four rules that govern changes here.
