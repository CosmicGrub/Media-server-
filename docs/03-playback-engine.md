# 03 — The Playback Engine

This is the product. Everything else is optional. Get this right and a library with rough edges is forgivable; get it
wrong and no amount of metadata polish saves you.

## 1. Core selection: libmpv

**Decision:** embed **libmpv** on every native platform. See [`adr/0001-playback-core.md`](adr/0001-playback-core.md).

### 1.1 Why libmpv over the alternatives

| Candidate | Verdict |
|---|---|
| **libmpv** | ✅ **Chosen.** Stable C client API (`mpv/client.h`) + render API (`mpv/render.h`, `mpv/render_gl.h`), current API version `MPV_MAKE_VERSION(2, 5)`. Handles demux, decode, HW accel, filters, audio output, subtitle rendering (libass), and the `gpu-next`/libplacebo render pipeline. Can be built **LGPLv2.1+** (`-Dgpl=false`) which is what makes App Store distribution possible at all. |
| libVLC / VLCKit | Viable, proven on iOS. Simpler to embed on Apple. Weaker HDR/tone-mapping, no user-shader story, less control over the render pipeline. **Keep as a documented fallback for Apple if libmpv integration stalls.** |
| ExoPlayer / Media3 (Android) | Great for adaptive streaming and platform integration, poor for remuxes: limited container/codec coverage, decoder-slot scarcity (5–16 slots depending on chipset), no libass-quality ASS rendering, historically fragile DTS-HD detection in Matroska. **Use it only for the passthrough/AVR path where the platform decoder is mandatory, and for Cast/DRM.** |
| AVPlayer (Apple) | Mandatory for AirPlay 2, FairPlay, Spatial Audio, and best battery on natively supported formats. Cannot open MKV, cannot do DTS/TrueHD, no VP9 or WebM. **Secondary path only.** |
| GStreamer | Genuinely cross-platform and pipeline-flexible, but the ergonomics, binary size, and per-platform plugin packaging are a tax you don't need. |
| Roll your own on FFmpeg | 2–4 engineer-years to reach mpv's A/V sync, seek accuracy, and hardware-decode fallback logic. Do not. |

### 1.2 Rendering integration

Use the **render API**, not window embedding — window embedding has known platform-specific problems, particularly on
macOS, and the render API lets you composite your own OSD/UI over the video and control the frame lifecycle.

| Platform | Surface | Notes |
|---|---|---|
| Windows | D3D11 (`MPV_RENDER_API_TYPE_SW` fallback) or ANGLE/OpenGL | libplacebo has a D3D11 backend; prefer it for HDR passthrough to the swapchain |
| macOS | Metal via libplacebo, or OpenGL (deprecated but functional) | Must go through `CAMetalLayer` for EDR/HDR output |
| Linux | Vulkan (preferred, best libplacebo path) or OpenGL | Wayland + Vulkan is the good path; keep X11/GLX working |
| Android | OpenGL ES 3.x on a `SurfaceView`/`GLSurfaceView`, Vulkan where available | `SurfaceView` (not `TextureView`) for HDR and for zero-copy hardware decode |
| iOS/tvOS | Metal via libplacebo; software render fallback | tvOS needs the `AVDisplayManager` dance to switch the display mode for HDR/frame rate |
| Web | N/A — libmpv doesn't apply. See §9. | |

**Threading rule:** libmpv's render context must be created and used on the thread that owns the GPU context.
Property access and command dispatch happen on any thread, but never block the render thread on a synchronous
`mpv_command`. In the Rust binding, model this as `MpvHandle` (Send + Sync, command/property) and `MpvRenderContext`
(!Send, pinned to the render thread), communicating over a channel.

## 2. Container and codec coverage

### 2.1 Containers (must all work, including as network streams)
`mkv/mka/mks` (Matroska — the priority), `webm`, `mp4/m4v/m4a/mov` (incl. fragmented), `ts/m2ts/mts` (MPEG-TS,
incl. multi-program), `avi`, `wmv/asf`, `flv`, `ogg/ogv/ogm`, `mpg/mpeg/vob`, `rm/rmvb`, `3gp`, `f4v`, `mxf`, `nut`,
`iso` (UDF + ISO9660), **`BDMV` folder structures**, **`VIDEO_TS` folder structures**, `.mpls`/`.m2ts` playlists,
`.cue` + image (audio), `.iso` SACD, `.dsf`/`.dff`, `.m3u/.m3u8/.pls/.xspf` playlists, `.strm`, HLS, DASH, RTSP,
RTMP, SRT (protocol), `.torrent`-backed streams (via plugin, not core).

### 2.2 Video codecs
`H.264/AVC` (all profiles incl. Hi10P, Hi422P, Hi444PP — anime libraries live here), `HEVC/H.265` (Main, Main10,
Main12, Main 4:2:2 10, Rext), `AV1` (Main, incl. 10-bit, film grain synthesis), `VP9` (Profile 0/2), `VP8`,
`MPEG-2`, `MPEG-4 ASP` (DivX/XviD), `VC-1` (**including interlaced** — a classic Blu-ray remux failure case),
`Theora`, `ProRes`, `DNxHD/HR`, `CineForm`, `MJPEG`, `RealVideo`, `WMV1/2/3`, `H.263`, `FFV1`, `Ut Video`,
`MPEG-4 MVC` (3D Blu-ray — decode base view at minimum), `VVC/H.266` (emerging; FFmpeg has a native decoder — plan
for it), `LCEVC` (enhancement layer; low priority).

### 2.3 Audio codecs — the remux-critical set
See §5 for the passthrough matrix; this is the decode/support list.

**Lossless / HD (the ones that matter for remuxes):**
`Dolby TrueHD` (incl. **Atmos** via the MAT/JOC extension), `DTS-HD Master Audio`, `DTS:X` (MA-based and IMAX
Enhanced), `DTS-HD High Resolution`, `LPCM` up to 7.1/24-bit/192 kHz, `FLAC` (incl. multichannel and 24/192),
`ALAC`, `WavPack`, `Monkey's Audio (APE)`, `TAK`, `TTA`, `DSD` (DSD64/128/256, DoP and native), `MLP`.

**Lossy:** `AC-3`, `E-AC-3` (incl. **JOC**/Atmos), `AC-4` (Atmos on tvOS/Android TV), `DTS` core, `DTS-ES`,
`AAC-LC/HE-AACv1/v2/xHE-AAC`, `Opus` (incl. multichannel), `Vorbis`, `MP3`, `MP2`, `WMA/WMA Pro/WMA Lossless`,
`Cook/RealAudio`, `Speex`, `AMR`, `ATRAC3/3+`, `Nellymoser`.

**Tracker/chiptune (VLC/foobar parity, cheap to add via libopenmpt/game-music-emu plugins):** `MOD/XM/IT/S3M`,
`SID`, `NSF`, `SPC`, `GBS`, `VGM`, `PSF/PSF2`, `USF`, `HES`.

### 2.4 Subtitle formats
`SRT`, `ASS/SSA` (via **libass** — the correctness reference; must honour MKV **attached fonts**), `WebVTT`,
`SUB/IDX (VobSub)`, `PGS/SUP` (Blu-ray bitmap), `DVB subs`, `CEA-608/708` closed captions (embedded in
H.264/HEVC SEI and in MPEG-TS), `TTML/IMSC`, `SAMI`, `MicroDVD`, `MPL2`, `TMP`, `RealText`, `Teletext`.

Requirements beyond "decode it":
- Encoding auto-detection (**uchardet**) for legacy non-UTF-8 SRT — a real-world necessity for older libraries.
- **Forced-subtitle auto-selection**: when the audio track is in the user's preferred language and a `forced` flagged
  track exists in that language, select it automatically. Getting this right is a top-5 user delight item.
- Per-library and per-user preferred audio/subtitle language chains with fallbacks (`ja > en`, `subs: en-forced > en > none`).
- ASS positioning/karaoke/animation correctness at non-native resolutions (`--sub-ass-override=no` semantics).
- **Never burn in subtitles** unless the client genuinely cannot render them; and when you must, say so (Reason G1).

## 3. Hardware decoding

| Platform | API | Notes |
|---|---|---|
| Windows | `d3d11va` (preferred), `dxva2` (legacy), `nvdec`, `qsv` | Prefer `d3d11va-copy` only when a filter chain requires system memory |
| Linux | `vaapi` (Intel/AMD), `nvdec`/`cuda` (NVIDIA), `vulkan` (emerging, best long-term) | VAAPI + Wayland + Vulkan is the modern path |
| macOS | `videotoolbox` | Solid for H.264/HEVC/ProRes; AV1 only on M3+ |
| Android | `mediacodec` (direct surface preferred; `mediacodec-copy` when filtering) | Decoder slots are scarce — release aggressively. Query `MediaCodecList` for actual profile/level support, don't assume. |
| iOS/tvOS | `videotoolbox` | A17/M-series get AV1; older devices software-decode AV1 badly — gate by device |

**Policy:** hardware decode is a *performance optimisation*, never a correctness requirement. Every stream must have a
software fallback path, and the fallback must trigger automatically on decoder error mid-stream (not just at open),
with the switch recorded in the Playback Report. Hi10P H.264 in particular has no hardware support on most GPUs —
always software.

## 4. HDR, Dolby Vision, and tone mapping

Use mpv's **`gpu-next`** video output, which is built on **libplacebo** (Vulkan, OpenGL, and Direct3D 11 backends).
This is the strongest open-source HDR pipeline available and it is what puts you ahead of Plex/Jellyfin/Kodi.

### 4.1 Support matrix — be honest with users

| Format | Status | Approach |
|---|---|---|
| **SDR (BT.709)** | Full | — |
| **HDR10 (ST 2084 + static ST 2086 metadata)** | Full | Passthrough to an HDR display; tone map to SDR otherwise |
| **HLG (BT.2100)** | Full | Passthrough / convert |
| **HDR10+ (ST 2094-40 dynamic metadata)** | Supported by libplacebo | Per-scene tone mapping when the display is SDR or lower-peak; passthrough where the sink accepts it |
| **Dolby Vision Profile 5** (single-layer IPTPQc2) | Supported — metadata consumed | Convert to HDR10/PQ for output, or DV passthrough where the platform allows (Android TV/Shield, some LG/Samsung) |
| **Dolby Vision Profile 8.1/8.4** (single-layer, HDR10/HLG-compatible base) | Supported | Base layer is a valid HDR10/HLG stream; consume DV RPU where possible |
| **Dolby Vision Profile 7 MEL** (dual-layer, minimal EL) | Partial — mpv can consume the DV metadata; the EL adds no picture information for MEL | Play base layer + DV metadata. Correct outcome. |
| **Dolby Vision Profile 7 FEL** (dual-layer, full EL — UHD Blu-ray remuxes) | **Not fully reconstructible in open source.** The FEL carries additional luma/chroma detail to reconstruct a 12-bit 4:2:2 4000-nit source from a 10-bit 4:2:0 1000-nit base. | Play the **base layer as HDR10** and label it clearly. Optionally offer offline conversion to P8.1 via `dovi_tool` as a plugin-driven "optimize for playback" job. Do not claim FEL support. |
| **Dolby Vision Profile 4** (dual-layer, HDR-compat, legacy) | Base layer only | Same as P7 |

Profile 7.6 discs use a 1000-nit HDR10 10-bit 4:2:0 base layer; MEL enhancement layers contain only the DV composer
and content metadata, while FEL layers contain real additional detail. This is why MEL is "fine" and FEL is not.

### 4.2 Tone mapping configuration to expose
Surface these as presets ("Accurate", "Bright Room", "Reference Display", "Custom"), not as raw mpv options:
- `tone-mapping`: `bt.2390` (default, safe), `spline`, `mobius`, `reinhard`, `hable`, `st2094-40`, `st2094-10`
- `target-peak` / `target-contrast` — with automatic detection from EDID/display where available
- `--hdr-compute-peak` (dynamic peak detection; note it interacts with FEL presence)
- `gamut-mapping-mode`: `perceptual`, `relative`, `desaturate`, `clip`
- `inverse-tone-mapping` (SDR→HDR) — offer it, default off, label it as non-reference
- ICC profile loading for calibrated desktop displays
- **Display mode switching**: on tvOS (`AVDisplayManager`), Android TV (`Display.Mode` / `preferredDisplayModeId`),
  and Windows/Linux exclusive fullscreen — match refresh rate to content frame rate (23.976 ↔ 24 ↔ 25 ↔ 50 ↔ 59.94)
  to eliminate judder. This is a huge, under-served quality win.

## 5. Remuxes and lossless audio — the deep dive

You called out remuxes specifically. This section is the design for taking them seriously.

### 5.1 What "remux" means operationally
A remux is a bit-exact copy of the original disc's video and audio elementary streams into a Matroska (usually)
container: no re-encoding, full-bitrate video (often 60–120 Mbps HEVC), and **lossless audio** (TrueHD/Atmos or
DTS-HD MA), plus PGS subtitles, chapters, and sometimes many audio and subtitle tracks.

Implications:
1. **Bitrate.** 100 Mbps sustained. Wi-Fi 5 on a congested band will not do it. The player needs a large,
   configurable read-ahead buffer (`--cache`, `--demuxer-max-bytes` in the hundreds of MB), true streaming reads over
   SMB/NFS with proper readahead, and a visible network-health indicator. Buffer-underrun-driven stutter on remuxes
   is the #1 support burden — instrument it.
2. **Never transcode by default.** A transcode of a remux is a downgrade the user did not ask for.
3. **Track counts.** Anime and international remuxes routinely carry 10+ audio and 30+ subtitle tracks with subtle
   flags. Track selection UX must scale to that (grouped, searchable, with codec/channel/language/flag chips).
4. **Attached fonts.** MKV font attachments must be extracted and fed to libass, or ASS subtitles render wrong.
5. **Chapters and disc structure.** Preserve and expose chapters, chapter names, and (for BDMV) `.mpls` playlists.

### 5.2 Disc-structure playback (BDMV / ISO / VIDEO_TS)
- **libbluray** for BDMV: playlist (`.mpls`) enumeration, main-title heuristic (longest playlist that isn't a
  playall/loop trap), seamless branching across `.m2ts` clips, chapter marks, PGS subtitle streams, forced-subtitle
  flags. Note: **libbluray does not decrypt AACS/BD+**; encrypted discs require external key databases which are a
  legal problem you should not ship (see [`08-legal-licensing.md`](08-legal-licensing.md)). Decrypted rips and
  remuxes are the supported case.
- **libdvdnav/libdvdread** for `VIDEO_TS`, incl. menus (optional), angles, multi-PGC titles.
- **libudfread** + ISO9660 for `.iso` images without mounting.
- **Java menus (BD-J)** — out of scope. Offer "play main title" plus a playlist picker.

### 5.3 Lossless audio decode vs. bitstream passthrough

Two fundamentally different paths, and the product must support both explicitly:

| Path | What happens | When to use |
|---|---|---|
| **Decode to LPCM** | Player decodes TrueHD/DTS-HD MA to multichannel PCM and sends PCM to the sink | Player does the downmix/upmix/EQ/normalization; works with headphones, USB DACs, non-decoding sinks; **loses object-based Atmos/DTS:X positioning** |
| **Bitstream passthrough** | Compressed stream is encapsulated (IEC 61937 / IEC 60958) and sent untouched to an AVR/soundbar which decodes it | Required for **Atmos objects** and **DTS:X**; the enthusiast expectation for remuxes; requires HBR-capable HDMI and a decoding sink |

Any Android TV box (or any sink chain) with **IEC 61937** support can pass through HD audio; TrueHD and DTS-HD are
carried as IEC 61937-encapsulated payloads, which require the **HBR (High Bit Rate)** HDMI audio mode.

### 5.4 Passthrough capability matrix by platform — **the honest version**

| Platform | TrueHD / Atmos | DTS-HD MA / DTS:X | E-AC3 (JOC) | AC-3 / DTS core | Multichannel LPCM | Notes |
|---|---|---|---|---|---|---|
| **Windows** | ✅ WASAPI **exclusive** mode, IEC 61937 | ✅ | ✅ | ✅ | ✅ up to 7.1/192k | Also supports Dolby MAT for Atmos over a PCM-capable path. Requires exclusive-mode device access; must handle device-in-use gracefully. |
| **Linux** | ✅ ALSA `hw:` direct, IEC 61937, correct AES status bits | ✅ | ✅ | ✅ | ✅ | PipeWire passthrough sinks work; PulseAudio needs `iec958` profiles. HBR requires the HDA HDMI HBR path. Document `--audio-exclusive=yes` + `--audio-device=alsa/hw:...`. |
| **macOS** | ❌ **Not practical** | ❌ **Not practical** | ⚠️ limited | ✅ AC-3/DTS over optical (legacy) or CoreAudio compressed formats where the device advertises them | ✅ | **CoreAudio has no general HBR bitstream path over HDMI.** Be upfront: on macOS, decode to LPCM. Atmos on Mac is achieved by Apple's own spatial-audio path, not by TrueHD passthrough. |
| **Android / Android TV** | ✅ `AudioTrack` with `ENCODING_DOLBY_TRUEHD`, `ENCODING_E_AC3_JOC`, `ENCODING_AC4` | ✅ `ENCODING_DTS_HD`, `ENCODING_DTS_UHD_P1/P2` | ✅ | ✅ | ✅ | Must query `AudioManager.getDevices()` → `AudioDeviceInfo.getEncodings()` on the **current output device** and re-query on device change. Known fragility on NVIDIA Shield across firmware versions (see jellyfin-androidtv #5168) — keep a device quirks table. |
| **iOS / iPadOS** | ❌ | ❌ | ⚠️ E-AC3 decode; Atmos via Apple's spatial path only | ✅ decode | ✅ decode | No lossless passthrough. Decode to PCM; offer binaural/spatial rendering. |
| **tvOS** | ❌ passthrough; ✅ Atmos via **E-AC3 JOC / AC-4** through AVPlayer | ❌ | ✅ | ✅ | ✅ | To get Atmos on an Apple TV you must feed AVPlayer an E-AC3-JOC or AC-4 track — meaning a **server-side Atmos-preserving conversion** from TrueHD+Atmos to E-AC3 JOC, which requires a licensed Dolby encoder. Flag as a licensing decision, not an engineering one. |
| **Web** | ❌ | ❌ | ❌ | ⚠️ browser-dependent | ⚠️ up to browser | Web Audio is PCM-only in practice. Web is a "convenience tier" — say so. |

**Design consequence:** the "audio capability" object must be attached to the *output device*, refreshed on device
change (headphones plugged in, AVR switched, Bluetooth connected), and displayed to the user. A device-level profile
is wrong; an app-level profile is very wrong.

### 5.5 Audio quality features to include
- **Bit-perfect mode**: no resampling, no volume scaling, no mixing, exclusive device access. For USB DACs and
  audiophile users. Show a "Bit-perfect ✓" badge with the actual sample rate/depth reaching the device.
- **Automatic sample-rate switching** to match source (44.1/48/88.2/96/176.4/192 kHz) — avoid the OS resampler.
- **DSD**: native DSD (ASIO/WASAPI/ALSA DoP) where supported, PCM conversion otherwise.
- **Downmix control**: user-selectable matrix (Dolby Surround / Lt-Rt vs. Lo-Ro), LFE handling, centre-channel boost
  for dialogue clarity — the single most requested audio feature in every media player forum.
- **Dynamic range compression / night mode**, per-track, remembered per user.
- **Audio delay** with per-device persistence (Bluetooth latency).
- **Gapless playback** and **ReplayGain/EBU R128** for music, with album-mode.
- **Speed control with pitch correction** (rubberband).
- **Channel remap / speaker layout** for non-standard setups.
- **Loudness normalization across a library** (measured offline as a scan job, applied as gain — never re-encode).

### 5.6 Video quality features (the mpv superpowers)
- **User shader packs** — bundle and one-click-enable: **Anime4K** (v4 presets), **FSRCNNX**, **NNEDI3**, **ravu**,
  **AMD CAS** sharpening, **KrigBilateral** chroma upscaling. Per-library defaults ("Anime library → Anime4K Mode A").
  Nothing in the competitive set has this.
- **Scaler selection**: `ewa_lanczossharp`, `spline36`, `polar` variants — as named presets with a live A/B preview.
- **Debanding** (`deband=yes` with tuned thresholds) — massive win on dark HDR grades and old anime.
- **Motion interpolation** (`--interpolation` + `tscale=oversample/mitchell`) and **display-sync** — off by default,
  clearly labelled.
- **Deinterlacing** (`--vf=bwdif`/`yadif`, field-order detection) — necessary for VC-1/MPEG-2 broadcast and DVD rips.
- **Film-grain synthesis** for AV1.
- **Screenshot at source resolution**, with and without subtitles/filters.
- **Frame stepping**, precise seeking (`--hr-seek=yes`), A-B loop, per-file resume with configurable threshold.

## 6. The playback decision ladder

The single most important piece of product logic. Implement it in `lumen-playback` and share it verbatim across all
shells and the server.

```rust
pub enum PlaybackPath {
    DirectPlay,                                    // byte-identical, no server CPU
    DirectStream { remux_to: Container },          // container change only
    PartialTranscode { video: Passthrough, audio: TranscodeSpec },
    PartialTranscode2 { video: TranscodeSpec, audio: Passthrough },
    FullTranscode(TranscodeSpec),
}

pub enum RejectReason {
    ContainerUnsupported { container: Container, client: ClientId },
    VideoCodecUnsupported { codec: Codec, profile: String, level: u8 },
    VideoTooLarge { have: Resolution, max: Resolution },
    BitrateCeiling { have_bps: u64, max_bps: u64, cause: BitrateCause },
    NoHardwareDecoder { codec: Codec, fallback_viable: bool },
    SinkLacksEncoding { codec: Codec, sink: SinkId, sink_encodings: Vec<Encoding> },
    ChannelCountUnsupported { have: u8, max: u8 },
    HdrUnsupportedByDisplay { format: HdrFormat },
    SubtitleBurnInRequired { format: SubtitleFormat, why: BurnInCause },
    NetworkHeadroom { measured_bps: u64, required_bps: u64 },
    UserPolicy { policy: String },   // e.g. "Bit-perfect mode forbids transcoding"
}
```

Rules:
1. Evaluate rungs in order; **stop at the first that fits**.
2. Record every rejection with its reason. Ship the reasons to the UI.
3. `UserPolicy::BitPerfect` short-circuits: if Direct Play fails, **fail loudly with an explanation** rather than
   silently degrading. Give the user the choice.
4. Prefer **audio-only transcode over video transcode**, always. A DTS-HD MA → AC-3 conversion costs ~2% of a CPU
   core; an HEVC 4K transcode costs a GPU.
5. Never burn subtitles if the client can render them; prefer sending the subtitle file/stream separately.
6. Cache the decision keyed by `(media_source_id, client_caps_hash, sink_caps_hash)` — but invalidate on sink change.

## 7. Network sources (server-optional playback)

The player must browse and stream directly from:
`SMB/CIFS` (SMB2/3, incl. signing and encryption), `NFS` (v3/v4), `WebDAV/HTTPS`, `SFTP`, `FTP/FTPS`,
`HTTP(S)` with range requests, `rclone` remotes (via an `rclone serve`/`rcd` bridge or an embedded VFS plugin),
`UPnP/DLNA` servers, and — via plugins — cloud providers.

Implementation notes:
- Prefer **userspace** SMB/NFS clients (e.g. libsmb2/libnfs) over OS mounts: works on Android and iOS without root,
  gives you control over readahead and reconnect behaviour, and avoids the mount-permission mess.
- Aggressive, configurable readahead sized for remux bitrates; measure and expose throughput.
- Resume-on-reconnect for flaky Wi-Fi: transparent range re-request, not a playback error.
- Never copy the whole file to play it.

## 8. Downloads / offline

- Download the **original file** by default (a "download" that transcodes is a lie), with an optional
  "optimize for device" transcode job that is explicit.
- Encrypted-at-rest local cache with per-device keys; the app can play it, other apps cannot casually scrape it.
- Offline watch-state queue that merges via the CRDT on reconnect ([`05-server-library.md`](05-server-library.md) §9).

## 9. The web player (a different animal)

libmpv does not apply. The web tier is:
- **MSE + `<video>`** with `fMP4/CMAF` segments; **EME** only if you ever need DRM (you don't — see §8 of arch doc).
- **WebCodecs + WebGL/WebGPU** custom renderer for formats browsers won't take natively — this is how you get
  meaningful Direct Play on the web (e.g. decoding HEVC/AV1 via WebCodecs where the platform exposes it, rendering
  yourself, doing your own audio through Web Audio). Increasingly viable; treat as an enhancement path.
- **libass compiled to WebAssembly** (`JASSUB`/`SubtitlesOctopus` lineage) for correct ASS rendering in the browser.
- Fall back to **server-side transcode to H.264/AAC in CMAF** for anything else.
- Set expectations in the UI: the web player is the convenience tier. No lossless audio, no HDR passthrough on most
  browsers, limited codecs.

## 10. Conformance corpus (build this in week 2)

Automated per-platform playback tests against a curated set of genuinely hard files. Store as small clipped samples
(a few seconds each) with expected-outcome manifests. Minimum set:

| # | File characteristic | Tests |
|---|---|---|
| 1 | HEVC Main10 + TrueHD Atmos 7.1 + PGS, MKV, 90 Mbps | Direct Play, passthrough, PGS render |
| 2 | HEVC + DTS-HD MA 7.1 + DTS:X, MKV | DTS-HD detection in Matroska (historically buggy) |
| 3 | Dolby Vision Profile 7 FEL, MKV | Base-layer HDR10 fallback + correct labelling |
| 4 | Dolby Vision Profile 5, MP4 | DV metadata consumption, no green/purple cast |
| 5 | HDR10+ dynamic metadata | Per-scene tone map on SDR display |
| 6 | H.264 Hi10P anime + 8 ASS tracks + 40 attached fonts | libass fidelity, font attachment, track UX |
| 7 | VC-1 interlaced, m2ts | Deinterlace + field order |
| 8 | MPEG-2 DVD `VIDEO_TS` rip, multi-angle | libdvdnav path |
| 9 | BDMV folder, seamless branching, forced subs | libbluray playlist logic |
| 10 | Truncated / index-damaged MKV | Graceful recovery, seekability |
| 11 | AV1 10-bit with film grain | Decode + grain synthesis |
| 12 | 3D MVC Blu-ray remux | Base-view decode without failure |
| 13 | Multi-program MPEG-TS from a tuner | Program selection |
| 14 | FLAC 24/192 7.1 + cue sheet | Bit-perfect, sample-rate switch |
| 15 | SACD ISO / DSF | DSD path |
| 16 | Windows-1251 SRT, no BOM | Encoding auto-detect |
| 17 | 60 fps HFR HEVC | Frame pacing, display-mode switch |
| 18 | xHE-AAC / AC-4 | Modern codec coverage |
| 19 | 8-hour concert MKV, 60 GB | Long-file seek, index handling |
| 20 | File on flaky SMB with induced packet loss | Reconnect without playback error |

Run 1–20 on every platform in CI (device farm or emulators + a small physical rack). A regression here is a P0.

## Sources
- [libmpv API documentation](https://mpv-player-mpv.mintlify.app/embedding/libmpv)
- [mpv-examples/libmpv README](https://github.com/mpv-player/mpv-examples/blob/master/libmpv/README.md)
- [mpv HDR Guide 2026 — HDR10, HDR10+, Dolby Vision & tone mapping](https://carlosfelic.io/misc/mpv-hdr-guide-2026/)
- [Ultimate mpv.conf Guide 2026 — HDR10+, DV, gpu-next, Atmos](https://carlosfelic.io/misc/best-mpv-config-2026/)
- [Dolby Vision Profile 7 FEL/MEL direct play discussion](https://github.com/damontecres/Wholphin/discussions/1114)
- [Playback Rodeo — Dolby Vision profiles](https://playback.rodeo/dolby-vision/)
- [DTS-HD MA passthrough regression, jellyfin-androidtv #5168](https://github.com/jellyfin/jellyfin-androidtv/issues/5168)
- [ExoPlayer DTS-HD detection in Matroska #6225](https://github.com/google/ExoPlayer/issues/6225)
- [DTS-HD Master Audio — Wikipedia](https://en.wikipedia.org/wiki/DTS-HD_Master_Audio)
- [ALSA HDMI HBR passthrough patches](https://lkml.iu.edu/hypermail/linux/kernel/1008.0/01017.html)
- [Kodi forum — IEC 61937 TrueHD passthrough](https://forum.kodi.tv/showthread.php?tid=371292)
- [media_kit — libmpv-based cross-platform Flutter player](https://github.com/media-kit/media-kit)
- [Flutter video playback constraints — decoder slots, AVPlayer format limits](https://verygood.ventures/blog/video-playback-flutter-feed/)
