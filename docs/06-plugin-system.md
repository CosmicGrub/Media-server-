# 06 — Plugin System

## 1. Why this deserves its own document

Your brief says plugins should be "downloadable, or included already, to make this fully fledged." Two things follow:

1. **Batteries included.** The default install must already do everything a normal user needs — TMDB metadata,
   subtitles, artwork, Trakt sync. A plugin system that's required to reach baseline usefulness is a bad product.
   Plugins extend; they don't complete.
2. **The plugin boundary is a security boundary.** A media server sits on a home LAN with read access to a NAS and
   often write access to download folders. If a plugin can execute arbitrary code, installing one is equivalent to
   handing someone a shell. Kodi's Python addons have no sandbox at all; Jellyfin plugins are trusted .NET assemblies
   loaded in-process. Both are accidents waiting to happen at scale. Do not repeat this.

## 2. Decision: WebAssembly Component Model, hosted by Wasmtime

See [`adr/0003-plugin-runtime.md`](adr/0003-plugin-runtime.md).

| Property | Why it matters here |
|---|---|
| **Capability-based sandbox** | A WASI component starts with **no ambient authority** — no filesystem, no sockets, no clock — and can only do what the host explicitly grants. This maps exactly onto a permission model users can understand. |
| **Deterministic resource limits** | Wasmtime lets the host cap memory, fuel/CPU, and wall-clock per call. A runaway plugin cannot wedge the server. |
| **Truly portable** | The same `.wasm` runs on x86 Linux, an ARM Synology, an Android phone, and (via `jco`/browser Wasm) the web client. One artifact, every platform. Nothing else gives you this. |
| **Any source language** | Rust, Go (TinyGo), C/C++, Zig, JS/TS (via ComponentizeJS), Python (componentize-py). Plugin authors are not forced into your language. |
| **Typed interfaces via WIT** | The Component Model's WIT IDL gives versioned, checked interfaces instead of duck-typed dictionaries. |
| **Mature enough** | Wasmtime was the first runtime to fully implement WASI Preview 2 and the Component Model; the ecosystem is moving to Preview 3 (native `future`/`stream` types for async I/O across the component boundary), which is exactly what a metadata-fetching plugin wants. |

**Faster on-ramp:** [Extism](https://extism.org) wraps the same idea with much less ceremony (host SDKs in a dozen
languages, plugin SDKs likewise). Recommendation: **start on Extism to get the ecosystem moving in month 3, migrate
to raw Wasmtime + WIT components when the interfaces stabilize.** Design the WIT interfaces first either way.

**Rejected alternatives:** native `.so`/`.dll` plugins (ABI hell, no sandbox, no portability); embedded Python
(Kodi's mistake — no sandbox, 40 MB runtime, per-platform pain); embedded JS via QuickJS/Deno (weaker isolation
guarantees than Wasm, no capability model); "plugins are just microservices over HTTP" (great isolation, terrible
UX — nobody wants to run six containers to get subtitles).

## 3. Plugin taxonomy

| Class | Examples | Runs on | Capabilities needed |
|---|---|---|---|
| **Metadata provider** | TMDB, TVDB, AniDB, MusicBrainz, Open Library | Server or client | `http-fetch(allowlist)`, `cache`, `secret` |
| **Artwork provider** | Fanart.tv, TheAudioDB, ThePosterDB | Server | `http-fetch`, `cache`, `secret` |
| **Subtitle provider** | OpenSubtitles, Addic7ed | Server or client | `http-fetch`, `cache`, `secret`, `write-sidecar` |
| **Scraper / parser** | Custom filename schemes, anime numbering maps | Both | none (pure) |
| **Sync target** | Trakt, Simkl, Last.fm, ListenBrainz, Letterboxd | Server | `http-fetch`, `secret`, `read-watch-state` |
| **Notification** | Discord, ntfy, Pushover, Telegram, webhook, email | Server | `http-fetch`, `secret`, `subscribe-events` |
| **PVR / Live TV backend** | HDHomeRun, Tvheadend, IPTV, NextPVR | Server | `http-fetch`, `net-connect(host)` |
| **Source / VFS** | Cloud storage, rclone remotes, torrent-backed VFS | Server or client | `net-connect`, `read-config` |
| **Post-processing job** | Comskip, intro detection variants, loudness, transcode profiles | Server | `read-media`, `spawn-ffmpeg(profile)`, `write-derived` |
| **Enhancement pack** | Anime4K, FSRCNNX, CAS, custom shader chains | Client | *declarative only* — ships `.glsl` + a manifest, no code |
| **Theme / skin** | Full re-skins, TV layouts, accessibility themes | Client | *declarative only* — CSS/tokens/layout JSON |
| **Recommender** | "Because you watched…", mood playlists | Server | `read-library`, `read-watch-state` |
| **Trusted native** (rare) | Hardware tuner drivers, DRM modules, exotic filesystems | Server | Full trust — **separate tier**, signed by you, explicit scary warning |

Note the two **declarative** classes. Shader packs and themes are the most-wanted community extensions and neither
needs to execute code. Make them data, and you get a large, safe ecosystem cheaply.

## 4. Interface design (WIT sketch)

```wit
package lumen:plugin@1.0.0;

interface host-http {
  record request { method: string, url: string, headers: list<tuple<string,string>>, body: option<list<u8>> }
  record response { status: u16, headers: list<tuple<string,string>>, body: list<u8> }
  // Host enforces the URL allowlist from the manifest, rate limits, timeouts, and TLS.
  fetch: func(req: request) -> result<response, http-error>;
}

interface host-cache {
  get: func(key: string) -> option<list<u8>>;
  set: func(key: string, value: list<u8>, ttl-seconds: u32);
}

interface host-secret {
  // Returns a user-supplied credential by declared name. The plugin never sees other secrets.
  get: func(name: string) -> option<string>;
}

interface host-log { log: func(level: level, msg: string); }

world metadata-provider {
  import host-http; import host-cache; import host-secret; import host-log;
  export search: func(q: search-query) -> result<list<candidate>, plugin-error>;
  export fetch-metadata: func(id: external-id, lang: string) -> result<metadata-bundle, plugin-error>;
  export fetch-images: func(id: external-id) -> result<list<image-ref>, plugin-error>;
  export describe: func() -> provider-info;
}
```

Design rules:
- **Plugins never get raw sockets or raw filesystem.** They get `host-http` with a manifest-declared allowlist.
- **Plugins never see user PII** unless they declare and are granted it.
- **Everything is a pure-ish function** — input in, result out. No long-lived plugin state beyond the host cache.
- **Async via WASI Preview 3 streams** once stable; until then, one call = one blocking host call with a hard timeout.
- Version the world (`@1.0.0`). Breaking changes bump the major and the host loads both for a deprecation window.

## 5. Manifest & permissions

```toml
[plugin]
id            = "com.example.tmdb"
name          = "The Movie Database"
version       = "2.1.0"
world          = "lumen:plugin/metadata-provider@1.0.0"
license       = "MIT"
source        = "https://github.com/example/lumen-tmdb"
min_host      = "1.4.0"

[permissions]
http_hosts    = ["api.themoviedb.org", "image.tmdb.org"]   # exact hosts, no wildcards by default
secrets       = ["tmdb_api_key"]
cache_mb      = 64
memory_mb     = 32
cpu_ms_per_call = 5000

[declares]
provides      = ["movie", "series", "season", "episode", "collection"]
languages     = ["*"]
attribution   = "This product uses the TMDb API but is not endorsed or certified by TMDb."
```

At install time the user sees a plain-English permission sheet:

> **The Movie Database** wants to:
> - Connect to `api.themoviedb.org`, `image.tmdb.org`
> - Use your saved *TMDB API key*
> - Use up to 32 MB of memory
> It **cannot** read your files, see other credentials, or connect anywhere else.

That last line is the whole point, and it's only truthful because of the sandbox.

## 6. Registry & distribution

- **Manifest-based repositories** (Jellyfin's model, which works well): a repo is a signed JSON manifest at a URL;
  users add repos; the app browses, installs, and updates from them.
- **Official repo** curated and reviewed; third-party repos allowed with a clear "unreviewed" badge.
- **Signing**: every plugin artifact signed (minisign or Sigstore/cosign with transparency-log inclusion). The host
  refuses unsigned plugins unless developer mode is explicitly enabled.
- **Reproducible builds** encouraged; publish the build recipe alongside the artifact.
- **Semver + compatibility gates**: `min_host`, `world` version. Never auto-update across a major.
- **Sandboxed by default even for official plugins.** No trusted tier for anything that doesn't strictly need it.
- **Kill switch**: a signed revocation list the host checks, so a malicious or broken plugin can be disabled
  remotely for users who opted into the official repo.

## 7. Developer experience (this determines whether an ecosystem exists)

- `lumen plugin new --template metadata-rust` scaffolds a working plugin in 30 seconds.
- `lumen plugin dev` hot-reloads a plugin against a running server with request/response tracing.
- `lumen plugin test` runs the plugin against a **fixture corpus** of 500 real filenames/queries with expected
  outputs — so provider authors can prove correctness.
- A **plugin playground** in the web UI: paste a filename, see what each installed provider returns, side by side.
- Templates for Rust, TinyGo, and TypeScript at minimum.
- Excellent docs with a complete worked example. Ecosystems live or die here.

## 8. First-party plugins to ship at 1.0

TMDB · TheTVDB · TVmaze · AniDB+AniList (with the TVDB mapping) · MusicBrainz+AcoustID · Open Library ·
OpenSubtitles · Fanart.tv · Trakt · Last.fm/ListenBrainz · Discord/ntfy/webhook notifications · HDHomeRun ·
IPTV (M3U/XMLTV) · NFO sidecar reader-writer · Jellyfin/Emby API shim · Anime4K + FSRCNNX + CAS shader packs ·
three built-in themes (Dark, Light, TV/High-contrast).

## Sources
- [WASI.dev — capability-based sandbox, no ambient authority](https://wasi.dev/)
- [WASI & Component Model status](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)
- [Component Model beyond the browser, 2026 — Preview 3, async streams](https://blog.iamcristhian.dev/2026/04/webassembly-component-model-wasm-beyond-browser-2026)
- [The Component Model: architecting agnostic plugin systems](https://techbytes.app/posts/wasm-component-model-plugin-architecture/)
- [Server-side Wasm, WASI, Component Model 2026](https://zeonedge.com/blog/webassembly-server-side-wasm-wasi-component-model-2026)
- [WebAssembly ecosystem 2026 — runtimes and tooling](https://reintech.io/blog/webassembly-ecosystem-2026-tools-frameworks-runtimes)
