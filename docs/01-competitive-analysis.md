# 01 — Competitive Analysis: What to Take, What to Avoid

The brief is "all the best parts of Plex, Jellyfin, Kodi, VLC." That is only actionable if you can name the parts.
This document does that, then names the gaps that none of them fill — which is where the product actually lives.

## 1. Teardown

### 1.1 VLC — the format-tolerance benchmark
**Architecture:** C core, module system (`libvlccore` + hundreds of `.so`/`.dll` plugin modules), each an access,
demux, decoder, filter, or output module. libVLC is the embeddable API; VLCKit is the Apple binding.

| What it does better than anyone | Why |
|---|---|
| Plays damaged, truncated, mis-muxed files | Extremely permissive demuxers, aggressive recovery paths, no "validate then reject" |
| Codec breadth including ancient/obscure | 20+ years of accumulated demuxers and decoders |
| Zero-config: open file, it plays | No library, no server, no account |
| Genuinely everywhere | Desktop, mobile, tvOS, consoles |

| What to avoid |
|---|
| UI/UX — decades of accreted dialogs; the mobile apps are functional, not delightful |
| Module system complexity — plugin ABI is C and tightly version-coupled |
| No library, no metadata, no multi-user — by design |
| HDR/tone-mapping is behind mpv/libplacebo |

**Take:** the philosophy that the player never refuses a file, and the fallback ladder that makes it true.

### 1.2 mpv — the quality benchmark (not in your list, but it should be)
You named VLC, but the technically superior embeddable engine is **mpv**. It is the reference for:
- `gpu-next` renderer built on **libplacebo**: correct HDR tone mapping (BT.2390, spline, ST2094-40), HDR10+ dynamic
  metadata, Dolby Vision Profile 5/8 (and Profile 7 MEL metadata consumption), dithering, debanding, ICC profiles.
- **User shaders** — Anime4K, FSRCNNX, NNEDI3, ravu, CAS. Nothing in the Plex/Jellyfin/Kodi world touches this.
- Frame-accurate seeking, `--interpolation` with `tscale` for judder-free playback, display-sync.
- libass subtitle rendering that is the de-facto correctness reference for ASS/SSA.
- A clean, stable, thread-safe **client API (libmpv)** with a **render API** (`libmpv/render.h`, current API version
  `MPV_MAKE_VERSION(2, 5)`) that lets you render into your own OpenGL/Vulkan/D3D11 context and draw your own OSD on
  top — which is exactly what an embedded player in a custom UI needs.

**Take:** all of it. This is the core. See ADR-0001.

### 1.3 Kodi — the living-room and local-metadata benchmark
**Architecture:** C++ core, Python 3 addon system, skinning engine (XML + textures), PVR abstraction, SQLite library
DB (`MyVideos*.db`, `MyMusic*.db`), platform "binary addons" for decoders/PVR backends.

| Take | Detail |
|---|---|
| **NFO/sidecar metadata model** | Local `.nfo` XML files and adjacent artwork (`poster.jpg`, `fanart.jpg`, `movie-name-thumb.jpg`) are authoritative when present. This is the correct model: the *filesystem is the source of truth*, the DB is a cache. Portable, git-able, survives server reinstalls. |
| **Disc-structure playback** | BDMV folders, `.mpls` playlists, ISO/UDF images, DVD `VIDEO_TS` — via libbluray/libdvdnav. Essential for a remux-focused product. |
| **Skinning engine** | Full re-theming, not just colors. A real differentiator for enthusiast users. |
| **PVR/Live TV abstraction** | Clean backend interface (Tvheadend, HDHomeRun, IPTV Simple). Copy the shape. |
| **10-foot UI competence** | Focus/D-pad navigation done properly. |

| Avoid | Why |
|---|---|
| Python addon system | No sandbox. An addon can `os.system()` anything. This is the reason for the Wasm decision in [`06-plugin-system.md`](06-plugin-system.md). |
| C++ build system | Legendarily painful cross-platform builds |
| DB schema | Denormalized, version-suffixed tables, hard migrations |
| Client-only model | No server: every device rescans, watch state sync needs MySQL hacks |

### 1.4 Jellyfin — the open server benchmark
**Architecture:** .NET (C#) server, modular API, `BaseItem` hierarchy with `Folder` doing recursive filesystem
enumeration through `LibraryManager`, `IFileSystem`/`ManagedFileSystem` abstraction for path normalization and
cross-platform paths, a resolver pipeline mapping paths → `Movie`/`Series`/`Audio` entities, then metadata providers
enriching them. Web client (React), plus native-ish clients per platform.

| Take | Detail |
|---|---|
| **`DeviceProfile` negotiation** | The client declares codec/container/bitrate/subtitle capabilities; the server decides Direct Play / Direct Stream / Transcode. The *concept* is right. |
| **REST API shape** | Well-documented, OpenAPI-generated clients. Good model for your public API. |
| **Plugin manifest + repository model** | Third-party repo URLs, versioned manifests, in-app browse/install. Copy the UX, replace the runtime. |
| **Hardware acceleration matrix** | Their docs on NVENC/QSV/VAAPI/AMF/VideoToolbox per-codec support are the best public reference. |
| **No account, no cloud, no phone-home** | The core value proposition against Plex. Keep it. |

| Avoid | Why |
|---|---|
| Scanner architecture | Acknowledged pain — scanner refactoring is an active, multi-release meta-project (see jellyfin-meta discussion #125); scanner optimisation was explicitly *not* a focus of 12.0 and is deferred to 13.0. Don't inherit this design; see [`05-server-library.md`](05-server-library.md). |
| Static device profiles | They describe the *device*, not the *current audio sink*. An Android TV box's capabilities change when you switch from TV speakers to an AVR. Probe the sink at playback time. |
| Client fragmentation | Every platform client is a separate codebase with a separate feature set and separate bugs. This is what the shared Rust core exists to prevent. |
| Tight server/transcoder coupling | Transcoding should be a separately schedulable, separately crashable unit. |

### 1.5 Plex — the polish benchmark
| Take | Detail |
|---|---|
| **Onboarding** | Sub-5-minute path from install to watching. Ruthlessly measure this. |
| **Remote access without port-forwarding** | Relay + NAT traversal. Users cannot configure routers. Solve it or lose them. |
| **Unified watch state across everything** | Including offline resume merge. |
| **Family-grade multi-user** | Managed users, parental controls, per-user libraries, sane defaults. |
| **Watch Together / synced playback** | High delight-to-effort ratio. |
| **Rich, curated metadata presentation** | Extras, trailers, cast pages, collections, "similar to". |

| Avoid | Why |
|---|---|
| Cloud dependency for local playback | Repeated outages have blocked local libraries. Architect so the server is fully functional with the WAN cable pulled. |
| Opaque transcode decisions | The #1 user complaint. This is your wedge. |
| Closed plugin ecosystem | Plugins were killed in 2018. |
| Paywalled basics | Hardware transcoding, mobile sync, and skip-intro behind a subscription. |

### 1.6 Infuse (Firecore) — the closest existing analogue
Worth studying carefully because it is *already* "player-first, server-optional, beautiful, Apple-native." It plays
direct from SMB/NFS/WebDAV/cloud without a server, scrapes metadata itself, and optionally federates with
Plex/Jellyfin/Emby. Its weaknesses are your openings: Apple-only, closed, no plugins, no server component of its own,
no Android/PC/web.

## 2. The gap map — where the product actually differentiates

None of the incumbents do these. Each is achievable and each is a headline feature.

| # | Gap | Why nobody does it | Your move |
|---|---|---|---|
| **G1** | **Transparent playback decisions.** Users never know why quality dropped. | Requires plumbing decision provenance from server to UI | An always-available "Playback Report" overlay: source stream detail, chosen path, every rejected path *with the reason*, sink capabilities, hardware decoder in use, dropped frames. One keystroke. |
| **G2** | **True sink-level audio capability probing.** | Hard, per-OS, poorly documented | Query the *actual* HDMI/AVR sink (Android `AudioDeviceInfo.getEncodings()`, WASAPI `IsFormatSupported`, ALSA ELD, CoreAudio streams) and cache per output device, not per app. See [`03-playback-engine.md`](03-playback-engine.md) §5. |
| **G3** | **Shader/enhancement packs in a mainstream media server.** | Plex/Jellyfin/Kodi have no equivalent to mpv user shaders | Ship Anime4K, FSRCNNX, CAS, debanding as one-click "Enhancement Presets" with per-library defaults. Genuinely nothing else has this. |
| **G4** | **Remux-grade correctness as a first-class promise.** | Servers optimise for "works on a Chromecast" | A "Bit-perfect" mode: refuse to transcode, surface exactly what the chain can and cannot do, verify lossless audio is actually reaching the sink. |
| **G5** | **Library identity that survives moves/renames.** | Everyone keys on path | Content-derived identity as a stable secondary key. See [`05-server-library.md`](05-server-library.md) §3. |
| **G6** | **Semantic search over dialogue.** | Requires subtitle indexing + embeddings | "Find the scene where they talk about the buried treasure." Local embedding model, no cloud. Killer app for the AI agent — see [`07-ai-agent.md`](07-ai-agent.md) §5.1. |
| **G7** | **Sandboxed, signed, cross-platform plugins.** | Kodi = unsandboxed Python; Jellyfin = trusted .NET DLLs; Plex = none | Wasm Component Model with capability grants. Plugins run identically on a Synology, a phone, and a browser. |
| **G8** | **Player without a server.** | Plex/Jellyfin require a server; Kodi requires local files | Direct SMB/NFS/WebDAV/SFTP/rclone-remote browsing and scraping from the client, server optional — Infuse's model, on every platform. |
| **G9** | **Disc-structure fidelity.** | Only Kodi does BDMV/ISO properly; Plex/Jellyfin flatten it | Full `.mpls` playlist selection, seamless branching, menu-less "main title" auto-detection, forced-subtitle handling. |
| **G10** | **An optional operator agent.** | Nobody has one | See [`07-ai-agent.md`](07-ai-agent.md). Must be genuinely optional and genuinely useful, not a chatbot bolted to a sidebar. |

## 3. Positioning statement (to keep the team honest)

> **Lumen is the player mpv users would build if they also wanted their family to use it.**
> It plays anything, bit-perfectly, and tells you the truth about what it's doing. The library, the server, the
> plugins, and the agent are things you can turn on — not things you must run.

If a proposed feature doesn't serve that sentence, it goes in Phase 4.

## 4. Sources

- [libmpv client API / render API](https://mpv-player-mpv.mintlify.app/embedding/libmpv) — render API recommended over window embedding; current API version 2.5
- [mpv-examples/libmpv](https://github.com/mpv-player/mpv-examples/tree/master/libmpv) — reference embedding examples
- [Jellyfin Media Library System (DeepWiki)](https://deepwiki.com/jellyfin/jellyfin/2-media-library-system)
- [Jellyfin File System and Library Scanning (DeepWiki)](https://deepwiki.com/jellyfin/jellyfin/2.4-file-system-and-library-scanning)
- [Jellyfin Scanner Refactoring discussion](https://github.com/jellyfin/jellyfin-meta/discussions/125)
- [State of the Fin, 2026-05-24](https://jellyfin.org/posts/state-of-the-fin-2026-05-24/) — scanner work deferred to 13.0
- [mpv HDR Guide 2026](https://carlosfelic.io/misc/mpv-hdr-guide-2026/) and [mpv.conf guide 2026](https://carlosfelic.io/misc/best-mpv-config-2026/)
