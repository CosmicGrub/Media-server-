# 05 — Server, Library, Scanner, Metadata & Streaming

## 1. Server technology choice

**Recommendation: Rust + `axum` + `tokio` + `sqlx`/SQLite (WAL), Postgres optional.**

| Option | For | Against | Verdict |
|---|---|---|---|
| **Rust + axum** | Same language as the core → zero FFI between server and domain logic; ~30–60 MB idle; safe against hostile media metadata; excellent FFmpeg FFI | Smaller hiring pool; longer compile times | ✅ **Chosen** |
| Go + echo/chi | Fast to write, easy hiring, great concurrency, single binary | Second language; cgo boundary to the core is a real tax; GC pauses irrelevant but memory floor higher | Strong alternative if team velocity matters more than elegance |
| .NET 9 (Jellyfin's stack) | You could fork/borrow Jellyfin subsystems directly; excellent tooling | Heavier runtime; third language; inherits the architecture you're trying to improve on | Only if you intend to fork Jellyfin |
| Node/TS | Shares language with web UI | Wrong tool for a file-and-process-heavy always-on daemon | ✗ |

The server is **optional**. It must be possible to run the player with no server at all, and the server must be
runnable headless with no UI. Package it as: Docker/OCI image (primary), static binaries, systemd unit,
Synology/QNAP packages, unRAID template, Home Assistant add-on, Helm chart.

## 2. Data model

Entity-per-concept, with `MediaSource` deliberately separate from the logical item — one movie can have a 4K remux, a
1080p encode, and a downloaded 720p copy.

```
Library ──┬── Item ────────┬── MediaSource ──┬── MediaStream (video/audio/sub/attachment)
          │  (Movie,       │  (a file or     │
          │   Series,      │   disc folder)  └── Chapter
          │   Season,      │
          │   Episode,     ├── ItemImage (poster/backdrop/logo/thumb/banner/clearart/disc)
          │   Album,       ├── ItemPerson ──► Person (actor/director/writer, role, order)
          │   Track,       ├── ItemGenre / ItemTag / ItemStudio
          │   Photo,       ├── ExternalId (tmdb/imdb/tvdb/musicbrainz/anidb/tvmaze/…)
          │   Book,        ├── Extra (trailer/behindthescenes/deleted/featurette/short)
          │   Collection)  └── IntroMarker / CreditMarker / AdMarker
          │
          ├── UserData (per user × item: played, position, favorite, rating, playCount)
          ├── PlaySession (live) / PlayHistory (append-only, for the agent + recommendations)
          └── ScanJob / MetadataJob / TranscodeJob (durable queue rows)
```

Rules:
- **`UserData` is never keyed by path.** It keys on `item_id`, and `item_id` survives file moves (§3).
- **Provider responses are cached raw** in a `provider_cache` table keyed by `(provider, endpoint, params_hash)` with
  a TTL. Re-matching must never re-hit the network for data you already have.
- **Every mutation is journalled** into an `item_revision` table with actor (`user`, `scanner`, `provider`, `agent`)
  and a diff. This gives you undo, an audit trail, and the ability to let the AI agent propose changes safely.
- **Field-level locks.** If a user edits a title, that field is locked and no provider may overwrite it. Kodi and
  Jellyfin both learned this the hard way.
- Full-text search via SQLite **FTS5** (or Postgres `tsvector`); optional vector index for semantic search (§10).

## 3. File identity — solving the "I moved my files" problem

The single most valuable robustness feature. **Do not key anything user-facing on path.**

```rust
struct FileIdentity {
    // Fast path — cheap, changes on move across filesystems
    device_id: u64,
    inode: u64,             // FileIndex on Windows, fileID on APFS
    size: u64,
    mtime_ns: i128,

    // Stable path — survives rename, move, and remount
    // xxh3-128 over: first 1 MiB, middle 1 MiB, last 1 MiB, plus size
    content_sketch: u128,

    // Optional, opt-in — survives remux/re-encode? No. Survives nothing more than sketch,
    // but proves byte-identity for dedupe and integrity checks.
    full_hash: Option<u128>,
}
```

Matching algorithm on scan:
1. Path unchanged + (size, mtime) unchanged → **skip entirely** (this is 99.9% of files on a rescan).
2. Path unchanged, size/mtime changed → re-probe.
3. Path gone, a new path has the same `content_sketch` → **it moved.** Update the path, keep the item, keep all user
   data. Log it.
4. Path gone, no sketch match → mark `missing` with a grace period (default 7 days, configurable). **Never delete
   user data on a missing file** — network mounts go away all the time. This alone eliminates the most common
   catastrophic-loss complaint about every competitor.
5. New path, no sketch match → new item, run the full pipeline.

Compute `content_sketch` during the Probe stage (you're already reading the file for `ffprobe`); it costs ~3 MiB of
reads regardless of file size. Store it indexed.

**Bonus:** the sketch gives you free duplicate detection and "you have the same movie in 4K and 1080p" grouping.

## 4. Scanner architecture

Six stages, each an independent consumer of a durable job queue. Learn from Jellyfin here: their scanner couples
filesystem enumeration into the `Folder` entity via `LibraryManager`, and the resulting design is under active
refactor precisely because that coupling makes it slow and hard to parallelize.

```
┌─────────┐  ┌──────────┐  ┌────────┐  ┌────────┐  ┌────────┐  ┌──────────────┐
│Discover │─►│ Identify │─►│ Probe  │─►│ Match  │─►│ Enrich │─►│ Materialize  │
└─────────┘  └──────────┘  └────────┘  └────────┘  └────────┘  └──────────────┘
   walk +      FileIdentity   ffprobe     title →     provider    artwork, thumbs,
   watch       + dedupe       streams     ext. IDs    fetch       chapters, intro
                                                                  detection, BIF,
                                                                  subtitle extract
                        ▲                                              │
                        └───── item is PLAYABLE from here ─────────────┘
```

### 4.1 Discover
- Parallel directory walk (`jwalk`/`ignore` crate) with a bounded worker pool. **I/O concurrency must be tunable per
  library** — 32 threads on NVMe is great, 32 threads on a spinning USB drive or an SMB mount is a disaster. Default
  to 4 for network paths, `min(8, cores)` for local.
- **Real-time watching**: `inotify` (Linux), `FSEvents` (macOS), `ReadDirectoryChangesW` (Windows), with a debounce
  window (default 30 s) so a 60 GB file copy triggers one scan, not four hundred.
- **Network mounts don't emit events reliably.** Fall back to a configurable polling interval, and support
  `.lumen-scan` trigger files and a webhook/CLI `scan --path` for *arr-stack integration (this is how power users
  actually want it wired).
- Ignore rules: `.nomedia`, `.ignore`, user globs, `@eaDir`, `.DS_Store`, `#recycle`, `lost+found`,
  `$RECYCLE.BIN`, sample files (`*sample*` under a size threshold), `.grab`/`.tmp`/`.partial`/`.!qB` in-progress
  downloads. **Never index a file that is still growing** — check size stability across two polls.
- **Recursive by design**, with depth limits and symlink-loop detection.
- Disc structures (`BDMV/`, `VIDEO_TS/`) are recognised as *one item*, not hundreds of `.m2ts` files. Same for
  multi-part movies (`movie-cd1.mkv`, `-part1`, `.stackable`) and multi-episode files (`S01E01-E02`).

### 4.2 Identify
Compute `FileIdentity`, dedupe against existing rows, decide new/moved/changed/unchanged. Cheap; runs at walk speed.

### 4.3 Probe
`libavformat`/`ffprobe` (prefer in-process `libavformat` via `ffmpeg-next` for speed; shell out for isolation from
malformed-file crashes — **recommend shelling out**, in a sandboxed subprocess, because a crash here must not take
down the server).

Extract: container, duration, overall bitrate, every stream (codec, profile, level, resolution, frame rate + exact
rational, pixel format, bit depth, color primaries/transfer/matrix, HDR metadata (`mastering_display`,
`content_light_level`, DV RPU presence and profile), audio codec/channels/layout/sample rate/bit depth/language/
title/flags (`default`, `forced`, `hearing_impaired`, `visual_impaired`, `original`, `commentary`), subtitle streams
and flags, attachments (fonts!), chapters, embedded cover art.

**After Probe, the item is playable.** Surface it in the UI immediately with a filename-derived title. Users should
never wait for TMDB to watch a file they just added.

### 4.4 Match
Filename/path → external IDs. This is where every product is mediocre and where a small amount of care pays off.

1. **Sidecar first.** If a `.nfo` exists (Kodi format), or an embedded `tmdbid`/`imdbid` tag, or a
   `{tmdb-12345}`/`{imdb-tt1234567}`/`[tvdbid-999]` token in the filename or folder name — **use it, stop, done.**
   This is how power users pin matches and it must be honoured absolutely.
2. **Structured parse.** Port/borrow the `guessit`/`parse-torrent-title`/`anitomy` approach: extract title, year,
   season/episode (all the forms: `S01E01`, `1x01`, `101`, `Episode 1`, absolute numbering, `Part 2`, date-based
   `2024-01-15`), edition (`Director's Cut`, `Extended`, `Theatrical`, `Remux`, `IMAX`), resolution, source
   (`BluRay`/`WEB-DL`/`HDTV`/`Remux`), codecs, release group, language tags. Do this as a **layered token-stripping
   parser**, not one giant regex.
3. **Folder context is stronger than filename.** `/Movies/Blade Runner (1982)/br.2049.mkv` — the folder wins.
4. **Rank candidates**, don't take the first result: weighted score over title similarity (normalized, accent-folded,
   article-stripped, romanization-aware), year proximity, runtime proximity (you have the real duration from Probe —
   nobody uses this and it's the single strongest disambiguator), popularity, language, and the number of files in
   the same folder.
5. **Ambiguity is a first-class state.** If the top two candidates are within a confidence delta, mark
   `needs_review` and surface a review queue in the UI. Do not silently pick. (This is also the AI agent's best
   job — see [`07-ai-agent.md`](07-ai-agent.md) §5.2.)
6. Anime needs **AniDB/AniList** and absolute-episode-number mapping (the TVDB↔AniDB mapping problem). Support it
   via provider plugins; don't hardcode.

### 4.5 Enrich
Provider plugins fetch metadata and artwork. See §5. All network I/O here, all rate-limited, all cached, all
retryable with exponential backoff, all failure-isolated (a dead provider degrades metadata, never blocks playback).

### 4.6 Materialize
- Download and normalize artwork (§6).
- Generate **trickplay/BIF thumbnails** for scrub previews (sprite sheets at ~10 s intervals; use hardware decode).
- Extract embedded subtitles to sidecar files for fast client access, and **attached fonts** for libass.
- **Chapter thumbnails.**
- **Intro/credit detection**: audio fingerprint (chromaprint over the first ~15 min) compared across episodes in a
  season to find the common segment. This is Plex's "Skip Intro"; it's very achievable and very loved.
  Also detect black-frame/silence boundaries for credits.
- **Loudness measurement** (EBU R128) stored as metadata for normalization without re-encoding.
- **Subtitle indexing** for search (§10).

Each of these is an independently schedulable, cancellable, low-priority background job with a configurable
concurrency and a "only run when idle / between 2am and 6am" scheduler. Do not saturate a NAS during dinner.

## 5. Metadata providers

**Every provider is a plugin.** ([`06-plugin-system.md`](06-plugin-system.md).) This is a licensing requirement as
much as an architectural one — see §5.2.

### 5.1 Providers to ship or support
| Domain | Providers |
|---|---|
| Film | TMDB, IMDb (via TMDB IDs; scraping IMDb directly violates their terms), OMDb, Trakt, Letterboxd (lists), MPAA/BBFC ratings |
| TV | TMDB, TheTVDB, TVmaze, Trakt |
| Anime | AniDB, AniList, MyAnimeList, Kitsu, plus the TVDB↔AniDB episode mapping tables |
| Music | MusicBrainz (+ AcoustID/Chromagraph fingerprinting), Discogs, Last.fm, Deezer/Spotify (art only, check terms) |
| Books/Audiobooks | Open Library, Google Books, Audible (metadata via Audnexus-style community APIs) |
| Subtitles | OpenSubtitles, Subscene-likes, Addic7ed, local extraction |
| Artwork | Fanart.tv, TheAudioDB, TMDB images, ThePosterDB (check terms), embedded cover art |
| Chapters/Intros | ChapterDB, MediaChapters, self-computed |
| Local | **NFO/sidecar reader-writer (Kodi format)** — must be a first-class two-way provider |

### 5.2 The licensing tripwire (see [`08-legal-licensing.md`](08-legal-licensing.md))
- **TMDB** is free for non-commercial use with attribution; commercial use requires a license obtained by contacting
  TMDB. Required notice: *"This product uses the TMDb API but is not endorsed or certified by TMDb"*, plus
  attribution in an About/Credits section.
- **TheTVDB** is tiered by company revenue: free under $50k/yr (with attribution and a direct link to TheTVDB.com
  shown to end users), $1,000/yr for $50k–$250k, $10,000/yr for $250k–$1M, custom above that.
- **Consequence:** if you ever monetize, your metadata bill starts immediately. Designing providers as swappable
  plugins with per-user API keys means (a) you can ship with no keys and let users supply their own, and (b) you can
  swap to whatever is affordable without touching the core. **Do this from commit one.**

### 5.3 Provider contract
```wit
interface metadata-provider {
    record search-query { kind: item-kind, title: string, year: option<u16>,
                          season: option<u16>, episode: option<u16>,
                          runtime-seconds: option<u32>, external-ids: list<external-id>,
                          language: string }
    search: func(q: search-query) -> result<list<candidate>, provider-error>
    fetch:  func(id: external-id, language: string) -> result<metadata-bundle, provider-error>
    images: func(id: external-id) -> result<list<image-ref>, provider-error>
    // host provides: rate-limited http, cache, secrets — plugin has no raw network
}
```
Providers are **ranked and merged** per field with a user-editable priority list (e.g. "titles from TMDB, ratings
from Trakt, artwork from Fanart.tv"), respecting field locks.

## 6. Artwork

- Types: poster, backdrop/fanart, logo/clearlogo, thumb, banner, clearart, disc, character art, season poster,
  episode still, actor headshot, album cover, artist background.
- Store **content-addressed** on disk (`artwork/aa/bb/<sha256>.jpg`), deduped globally. A 50k library shares a lot of
  actor headshots.
- Generate and cache resized variants on demand (not eagerly), with a WebP/AVIF variant for web clients.
- Compute **BlurHash/ThumbHash** and a dominant-color palette at ingest — enables instant, beautiful placeholder
  loading and adaptive UI theming per item. Cheap, and a huge perceived-polish win.
- Local artwork in the media folder always wins over remote (Kodi convention: `poster.jpg`, `fanart.jpg`,
  `<basename>-thumb.jpg`, `logo.png`, `season01-poster.jpg`).
- Serve with strong `ETag`/`Cache-Control` and support `?width=`/`?format=` transforms.

## 7. Device profiles & capability negotiation

The client posts a **capability document** at session start and again whenever the output sink changes:

```jsonc
{
  "client": { "id": "…", "app": "lumen-android", "version": "1.4.0", "platform": "android/34" },
  "display": { "width": 3840, "height": 2160, "hdr": ["hdr10","hlg","dv_p5"],
               "refresh_modes": [23.976, 24, 25, 30, 50, 59.94, 60], "can_switch_mode": true },
  "video": [ { "codec":"hevc","profiles":["main","main10"],"max_level":"5.1","max_bitrate":120000000,
               "hw": true, "max_width":3840,"max_height":2160 }, … ],
  "audio_sink": {                      // ← re-probed on every device change, NOT static
      "device": "HDMI (Denon AVR-X3800H)",
      "encodings": ["pcm_16","pcm_float","ac3","eac3","eac3_joc","dts","dts_hd","truehd","ac4"],
      "max_channels": 8, "sample_rates": [44100,48000,96000,192000],
      "passthrough_available": true, "exclusive_available": true },
  "containers": ["mkv","mp4","ts","webm","m2ts","avi"],
  "subtitles": { "external": ["srt","ass","vtt"], "embedded": ["ass","srt","pgs","vobsub"],
                 "can_render_pgs": true, "can_render_ass": true },
  "network": { "measured_bps": 940000000, "class": "lan" },
  "policy": { "bit_perfect": true, "max_transcode": "none" }
}
```

The server runs the **exact same ladder code** (`lumen-playback`, compiled into the server) that the client would run
locally. One implementation, one behaviour, no drift. The response includes the full `PlaybackReport` with all
rejection reasons.

## 8. Transcoding & streaming

### 8.1 Pipeline
- FFmpeg as **supervised subprocesses**, never in-process. One process per session, killed hard on client
  disconnect, with a watchdog and an orphan reaper. A transcoder crash must never affect the server.
- **Segmented output**: CMAF/fMP4 segments served as **LL-HLS** (required for Apple) and **DASH** from the same
  segment set. `EXT-X-INDEPENDENT-SEGMENTS`, proper `#EXT-X-MAP`.
- **Seek-ahead handling**: transcode from the seek point rather than throwing away the session; keep an LRU of
  already-produced segments so a rewind is free.
- **Throttling**: pause the encoder when the client is sufficiently buffered (Jellyfin's throttler idea) — but make
  it adaptive, not a fixed sleep.
- **Parallel segment transcoding** across workers for the "optimize/pre-transcode a version" job (not for live
  playback, where ordering matters).

### 8.2 Hardware acceleration matrix
| Vendor | API | Decode | Encode | Notes |
|---|---|---|---|---|
| NVIDIA | NVDEC/NVENC (`cuda`) | H.264, HEVC, VP9, AV1 (Ampere+) | H.264, HEVC, AV1 (Ada+) | Consumer cards have a concurrent-session limit; patched drivers exist but shipping that is a licence violation — document, don't automate |
| Intel | QSV / VAAPI | H.264, HEVC, VP9, AV1 (Arc/Xe2+) | same | Best price/perf for a home server; Arc A310 is the enthusiast pick |
| AMD | VAAPI / AMF | H.264, HEVC, AV1 (RDNA3+) | same | Quality historically behind NVENC/QSV |
| Apple | VideoToolbox | H.264, HEVC, ProRes, AV1 (M3+) | H.264, HEVC, ProRes | Excellent perf/watt |
| Raspberry Pi / ARM SBC | V4L2 M2M | H.264 (+HEVC on Pi5) | limited | Fine for one 1080p stream |

**HDR tone mapping during transcode** must be hardware-accelerated (OpenCL/Vulkan/libplacebo) — the software `zscale`
path is 10–20× slower and will fail real-time on 4K. Getting HDR→SDR tone mapping right in transcode is a common
source of "washed out / grey" complaints; use a proper BT.2390 curve, not a naive clip.

### 8.3 Quality policy
- Bitrate ladders per resolution with a **CRF-first, bitrate-capped** approach rather than fixed bitrates.
- Never upscale. Never increase bitrate above source.
- Preserve HDR when the target supports it; tone-map only when it doesn't.
- Audio: prefer copy → then E-AC3 (keeps 5.1, widely supported) → then AAC stereo. Never go straight to stereo AAC.

## 9. Sync, multi-user, and offline

- **Users, profiles, and managed (child) users**, per-user library visibility, parental controls by rating and by
  tag, PIN-locked profiles, per-user language/subtitle preferences and playback policies.
- **Watch state as a CRDT** (LWW-register for `position`, G-counter for `play_count`, OR-set for `favorites`) so
  offline devices merge deterministically. Naive last-write-wins across three devices loses data and users notice.
- Sync targets as plugins: **Trakt**, Simkl, Letterboxd, Last.fm/ListenBrainz scrobbling, Kodi (via the same API).
- **Continue Watching** logic that is actually correct: threshold-based completion (default: >90% or <2 min left
  = watched), next-episode surfacing, cross-series grouping, and a manual "remove from continue watching".

## 10. Search

Three tiers, all local:
1. **FTS5/tsvector** over titles, people, genres, studios, taglines, overviews — instant, typo-tolerant with
   trigram fallback.
2. **Subtitle full-text index**: extract every subtitle track's text with timestamps into an FTS table. "Find the
   episode where someone says 'I am the one who knocks'" → jump to 34:12. Genuinely differentiating, no AI required.
3. **Semantic/vector search** (optional, opt-in): local sentence-embedding model (e.g. a small ONNX model like
   `bge-small` or `all-MiniLM-L6-v2`) over overviews and subtitle chunks, stored in `sqlite-vec`/`usearch`. Enables
   "the one where they get stuck in an elevator". Runs on CPU, no cloud, ~100 MB model. This is the substrate the AI
   agent uses ([`07-ai-agent.md`](07-ai-agent.md) §5.1).

## 11. Live TV & DVR

Backend abstraction (steal Kodi's PVR shape), implemented as plugins:
- **HDHomeRun** (ATSC/DVB network tuners) — the easy, popular one
- **IPTV**: M3U playlists + XMLTV EPG, with stream-health monitoring and auto-failover between duplicate channels
- **Tvheadend / NextPVR / TVHeadend-compatible** backends
- **ATSC 3.0** where tuners expose it
- DVR: series recording rules, conflict resolution across tuners, padding, commercial detection
  (Comskip-equivalent) as a post-process job, automatic transcode-to-archive.

## 12. Discovery, casting, and remote access

| Feature | Approach |
|---|---|
| LAN discovery | mDNS/DNS-SD (`_lumen._tcp`) + a legacy UDP broadcast for constrained clients |
| DLNA/UPnP **server** | Expose the library to any smart TV/console. Cheap, huge reach. |
| DLNA/UPnP **renderer** | Let phones push to the desktop app |
| Chromecast | Cast sender in every client + a custom HTML **receiver** app reusing the web player |
| AirPlay 2 | Sender on Apple platforms (via AVPlayer path); receiver is not licensable — skip |
| Remote access | 1) UPnP-IGD/NAT-PMP auto port mapping, 2) optional relay service (self-hostable), 3) **first-class Tailscale/WireGuard integration** (documented + one-click where possible), 4) plain reverse proxy docs for nginx/Caddy/Traefik |
| Auth for TVs | OAuth device-code flow: TV shows a code, user enters it on a phone. Never make anyone type a password with a D-pad. |

## 13. API design

- **OpenAPI 3.1** as the single source of truth; generate TS, Kotlin, Swift, Rust, and Python clients in CI.
- Versioned (`/api/v1/`), additive-only within a major version.
- **WebSocket** channel for: playback state, scan progress, job status, library invalidations, agent messages.
- Consistent pagination (cursor-based), consistent error envelope with stable codes, `Idempotency-Key` on mutations.
- **Jellyfin/Emby API shim** (optional plugin): implement enough of their REST surface that existing third-party
  clients and the *arr stack "just work" against your server. This is a *massive* adoption lever for near-zero
  ongoing cost — it makes migration a config change instead of a project.
