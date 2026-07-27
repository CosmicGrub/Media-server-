# 08 — Legal, Licensing, Patents & Distribution Constraints

> **Not legal advice.** This is engineering-relevant research to scope the questions you must put to a lawyer. The
> items marked 🔴 should be resolved with counsel *before* the corresponding code is written.

## 1. The single most important decision: LGPL-only, dynamically linked

FFmpeg is dual-licensed: **LGPL v2.1+** for the core, with parts available only under **GPL v2+**. The choice is
determined by your build flags and linkage:

| Build | Resulting license | Consequence |
|---|---|---|
| `--disable-gpl` (default), dynamic linking | **LGPL 2.1+** | Closed-source or differently-licensed application code is permitted, provided you meet LGPL obligations (see §1.2) |
| `--enable-gpl` (needed for `libx264`, `libx265`, and some filters) | **GPL 2+** | The **entire combined work** must be GPL. No App Store. No proprietary licensing, ever. |
| `--enable-nonfree` (e.g. with `libfdk-aac`) | **Non-distributable** | You may not distribute the binary at all |
| `--enable-version3` | GPLv3/LGPLv3 | Additional obligations; also incompatible with some app-store terms |

Most of FFmpeg's core (`libavformat`, `libavcodec`, `libavutil`) is LGPL and usable in closed-source applications
when dynamically linked with proper attribution. FFmpeg is **not** available under any other terms, including
commercially — you cannot buy your way out.

**mpv** is likewise dual-natured: historically GPLv2+, with an LGPL relicensing effort that allows building an
**LGPLv2.1+** libmpv (meson `-Dgpl=false`, historically `--enable-lgpl`). An LGPL build drops some GPL-only
components (certain filters and video outputs) but the core player, `gpu-next`, and the client/render APIs work.
🔴 **Verify this per mpv release against the exact feature set you need** — the set of LGPL-excluded components has
changed over time. **libplacebo** is LGPLv2.1+. **libass** is ISC. **libbluray** is LGPLv2.1+. **libdvdnav/libdvdread**
are GPLv2+ 🔴 — this is a trap; if you need DVD menu support you may need to isolate it (§1.3).

### 1.1 Enforce it in CI (do this in week 1)

```bash
#!/usr/bin/env bash
# ci/license-gate.sh — run on every build of the native stack
set -euo pipefail
BAD='--enable-gpl|--enable-nonfree|--enable-version3|--enable-libx264|--enable-libx265|--enable-libfdk-aac'
grep -REn "$BAD" native/ && { echo "GPL/nonfree flag in build recipes"; exit 1; }
# verify the produced binary
"$FFMPEG_BIN" -version | grep -Eq 'enable-gpl|enable-nonfree' && { echo "GPL/nonfree in built FFmpeg"; exit 1; }
"$FFMPEG_BIN" -version | grep -q 'libavutil license: LGPL' || { echo "unexpected license string"; exit 1; }
cargo deny check licenses   # Rust dependency license policy
echo "license gate OK"
```
Add `cargo-deny` with an explicit allow-list (`MIT`, `Apache-2.0`, `BSD-*`, `ISC`, `MPL-2.0`, `Zlib`, `LGPL-2.1+`)
and a deny-list (`GPL-*`, `AGPL-*`, `SSPL`, `BUSL`, unknown).

### 1.2 LGPL obligations you must actually meet
1. **Dynamic linking** (shared libraries / frameworks inside the bundle), not static.
2. **Publish complete corresponding source** for the LGPL components including any modifications you made, at a
   durable public URL.
3. **Enable relinking**: ship object files or a documented, working build script so a user can rebuild the app
   against their own modified libmpv/FFmpeg (LGPL §6).
4. **Attribution and license text** in an accessible About/Legal screen, plus a copy of LGPL 2.1.
5. **Do not impose additional restrictions** that would prevent 2–3 (this is where app stores get complicated).

Build an **SBOM** (CycloneDX or SPDX) per release and auto-generate the Legal screen from it. Do it from the first
release; retrofitting attribution across a hundred dependencies is miserable.

### 1.3 Handling GPL-only components you might want
`libdvdnav`/`libdvdread` (GPLv2+), `libx264`/`libx265` (GPL, for server-side encoding), some FFmpeg filters, and
GPL-only mpv pieces. Options, in order of preference:

- **Do without.** Use `libavcodec`'s LGPL encoders for server transcoding: **hardware encoders** (NVENC, QSV, VAAPI,
  AMF, VideoToolbox) are LGPL-compatible and are what you want on a server anyway. For software encoding, `libaom`
  (BSD), `SVT-AV1` (BSD-3+patent), and `libvpx` (BSD) are permissive. **You do not need x264/x265.** This is a real
  and often-missed finding: a hardware-first transcoder sidesteps the entire GPL encoder problem.
- **Separate process, separate distribution.** If a user opts into a GPL component, it's a separately-downloaded,
  separately-licensed executable invoked over a process boundary, with its own source availability. Legally cleaner
  than linking, but 🔴 get an opinion on whether your invocation creates a combined work.
- **Ship the whole product as GPL.** Perfectly reasonable if you never want App Store distribution or proprietary
  licensing. Decide this consciously, not by accident.

## 2. Codec patents (separate from copyright licensing)

Patent-encumbered codecs require licensing **regardless of the software license**. FFmpeg's license says nothing
about patents.

| Codec | Pool(s) | Practical exposure |
|---|---|---|
| **H.264/AVC** | Via Licensing (ex-MPEG LA) | Decoding in free software distributed at no charge has historically drawn no enforcement; the AVC pool has royalty-free provisions for free internet video. Commercial distribution at volume 🔴 |
| **H.265/HEVC** | Via LA, Access Advance, plus unpooled holders | Genuinely messy — multiple pools, no single license. The main reason browsers avoided HEVC. 🔴 |
| **VVC/H.266** | Access Advance, Via LA | Same shape as HEVC, earlier |
| **AAC** | Via LA | Requires a license for encoder distribution; decoder terms vary 🔴 |
| **Dolby (AC-3, E-AC-3, TrueHD, Atmos, AC-4)** | Dolby Laboratories | 🔴 **Decoding Dolby formats in a distributed product requires a Dolby license.** Passthrough/bitstreaming generally does not (you're not decoding). **Encoding to E-AC-3 JOC for Atmos on tvOS absolutely does.** |
| **DTS (DTS, DTS-HD MA, DTS:X)** | Xperi/DTS Inc. | Same shape as Dolby 🔴 |
| **Dolby Vision** | Dolby | Certified DV output requires licensing + certification. Consuming the RPU and tone-mapping to HDR10 is a different question 🔴 |
| **AV1, VP9, Opus, FLAC, Vorbis, Theora** | AOMedia / royalty-free | ✅ Safe. Prefer these wherever you control the format. |

**Practical posture (what everyone actually does):**
- Open-source, free-of-charge distribution of a decoder-only player has a long, un-enforced history (VLC, mpv, Kodi,
  Jellyfin all ship these decoders). Risk is low but non-zero and rises sharply with commercial revenue.
- **Passthrough is your friend.** Bitstreaming TrueHD/DTS-HD to an AVR that holds its own license moves the decode
  (and the patent question) to hardware the user already paid for. Another reason the remux-first design is right.
- If you commercialize: budget for Dolby and DTS licensing, or ship a build without those decoders and let users
  supply them.
- Some platforms (Windows, macOS, Android, iOS) already license H.264/HEVC/AAC at the OS level — using the
  **platform decoder** (`d3d11va`, `videotoolbox`, `mediacodec`) rather than a bundled software decoder reduces
  exposure. Another argument for hardware-first.

## 3. App store constraints

### 3.1 Apple App Store 🔴 (the hardest one)
The conflict is well documented: App Store terms impose **non-transferable licenses** and **DRM (FairPlay)** on all
apps, which conflicts with the GPL's freedom to redistribute and modify. This is why GPL-licensed VLC was pulled from
the App Store in 2011, and why VideoLAN pursued LGPL relicensing. Even under LGPL the question isn't fully settled,
because LGPL §6 requires that users be able to relink the application against a modified version of the library,
which a DRM-locked store arguably prevents.

**Mitigation plan** (also in [`04-platform-strategy.md`](04-platform-strategy.md) §4.1):
1. LGPL-only native stack, dynamically linked as frameworks in the bundle.
2. Publish complete source + object files/build scripts enabling relinking, at a stable public URL.
3. You hold copyright on 100% of your own code (use a CLA if you take contributions, or keep the app shell
   permissively licensed).
4. 🔴 Written legal opinion before first submission.
5. Existence proof: VideoLAN maintains VLC on the App Store today under LGPL. That is the template.
6. Fallbacks: EU alternative app marketplaces (DMA), AltStore, TestFlight community builds.

Also note: Apple requires an **export-compliance** declaration for encryption (you'll use TLS — the standard
exemption applies but must be declared), and **App Tracking Transparency** if you ever add analytics.

### 3.2 Google Play
Far more permissive about GPL/LGPL. Watch instead for:
- `MANAGE_EXTERNAL_STORAGE` — heavily restricted; use SAF and per-file access. 🔴 Justify or avoid.
- Data safety declarations must be accurate.
- If you ever add a paid tier, Play Billing rules apply.

### 3.3 Amazon Appstore (Fire TV), Samsung Tizen, LG webOS, Roku
Generally permissive about licensing. Each has its own review process and technical certification (especially Roku).

### 3.4 F-Droid
Requires fully free/open dependencies and reproducible builds. An LGPL-only stack qualifies. Worth targeting — it's
free distribution to exactly your audience.

## 4. Content-related legal boundaries

Things the product must **not** do, because they convert a legitimate media player into a legal problem:

| Don't | Why |
|---|---|
| Ship AACS/BD+ decryption keys or `libaacs`/`libbdplus` key databases | DMCA §1201 / EUCD anti-circumvention. **libbluray itself does not decrypt** — keep it that way. Support decrypted rips and remuxes only. |
| Ship CSS (DVD) decryption in jurisdictions where it's contested | Same. `libdvdcss` is a separate, user-installed component for a reason — follow VLC's model if you support it at all. |
| Bundle a torrent/usenet client or indexer | Turns you into a target. Integrate with the *arr stack via plugins instead. |
| Ship default IPTV playlists or channel lists | You would be distributing links to (usually) infringing streams. Users supply their own M3U. |
| Implement Widevine L1 / FairPlay / PlayReady for commercial streaming | Requires certification you won't get, and poisons the licensing story. Link out to the official apps. |
| Scrape sites whose ToS forbid it (IMDb in particular) | Use TMDB's IMDb ID cross-references instead. |
| Auto-download "missing" media | Same category as the torrent client. |

Include a clear statement that the product is for media the user has the right to possess, and do not build features
whose only use is infringement.

## 5. Metadata provider terms

| Provider | Terms | Action |
|---|---|---|
| **TMDB** | Free for non-commercial use with attribution. Commercial use requires a license (contact TMDB). Must display *"This product uses the TMDb API but is not endorsed or certified by TMDb"* prominently, and attribution in About/Credits. | Ship the notice. **Ship with no bundled API key** — let users supply their own, which also keeps you out of the commercial tier by default. 🔴 If you monetize, get the license. |
| **TheTVDB** | Tiered by company revenue: free under $50k/yr with attribution + a direct link to TheTVDB.com shown to end users; $1,000/yr at $50k–$250k; $10,000/yr at $250k–$1M; custom above. | Same posture. Budget for it if you monetize. |
| **MusicBrainz** | Core data is CC0 / open; requires a descriptive User-Agent and rate-limit compliance (1 req/s). | Easy. Comply with the UA and rate limit. |
| **OpenSubtitles** | API key required, quota-limited, attribution required. | User-supplied key. |
| **Fanart.tv** | API key, personal keys free, project keys for apps. | User-supplied or project key with attribution. |
| **Trakt** | OAuth app registration, rate limits, branding requirements. | Register a project app. |
| **AniDB** | Strict client registration and rate limits; aggressive about abuse. | Register properly, respect limits hard. |

**Architectural consequence:** because every one of these has a revenue-linked or key-linked constraint, the provider
layer must be a **plugin boundary with per-user credentials** from day one. This is a legal requirement dressed as an
architecture decision.

## 6. Your own license choice

| Option | Implication |
|---|---|
| **GPLv3 / AGPLv3** | Maximum community trust and the Jellyfin/Kodi posture. Forecloses App Store distribution and proprietary licensing. |
| **MPL-2.0** (recommended for the core) | File-level copyleft: your improvements stay open, but the combined work can be distributed under app-store terms. Compatible with an LGPL native stack. Good balance. |
| **Apache-2.0** with a CLA | Maximum flexibility including future commercial options; includes a patent grant. |
| **Split**: permissive/MPL core + GPL server + proprietary optional services | The "open core" model. Works, but be transparent about it from day one or the community will (justifiably) turn on you. |

**Recommendation: MPL-2.0 for `lumen-core` and the shells, Apache-2.0 for the plugin SDK and WIT interfaces**
(so plugin authors have no friction), with an SPDX header in every file and a CLA if you accept outside
contributions. This keeps every option open, including App Store distribution.

## 7. Privacy & regulatory

- **No telemetry by default.** Opt-in, documented, locally inspectable before sending, and easy to revoke.
- **GDPR/CCPA**: if you ever run any hosted service (relay, plugin registry, account system), you become a
  controller. Data minimization, export, and deletion must exist. Design the relay to be **zero-knowledge** — it
  forwards encrypted bytes and never sees library contents.
- **Children's data (COPPA/age-appropriate design)**: managed child profiles are a regulated surface if any data
  leaves the device. Keep them entirely local.
- **AI data egress**: for the cloud agent backend, explicit informed consent naming exactly what is sent, plus a
  redaction profile. Filenames alone can be sensitive.
- **Security disclosure policy** and a `SECURITY.md` from day one. You are shipping a network service that indexes
  people's private files.

## 8. Checklist before first public release

- [ ] CI license gate green; SBOM generated per release
- [ ] Legal/About screen auto-generated from the SBOM, includes LGPL text and all provider attributions
- [ ] Complete corresponding source for LGPL components published at a stable URL
- [ ] Relinking instructions + object files published for App Store builds
- [ ] 🔴 Counsel opinion on App Store + LGPL
- [ ] 🔴 Counsel opinion on Dolby/DTS decoder distribution for your business model
- [ ] No AACS/BD+/CSS keys anywhere in the tree or build
- [ ] No bundled provider API keys; user-supplied credentials only
- [ ] `SECURITY.md`, disclosure contact, and a signing key for releases
- [ ] Privacy policy that is true
- [ ] Export-compliance declaration for Apple

## Sources
- [FFmpeg License and Legal Considerations](https://www.ffmpeg.org/legal.html)
- [FFmpeg LICENSE](https://ffmpeg.org/doxygen/4.4/md_LICENSE.html)
- [FFmpeg commercial license guide — LGPL, GPL, patent risks](https://32blog.com/en/ffmpeg/ffmpeg-commercial-license-guide)
- [Understanding FFmpeg licensing before shipping](https://hoop.dev/blog/understanding-ffmpeg-licensing-what-developers-need-to-know-before-shipping)
- [Using LGPL in commercial software](https://medialooks.com/lgpl)
- [mpv (media player) — licensing history](https://en.wikipedia.org/wiki/Mpv_(media_player))
- [The GPL and the iOS App Store — Michel Fortin](https://michelf.ca/blog/2011/gpl-ios-app-store/)
- [FSF — VLC and App Store DRM enforcement](https://www.fsf.org/blogs/licensing/vlc-enforcement)
- [As Apple pulls GPL-licensed VLC — CDM](https://cdm.link/as-apple-pulls-gpl-licensed-vlc-the-developers-version-of-events-what-it-means-for-free-video/)
- [LGPL and app stores — LWN](https://lwn.net/Articles/526355/)
- [Apple Developer Forums — LGPL in iOS apps](https://developer.apple.com/forums/thread/73402)
- [TMDB API Terms of Use](https://www.themoviedb.org/api-terms-of-use) · [TMDB API for Business](https://www.themoviedb.org/api-for-business) · [TMDB commercial-use discussion](https://www.themoviedb.org/talk/592c8779c3a3680fc20012d5)
- [TheTVDB API information & pricing tiers](https://www.thetvdb.com/api-information)
