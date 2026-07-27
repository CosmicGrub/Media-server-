# 09 — Roadmap, Effort & Team

## 1. The sequencing principle

Every phase ships something a real person would use on its own. No phase is "infrastructure." If a phase's output
isn't usable, it's scoped wrong.

The order follows your own instinct — **player first**:

```
P0 Spikes → P1 The Player → P2 The Library → P3 The Server → P4 Ecosystem → P5 Everywhere → P6 Agent
   3 wks      3 months        2 months        3 months        3 months       4 months      2 months
```

Note the agent is **last**. It's the feature that most depends on everything else being solid, and the one that
teaches you the least if built early. (Except the embedding-based semantic search, which lands in P2 because it needs
no LLM.)

## 2. Phase 0 — De-risking spikes (3 weeks, before committing to anything)

Run these in parallel. Each answers a question that could invalidate the architecture. **Time-box hard; a spike that
overruns is itself the answer.**

| # | Spike | Question it answers | Success criterion |
|---|---|---|---|
| S1 | libmpv render API into a Tauri v2 window on Win/mac/Linux | Can we composite a WebView UI over hardware video without tearing or lag? | 4K HDR playback at full frame rate with a responsive HTML OSD on all three |
| S2 | libmpv + UniFFI core on Android, `SurfaceView`, `mediacodec` | Does the Rust-core + native-player model work on Android? | 4K HEVC Direct Play, HW decode confirmed |
| S3 | libmpv XCFramework on iOS + tvOS, Metal render | Is Apple viable at all technically? | Same file playing on an iPhone and an Apple TV |
| S4 | **LGPL-only build of FFmpeg + mpv + libplacebo for all 6 targets** | Is the license posture achievable without losing needed features? | Reproducible build recipes, license gate green, conformance corpus items 1–6 pass |
| S5 | Audio passthrough: TrueHD + DTS-HD MA to a real AVR on Windows, Linux, Android | Does the headline remux promise actually work? | AVR display reads "Dolby TrueHD / Atmos" and "DTS-HD MA" |
| S6 | Scanner throughput: walk + probe 10k files on a spinning disk and an SMB mount | Are the perf targets realistic? | < 5 min local, < 20 min SMB, tunable concurrency |
| S7 | Wasmtime plugin host: a TMDB provider as a Wasm component | Is the plugin model practical? | Metadata fetched through the sandbox, allowlist enforced |
| S8 | 🔴 Legal review kickoff: App Store + LGPL, Dolby/DTS decoder distribution | Is the iOS target and the business model viable? | Written opinion or a clear "needs restructuring" |

### Phase 0 status

| Spike | Status | Artifact |
|---|---|---|
| S1 desktop compositing | not started | needs a GPU runner |
| S2 Android core + libmpv | not started | needs the NDK |
| S3 Apple XCFramework | not started | **blocked on S8** — do not write iOS code before counsel reports |
| **S4 LGPL-only build** | **recipes written, build pending** | [`../native/`](../native/) — `ffmpeg.config`, `mpv.config`, [`../ci/license-gate.sh`](../ci/license-gate.sh) (passing, with a verified negative test), [`../deny.toml`](../deny.toml) |
| S5 audio passthrough | not started | needs a physical AVR |
| S6 scanner throughput | not started | needs a corpus |
| S7 Wasm plugin host | not started | — |
| S8 legal review | **not started — start this week** | this gates S3 and the whole Apple target |

**Shipped ahead of the spikes**, because it is the piece everything else depends on and it needed no
hardware ([`../crates/`](../crates/)):

| Crate | What it is | Tests |
|---|---|---|
| `lumen-model` | Containers, codecs, streams, colour/HDR, remux carriage rules | 24 |
| `lumen-caps` | Client + **sink-level** capability model (gap G2) | 5 |
| `lumen-playback` | **The decision ladder** and track auto-selection — ADR-0004's one implementation | 21 + 15 |
| `lumen-identity` | Move-surviving content sketch — decision D5 | 12 |

The ladder's property tests found **six real bugs** on first run, including plans that emitted a
container the client could not open, an upscaling transcode, and a T4 with no explanation. That is
the argument for writing the referee independently of the implementation.

**Kill criteria.** If S1 fails on all three desktops → switch desktop to Qt 6. If S3 fails → iOS ships with VLCKit or
not at all in v1. If S4 fails → decide consciously to be a GPL product and drop the App Store. If S5 fails → the
positioning changes and you must say so before writing marketing copy.

## 3. Phase 1 — The Player (3 months) → **first public release**

**Deliverable: a desktop + Android player that beats VLC on hard files and looks like it was designed this decade.**
No server. No accounts. Open a file or a network share and play it.

- `lumen-core`: playback session, track selection, the decision ladder, file identity, SQLite store
- `lumen-mpv`: the libmpv binding + render bridge
- Desktop shell (Tauri): file/folder open, network browse (SMB/NFS/WebDAV/SFTP), full playback UI, track selection,
  subtitle controls, audio device + passthrough config, **the Playback Report overlay**
- Android shell: phone + Android TV, same feature set
- Shader/enhancement presets (Anime4K, CAS, deband) — an early, visible differentiator
- Frame-rate/display-mode matching
- **The Universal Play Guarantee ([`11`](11-compatibility-charter.md) G0–G2) fully implemented.** The ladder,
  tier model, reason taxonomy, and track auto-selection already exist in
  [`../crates/lumen-playback`](../crates/lumen-playback). What remains: content-probing over extension trust, the
  ten-rung recovery ladder ([`12`](12-container-conformance.md) §5), the full MKV and MP4 feature surface, and
  64-bit rational timestamps end to end
- **Conformance corpus green on both platforms in CI** — the full seed set in [`../conformance/corpus.yaml`](../conformance/corpus.yaml),
  not just the 20-file subset. This is the release gate, not a stretch goal.
- LGPL build pipeline + license gate + SBOM + Legal screen

**Why this ships alone:** "mpv's engine with a good UI, on desktop and Android TV, that plays remuxes bit-perfectly
and tells you what it's doing" is a product people will use today. It also builds the hardest thing first.

## 4. Phase 2 — The Library (2 months, local-only)

**Deliverable: the player now has a beautiful local library. Still no server.**

- Scanner: Discover → Identify → Probe → Match → Enrich → Materialize, all six stages, job queue, watchers
- NFO/sidecar read+write; local artwork conventions
- TMDB + TheTVDB + OpenSubtitles providers (as Wasm plugins from the start — dogfood the plugin system)
- Artwork pipeline, BlurHash, trickplay thumbnails
- Library UI: posters, collections, filters, sort, `needs_review` queue
- FTS search + **subtitle dialogue search** + optional local embedding index (§5.1 of the agent doc, no LLM)
- Watch state, resume, continue-watching

## 5. Phase 3 — The Server (3 months)

**Deliverable: multi-device, multi-user, streaming.**

- `lumen-server`: axum API, OpenAPI, WebSocket events, auth (local + passkeys + device-code TV login)
- Users, profiles, parental controls, per-user libraries
- Capability negotiation + the shared ladder running server-side
- Transcoding: FFmpeg workers, CMAF/LL-HLS/DASH, hardware accel on all five vendor paths, HDR tone mapping
- Watch-state CRDT sync, offline downloads
- Discovery: mDNS, DLNA/UPnP server, Chromecast sender + receiver
- Remote access: NAT-PMP/UPnP-IGD, Tailscale docs, optional relay
- Packaging: Docker, systemd, Synology/QNAP, unRAID
- **Jellyfin/Emby API shim** — instant compatibility with existing clients and the *arr stack

## 6. Phase 4 — Ecosystem (3 months)

- Wasm plugin host hardened; signing; registry; permission UI; developer tooling and templates
- Theme/skin system (declarative) + shader pack format
- Web PWA client (MSE/CMAF + WASM libass + WebCodecs enhancement path)
- Live TV/DVR: HDHomeRun + IPTV/XMLTV + recording rules
- Music mode: gapless, ReplayGain, album/artist views, playlists, scrobbling
- Trakt/Last.fm sync, notification plugins
- Intro/credit detection, loudness normalization
- Migration importers from Plex, Jellyfin, Emby, and Kodi (watch state + metadata). **High-leverage adoption feature.**

## 7. Phase 5 — Everywhere (4 months)

- iOS/iPadOS/tvOS (pending the legal answer from S8)
- Samsung Tizen + LG webOS (reuse the web player)
- Steam Deck / gamepad mode
- Native macOS app (optional)
- Roku (BrightScript — a separate mini-project; only if the data says the audience is there)
- Watch Together / synced playback
- Accessibility pass: screen readers, high-contrast themes, audio description track handling, dyslexia-friendly
  subtitle presets, full keyboard navigation. **Do not defer this further.**
- i18n/l10n framework and the first 10 languages

## 8. Phase 6 — The Agent (2 months)

- `lumen-agent-mcp` tool surface + policy engine + audit log
- Local backend (llama.cpp/Ollama) and cloud backend (Claude API) with consent flow
- Features in priority order: operator triage (§5.4) → match disambiguation (§5.2) → curation (§5.3) →
  quality audit (§5.5) → NL config (§5.6) → subtitle work (§5.7)
- Ships **off by default**, in Settings → Labs

## 9. Effort estimate — honest numbers

| Phase | Scope | Engineer-months (experienced team) | Solo-developer equivalent |
|---|---|---|---|
| P0 Spikes | 8 parallel spikes | 3 | 3–4 months |
| P1 Player | 2 platforms, full playback | 18 | 12–18 months |
| P2 Library | Scanner + metadata + UI | 10 | 8–12 months |
| P3 Server | API, auth, transcode, sync | 20 | 15–24 months |
| P4 Ecosystem | Plugins, web, live TV, music | 18 | 15–24 months |
| P5 Everywhere | Apple + TV platforms + a11y | 24 | 24–36 months |
| P6 Agent | Agent + tooling | 6 | 4–6 months |
| **Total to "comprehensive"** | | **~99 engineer-months ≈ 8.3 engineer-years** | **6–10 calendar years solo** |

Plus ongoing: ~30% of capacity on maintenance, device quirks, provider API churn, and support once you have users.

**If you're solo or a small team, the correct move is to stop at Phase 2 or 3 and be excellent there.** "The best
local player + library on desktop and Android" is a real, defensible product. "40% of Plex on six platforms" is not.

## 10. Team shape (if funded)

| Role | Count | Focus |
|---|---|---|
| Media/AV engineer | 2 | libmpv, FFmpeg, codecs, audio passthrough, HDR — the hardest and rarest skill set. Hire first. |
| Rust core | 2 | core, server, scanner, plugin host |
| Android | 1–2 | phone + TV |
| Apple | 1–2 | iOS/tvOS/macOS |
| Frontend | 2 | web + Tauri UI + design system |
| Designer | 1 | this is a product where design is a differentiator, especially the 10-foot UI |
| QA / device lab | 1 | the conformance corpus, the physical device rack, release validation |
| DevRel / community | 1 | from Phase 4 — the plugin ecosystem needs a human |
| **Total** | **11–13** | |

## 11. Milestone definition of done

Every phase must satisfy all of these before it's called done:
1. **Conformance corpus at expected tier or better on every shipped platform** ([`../conformance/`](../conformance/)).
   No waivers. A platform limitation is expressed as a documented `overrides:` entry matching the published
   capability matrix ([`04`](04-platform-strategy.md) §7) — never as a skipped vector.
2. Non-functional targets from [`02-architecture.md`](02-architecture.md) §7 met and tracked in CI
3. License gate green, SBOM published, Legal screen accurate
4. Zero known data-loss bugs
5. Accessibility smoke test passes (keyboard + screen reader on one primary flow)
6. Docs: user-facing guide + API reference regenerated
7. A clean-machine install-to-playing test in under 5 minutes, timed

## 12. The things most likely to sink this

| Risk | Mitigation |
|---|---|
| **Scope** (the #1 killer) | The phase gates above. Nothing from a later phase enters an earlier one. Write it down and enforce it. |
| **Apple legal** | Spike S8 in week 1, not month 12 |
| **Audio passthrough rabbit hole** | S5 early; maintain a device-quirks table; set expectations publicly (the capability matrix in the app) |
| **libmpv integration friction on one platform** | Documented fallbacks per platform (Qt, VLCKit, media_kit) decided in P0, not improvised later |
| **Metadata provider costs on monetization** | Provider-as-plugin + user-supplied keys from commit one |
| **Building a second *arr stack** | Explicit non-goal. Integrate, don't absorb. |
| **Burnout** | The estimate is real. Plan for a decade or plan for a smaller product. Choose deliberately. |
