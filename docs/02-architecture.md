# 02 — System Architecture

## 1. Guiding principles

1. **The player works with no server.** Server features are additive. Pull the network cable and local playback,
   local library, and local scraping still work.
2. **One core, many shells.** Anything that isn't pixels-and-touch lives in the shared Rust core. Shells are thin.
3. **The media path is sacred.** Nothing optional — plugins, agent, analytics, telemetry — may sit in the decode or
   render path. They observe and configure; they do not intercept.
4. **Every subsystem is separately killable.** Transcoder crash must not take down playback of a Direct Play stream.
5. **Filesystem is the source of truth; the database is a cache.** You must be able to `rm` the DB and rebuild losing
   only performance, and (with sidecars enabled) losing nothing at all.
6. **Every long operation is a resumable job.** Scans, transcodes, artwork fetches, hash passes, intro detection.

## 2. Component map

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                                   SHELLS                                     │
│  Tauri Desktop │ Kotlin/Compose  │  Swift/SwiftUI   │  React PWA  │  TV apps  │
│  (Win/mac/Lin) │ (Android + ATV) │ (iOS/iPadOS/tvOS)│   (web)     │ (Tizen…)  │
└───────┬──────────────┬───────────────────┬───────────────┬──────────────┬─────┘
        │  UniFFI /    │  UniFFI (Kotlin)  │ UniFFI (Swift)│ wasm-bindgen │ REST
        │  napi-rs     │                   │               │  / REST      │
┌───────▼──────────────▼───────────────────▼───────────────▼──────────────▼─────┐
│                        lumen-core  (Rust, no_std-friendly where possible)      │
│                                                                                │
│  ┌────────────┐ ┌────────────┐ ┌─────────────┐ ┌──────────┐ ┌───────────────┐ │
│  │  Playback  │ │ Capability │ │   Library    │ │   Sync   │ │   Sources     │ │
│  │  Session   │ │ Negotiation│ │   Model      │ │  Engine  │ │  (SMB/NFS/    │ │
│  │  State M/C │ │ (D-Play    │ │  (entities,  │ │  (CRDT   │ │  WebDAV/SFTP/ │ │
│  │            │ │  ladder)   │ │   queries)   │ │  watch)  │ │  local/HTTP)  │ │
│  └─────┬──────┘ └────────────┘ └──────┬──────┘ └──────────┘ └───────────────┘ │
│        │                              │                                        │
│  ┌─────▼──────────────────┐   ┌───────▼─────────┐   ┌───────────────────────┐ │
│  │  mpv-rs (libmpv FFI)   │   │  Local store    │   │  Plugin host          │ │
│  │  + render API bridge   │   │  (SQLite/WAL)   │   │  (Wasmtime, optional) │ │
│  └─────┬──────────────────┘   └─────────────────┘   └───────────────────────┘ │
└────────┼───────────────────────────────────────────────────────────────────────┘
         │
┌────────▼──────────────────────────────────────────────────────────────────────┐
│                      NATIVE AV STACK  (per-platform binaries)                  │
│   libmpv (LGPL)  →  FFmpeg (LGPL, --disable-gpl)  →  libplacebo (gpu-next)     │
│   libass · libbluray · libdvdnav · libudfread · zimg · uchardet · rubberband   │
│   HW decode: NVDEC │ QSV │ VAAPI │ D3D11VA │ VideoToolbox │ MediaCodec         │
│   Audio out: WASAPI │ CoreAudio │ ALSA/PipeWire │ AAudio/AudioTrack │ AudioUnit│
└────────────────────────────────────────────────────────────────────────────────┘

                                    ▲  REST/gRPC + WebSocket (optional)
                                    │
┌───────────────────────────────────┴────────────────────────────────────────────┐
│                     lumen-server  (Rust, axum) — OPTIONAL                       │
│                                                                                 │
│  API gateway ─ auth (OIDC/passkeys/device tokens) ─ rate limit ─ audit log      │
│  ┌───────────┐ ┌────────────┐ ┌──────────┐ ┌───────────┐ ┌──────────────────┐  │
│  │  Scanner  │ │  Matcher / │ │ Artwork  │ │  Job      │ │  Streaming        │  │
│  │  (walk +  │ │  Metadata  │ │  cache   │ │  Queue    │ │  (Direct/Remux/   │  │
│  │  watch)   │ │  pipeline  │ │  + CDN   │ │ (durable) │ │  Transcode/HLS)   │  │
│  └───────────┘ └────────────┘ └──────────┘ └───────────┘ └────────┬─────────┘  │
│  ┌───────────┐ ┌────────────┐ ┌──────────┐ ┌───────────┐          │            │
│  │  Users /  │ │ Live TV /  │ │  Plugin  │ │ Discovery │  ┌───────▼────────┐   │
│  │  Profiles │ │ DVR (PVR)  │ │  host    │ │ mDNS/UPnP │  │  ffmpeg worker │   │
│  │           │ │            │ │(Wasmtime)│ │ Cast/AirP │  │  pool (procs)  │   │
│  └───────────┘ └────────────┘ └──────────┘ └───────────┘  └────────────────┘   │
│         │                          │                                            │
│    ┌────▼──────────┐        ┌──────▼──────────┐                                │
│    │ SQLite / Pg   │        │  MCP surface    │◄──── lumen-agent (OPTIONAL,     │
│    │ + object store│        │  (tools, RO by  │      separate process/container)│
│    └───────────────┘        │   default)      │      local LLM or Claude API    │
│                             └─────────────────┘                                │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## 3. Crate / module layout

```
lumen/
├─ crates/
│  ├─ lumen-core/            # facade re-exported to every shell via UniFFI
│  ├─ lumen-playback/        # session state machine, track selection, playback ladder
│  ├─ lumen-mpv/             # libmpv FFI, render-API bridge (GL / Vulkan / D3D11 / Metal)
│  ├─ lumen-caps/            # device + SINK capability probing, DeviceProfile generation
│  ├─ lumen-model/           # entities: Item, MediaSource, Stream, Person, Collection…
│  ├─ lumen-store/           # SQLite (rusqlite/sqlx), migrations, query layer
│  ├─ lumen-sources/         # VFS: local, SMB, NFS, WebDAV, SFTP, FTP, HTTP, rclone-remote
│  ├─ lumen-scan/            # walker, watcher, probe, dedupe, job emission
│  ├─ lumen-match/           # filename parsing, external-ID resolution, ranking
│  ├─ lumen-meta/            # provider abstraction + built-in providers + NFO read/write
│  ├─ lumen-artwork/         # fetch, dedupe, resize, blurhash, palette extraction
│  ├─ lumen-transcode/       # ffmpeg process supervision, segmenting, HLS/DASH packaging
│  ├─ lumen-sync/            # watch-state CRDT, offline merge, downloads
│  ├─ lumen-plugin/          # Wasmtime host, WIT bindings, capability enforcement
│  ├─ lumen-discovery/       # mDNS, SSDP/DLNA, Cast, AirPlay, relay/NAT traversal
│  ├─ lumen-pvr/             # Live TV / DVR backends (HDHomeRun, M3U/XMLTV, Tvheadend)
│  ├─ lumen-api/             # axum routes, OpenAPI, WebSocket events
│  ├─ lumen-agent-mcp/       # MCP server exposing curated tools to the optional agent
│  └─ lumen-ffi/             # UniFFI + napi-rs + wasm-bindgen bindings
├─ shells/
│  ├─ desktop/               # Tauri v2 + shared web UI
│  ├─ android/               # Kotlin, Compose + Compose for TV
│  ├─ apple/                 # Swift, SwiftUI (iOS/iPadOS/tvOS/macOS-native option)
│  └─ web/                   # React PWA (shared design system with desktop)
├─ native/                   # build recipes for libmpv/FFmpeg/libplacebo per platform
├─ plugins/                  # first-party Wasm plugins (TMDB, TVDB, OpenSubtitles, …)
└─ conformance/              # the hard-file corpus + automated playback tests
```

## 4. Key data flows

### 4.1 Playback (the important one)

```
User picks item
  │
  ├─► core: resolve MediaSource(s)  ──► pick best source (quality, availability, local-first)
  │
  ├─► core: probe SINK capabilities NOW (not cached device profile)
  │       audio sink encodings, HDMI ELD, display HDR caps, decoder availability
  │
  ├─► core: build PlaybackPlan via the ladder
  │       1. Direct Play        (byte-for-byte, local or HTTP range)
  │       2. Direct Stream      (remux container only; codecs untouched)
  │       3. Partial transcode  (video untouched, audio converted)  ← or the reverse
  │       4. Full transcode
  │     Each rejected rung records a machine-readable Reason.
  │
  ├─► mpv: configure (hwdec, vo=gpu-next, ao, tone-mapping, shaders, tracks)
  │
  ├─► mpv: loadfile → render into shell-owned surface via libmpv render API
  │
  └─► core: emit PlaybackReport (plan + reasons + live stats) to UI and to server
```

The **Reason** enum is a first-class product feature, not a log line. Every rung rejection carries a structured
reason (`SinkLacksEncoding{codec, sink}`, `NoHardwareDecoder{codec, profile, level}`, `BitrateCeiling{have, want}`,
`ContainerUnsupported{..}`, `SubtitleBurnInRequired{..}`, `DrmRequired{..}`) that the UI renders as plain English.

### 4.2 Scan (see [`05-server-library.md`](05-server-library.md) for detail)

```
Discover ──► Identify ──► Probe ──► Match ──► Enrich ──► Materialize ──► Index
 (walk)      (fs ident)   (ffprobe) (title→ID) (providers) (art,thumbs,   (FTS +
                                                            chapters,      embeddings)
                                                            intro detect)
```
Six independent, idempotent, resumable stages backed by a durable job queue. A failure in Enrich never blocks
Discover. Media becomes playable after **Probe** — the user does not wait for metadata.

## 5. Deployment topologies to support

| Topology | Notes |
|---|---|
| **Standalone player** | No server at all. Local files + network shares browsed directly. Client-side scraping into a local SQLite. This is Phase 1. |
| **Single-box server + clients** | The Plex/Jellyfin default. Server on a NAS/PC, clients everywhere. |
| **Server + separate transcode nodes** | Job queue dispatches transcode segments to worker nodes with GPUs. Serious differentiator for large libraries. |
| **Headless/embedded** | Docker, unRAID, Synology/QNAP packages, systemd. Must idle under ~150 MB RSS with a 50k-item library. |
| **Peer/mesh (later)** | Multiple servers federating libraries without a cloud account. |

## 6. Cross-cutting decisions

| Concern | Decision |
|---|---|
| **IPC / API** | REST (OpenAPI 3.1) as the stable public contract + WebSocket for events. gRPC internally between server and transcode workers. Generate all clients from the spec. |
| **Auth** | Local accounts + passkeys (WebAuthn) first; OIDC for those who want it; per-device long-lived tokens with revocation; PIN-based TV login flow (device-code grant). No cloud account required, ever. |
| **Transport security** | Embedded ACME (Let's Encrypt) with DNS-01 for LAN-only certs; self-signed + pinning fallback for pure-LAN. |
| **Remote access** | Optional relay (like Plex's) *plus* first-class Tailscale/WireGuard integration and UPnP-IGD/NAT-PMP port mapping. Never required for LAN use. |
| **Config** | TOML files that are the source of truth, editable by hand, watched for changes. No config-only-in-DB. |
| **Observability** | `tracing` → structured JSON logs; OpenTelemetry optional; a built-in `/diagnostics` bundle generator that produces a redacted zip for bug reports. |
| **Telemetry** | Off by default, opt-in, fully documented, locally inspectable before sending. |
| **Error taxonomy** | One `LumenError` enum with stable string codes (`LUM-PLAY-0142`) so support and the AI agent can reason about them. |
| **Threading** | `tokio` for I/O; a bounded `rayon` pool for CPU (hashing, image work); mpv keeps its own threads — never call libmpv from an async context without a dedicated blocking bridge. |
| **Database** | SQLite in WAL mode by default (single-writer is fine; the scanner batches). Postgres as an opt-in backend for >250k items or multi-node. Use `sqlx` with compile-time-checked queries and versioned migrations from day one. |
| **Object storage** | Artwork/thumbnails/subtitles on the filesystem in a content-addressed tree (`aa/bb/aabbcc….jpg`), not BLOBs in SQLite. |

## 7. Non-functional targets (set these now, measure in CI)

| Metric | Target |
|---|---|
| Cold start to first frame, local 4K remux, desktop | < 1.5 s |
| Seek latency, 80 GB remux over gigabit SMB | < 400 ms |
| Server idle RSS, 50k items | < 150 MB |
| Full scan, 10k items, spinning disk, no metadata | < 5 min |
| Incremental scan after 1 file added | < 2 s to playable |
| UI frame budget, TV shells | 16.6 ms p99 during browse |
| Direct Play rate on a "typical enthusiast library" | > 95 % across supported clients |
| API p99, library browse of 5k items | < 120 ms |

## 8. What is explicitly *not* in the architecture

- No cloud requirement for any core feature.
- No DRM-protected commercial streaming integration (Netflix/Disney+). It requires Widevine L1/FairPlay
  certification you will not obtain, and it poisons the licensing story. Link out to those apps instead.
- No in-process untrusted code.
- No bundled torrent/usenet client. Integrate with *arr-stack tools via plugins; do not become one.
