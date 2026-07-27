# 11 — Compatibility Charter: The Universal Play Guarantee

This document turns "maximum compatibility" from an aspiration into a **testable contract**. It defines what the
player promises, enumerates the complete format surface, specifies the full quality and resolution spectrum, and
names the small, honest set of things that genuinely cannot work.

Companion documents:
- [`12-container-conformance.md`](12-container-conformance.md) — the exhaustive MP4 and MKV conformance spec (**"all MP4s and MKVs perfectly playable"** lives there)
- [`13-remux-transcode-matrix.md`](13-remux-transcode-matrix.md) — remux legality and transcode compatibility matrices
- [`../conformance/`](../conformance/) — the machine-readable test corpus that proves all of it

---

## 1. The guarantee

> **G0 — Universal Play.** If a file contains at least one stream that FFmpeg can demux and decode, this player
> plays it. Container quirks, missing indexes, damaged headers, wrong file extensions, exotic codec/container
> combinations, and non-conformant muxer output are **the player's problem to solve, not the user's**.
>
> **G1 — No Silent Degradation.** Whenever playback is anything less than bit-exact source reproduction, the player
> states — on screen, in plain language — exactly what changed and why.
>
> **G2 — No Refusal.** The player never shows "unsupported format." If it cannot play something, it says
> specifically what it cannot do and offers the nearest thing it can.

These are enforced by the conformance corpus in CI. A file in the corpus that stops playing is a **P0 release
blocker**, not a bug report.

### 1.1 Playback tiers

Every playback session resolves to exactly one tier, and the tier is displayed in the Playback Report.

| Tier | Name | Meaning |
|---|---|---|
| **T0** | **Bit-exact** | Every byte of every selected stream reaches the decoder/sink untouched. Lossless audio bitstreamed or decoded without resampling or mixing. No container rewrite. |
| **T1** | **Full fidelity** | Streams untouched; container rewritten (remux) and/or lossless audio decoded to LPCM at source rate/depth. Visually and audibly identical. |
| **T2** | **Preserved** | Video untouched; audio converted but at or above source channel count (e.g. TrueHD 7.1 → LPCM 7.1, or DTS-HD MA → its lossy DTS core). HDR preserved. |
| **T3** | **Adapted** | Video and/or audio transcoded. HDR possibly tone-mapped. Reason stated for every change. |
| **T4** | **Recovered** | Source is damaged/non-conformant; the player reconstructed enough to play. Extent of the damage stated. |
| **T5** | **Blocked** | Cannot play. Only the causes in §7 may produce this, and each has a specific, actionable message. |

**Design rule:** the ladder in [`03-playback-engine.md`](03-playback-engine.md) §6 always seeks the lowest tier
number that the chain supports. `PlaybackReport` records the achieved tier plus every higher-fidelity tier that was
attempted and the structured reason it failed.

---

## 2. Container support surface

**Rule 0: the file extension is a hint, never a decision.** Every file is content-probed. A `.avi` that is actually
Matroska plays as Matroska. A file with no extension plays. A `.txt` the user drags in gets probed.

### 2.1 Tier A — first-class (full feature support, in the conformance corpus)

| Container | Extensions | Notes |
|---|---|---|
| **Matroska** | `.mkv .mka .mks .mk3d` | **Full spec surface — see [`12`](12-container-conformance.md) §2** |
| **WebM** | `.webm` | Matroska subset; must also accept out-of-subset content muxed as WebM |
| **ISOBMFF / MP4** | `.mp4 .m4v .m4a .m4b .m4r .mp4v` | **Full box surface — see [`12`](12-container-conformance.md) §3** |
| **QuickTime** | `.mov .qt` | Including reference movies, legacy codecs, timed metadata |
| **Fragmented MP4 / CMAF** | `.mp4 .m4s .cmfv .cmfa` | `moof`/`sidx`/`tfdt`, init+media segment pairs |
| **MPEG-TS** | `.ts .m2ts .mts .m2t .tsv .tp` | Multi-program, 188/192/204-byte packets, PCR discontinuities, partial captures |
| **MPEG-PS** | `.mpg .mpeg .vob .evo .mod .tod` | DVD/HD-DVD structures |
| **AVI** | `.avi .divx` | OpenDML (>4 GB), VBR audio, ODML index, broken index recovery |
| **ASF/WMV** | `.wmv .wma .asf` | Including DRM detection (report, don't fail silently) |
| **FLV / F4V** | `.flv .f4v` | Legacy but abundant |
| **Ogg** | `.ogg .ogv .oga .opus .spx .ogm` | Chained streams |
| **HLS** | `.m3u8` | TS and CMAF variants, LL-HLS, byte-range, discontinuities, EXT-X-MAP |
| **DASH** | `.mpd` | Static + dynamic, multi-period, SegmentTemplate/Timeline/Base |
| **Disc structures** | `BDMV/`, `VIDEO_TS/`, `.iso`, `.mpls` | libbluray / libdvdnav / libudfread |

### 2.2 Tier B — supported (plays correctly, fewer advanced features)

`MXF` (OP1a, OP-Atom, AS-11) · `GXF` · `LXF` · `NUT` · `RealMedia` (`.rm .rmvb .ra`) · `3GP/3G2` (`.3gp .3g2`) ·
`AMV` · `MTV` · `NSV` · `SWF` · `VIV` · `DV` raw (`.dv .dif`) · `CAF` · `AIFF/AIFC` · `WAV/RF64/W64/BW64` (>4 GB) ·
`AU/SND` · `VOC` · `IFF/8SVX` · `Bink` (`.bik`) · `Smacker` (`.smk`) · `Electronic Arts` (`.vp6 .cdata`) ·
`Sega FILM` · `Interplay MVE` · `Westwood VQA` · `RoQ` · `THP` · `4XM` · `Discworld II BMV` · `Konami PS2 BMV` ·
`MPEG-4 in raw ES` · `HEIF/HEIC/AVIF sequences` · `Motion Photos` (Google/Samsung MP4-in-JPEG)

### 2.3 Tier C — raw elementary streams (headerless; the last-resort demuxers)

`.h264 .264 .avc` · `.h265 .265 .hevc` · `.h266 .266 .vvc` · `.av1 .obu` · `.ivf` · `.m1v .m2v .mpv` ·
`.vc1` · `.cavs` · `.ac3 .eac3` · `.dts .dtshd` · `.aac .adts .latm` · `.mp3 .mp2 .mpa` · `.flac` · `.ape` ·
`.wv` · `.tta` · `.tak` · `.mlp .thd` (raw TrueHD) · `.dsf .dff` (DSD) · `.sbc` · `.amr` · `.gsm` · raw PCM/YUV/RGB
with user-specified geometry

### 2.4 Tier D — playlists, sidecars, and indirection

`.m3u .m3u8 .pls .xspf .asx .wax .wvx .cue .strm .ram .qtl .wpl .zpl` · `.iso` mount-free · `rar`/`zip`-contained
video (non-solid, non-compressed store mode — as a plugin) · `.torrent`/magnet-backed VFS (plugin) · `.nfo` sidecars

### 2.5 Protocols

`file` · `http(s)` (with range, redirects, cookies, custom headers, HTTP/2 and HTTP/3) · `ftp`/`ftps` · `sftp` ·
`smb`/`cifs` (SMB 2/3, encrypted, signed) · `nfs` (v3/v4) · `webdav(s)` · `rtsp`/`rtsps` (TCP+UDP interleaved) ·
`rtmp(e/s/t)` · `rtp`/`sdp` · `udp`/`multicast` (with FEC) · `srt` · `rist` · `hls` · `dash` · `data:` ·
`pipe:`/stdin · `dlna`/`upnp` · rclone remotes

---

## 3. Video codec matrix

**Column key:** SW = software decode always available · HW = hardware decode where the platform provides it.
Every codec has a software fallback. **Hardware decode is never a correctness requirement** (see §6).

### 3.1 Modern / primary

| Codec | Profiles required | SW | HW (D3D11VA / VAAPI / NVDEC / VideoToolbox / MediaCodec / Vulkan) |
|---|---|:--:|---|
| **H.264 / AVC** | Baseline, Constrained Baseline, Main, Extended, **High, High 10 (Hi10P), High 4:2:2, High 4:4:4 Predictive**, Intra profiles, MVC (base + stereo view) | ✅ | ✅ 8-bit 4:2:0 broadly. **Hi10P/4:2:2/4:4:4 are software-only on virtually all hardware** — expect and plan for it. |
| **H.265 / HEVC** | Main, Main 10, Main 12, Main Still, **Main 4:2:2 10/12, Main 4:4:4 8/10/12/16, Rext, SCC (screen content), Monochrome 8/12/16** | ✅ | ✅ Main/Main10 broadly; 4:2:2/4:4:4/12-bit varies sharply by vendor |
| **AV1** | Main (0), High (1), Professional (2); 8/10/12-bit; **film grain synthesis**; Annex-B and low-overhead OBU | ✅ (dav1d) | ✅ Intel Xe/Arc, NVIDIA Ampere+, AMD RDNA2+, Apple M3+, recent Snapdragon/Exynos; **Vulkan compute decode** as a cross-vendor path |
| **VP9** | Profile 0/1/2/3, 8/10/12-bit, 4:2:0/4:2:2/4:4:4 | ✅ | ✅ widely; **Vulkan** path available |
| **VVC / H.266** | Main 10, Main 10 4:4:4, Multilayer, **SCC (IBC, palette mode, ACT)** | ✅ (native FFmpeg decoder) | ⚠️ emerging — **VA-API decode exists**; otherwise software |
| **VP8** | — | ✅ | ✅ |
| **MPEG-2 Video** | SP, MP, 422P, HP; interlaced field/frame pictures | ✅ | ✅ (legacy paths) |
| **MPEG-4 Part 2 (ASP)** | SP, ASP; DivX 3/4/5, XviD, 3ivx, GMC, QPel, packed bitstreams, B-VOP | ✅ | ⚠️ |
| **VC-1 / WMV9** | SP, MP, **AP including interlaced** | ✅ | ⚠️ legacy hardware only |

### 3.2 Professional / intermediate / lossless

`Apple ProRes` (Proxy, LT, 422, HQ, 4444, 4444 XQ) · **`ProRes RAW`** · `Apple Intermediate` ·
`Avid DNxHD / DNxHR` (LB, SQ, HQ, HQX, 444) · `GoPro CineForm` · **`APV` (Advanced Professional Video)** ·
`JPEG 2000` (incl. lossless, Digital Cinema profiles) · `FFV1` (v0–v3, all slicing) · `HuffYUV`/`FFVHuff` ·
`Ut Video` · `MagicYUV` · `Lagarith` · `Dirac`/`VC-2` · `CFHD` · `SheerVideo` · `Uncompressed` (`v210`, `v410`,
`r210`, `Y41P`, `UYVY`, `YUY2`, `NV12`, `RGB24/32`, `BGRA`, planar 16-bit) · `DV`/`DVCPRO`/`DVCPRO50`/`DVCPRO HD` ·
`Cineon/DPX sequences` · `EXR sequences` · `AVC-Intra` · `XAVC` · `XDCAM HD/EX` · `HAP`/`HAP Q`/`HAP Alpha`

### 3.3 Legacy / archival (VLC-parity — cheap coverage, real value)

`Theora` · `RealVideo 1/2/3/4` and **`RealVideo 6`** · `Sorenson SVQ1/SVQ3` · `Cinepak` · `Indeo 2/3/4/5` ·
`MJPEG`/`MJPEG-B`/`LJPEG` · `H.261` · `H.263`/`H.263+`/`H.263++` · `Flash Screen Video 1/2` · `VP3/5/6/7` ·
`Windows Media Video 7/8` · `MSMPEG-4 v1/v2/v3` · `MS Video 1` · `MS RLE` · `QuickTime RLE`/`Animation` ·
`Smacker` · `Bink Video 1/2` · `TrueMotion 1/2` · `Duck TM2X` · `ZMBV` · `TSCC`/`TSCC2` · `CamStudio` ·
`Screenpresso` · `FIC` · `Fraps` · `AASC` · `Loco` · `WNV1` · `8BPS` · `QDraw` · `PAF` · `Delphine CIN` ·
`Amiga IFF/ANIM` · `FLIC` · `GIF` (animated) · `APNG` · `WebP` (animated) · `AVIF` (animated/sequence)

### 3.4 Image formats (photo libraries and cover art)

`JPEG` · `PNG` · `WebP` · `AVIF` · `HEIF/HEIC` · `JPEG XL` · `TIFF` · `BMP` · `GIF` · `TGA` · `PSD` ·
`DDS` · `EXR` · `HDR/Radiance` · `SVG` (rasterized) · **camera RAW** (`CR2/CR3, NEF, ARW, DNG, RAF, ORF, RW2`) via a
plugin · `PGM/PPM/PBM/PAM` · `XPM` · `ICO`

---

## 4. Audio codec matrix

### 4.1 Lossless — the remux-critical set

| Codec | Requirements |
|---|---|
| **Dolby TrueHD** | Full decode to LPCM up to 7.1/24/192; **Atmos via the MAT/JOC extension** preserved on passthrough; raw `.thd`/`.mlp` streams; MLP |
| **DTS-HD Master Audio** | Full lossless decode; **core extraction** (fallback to the 1.5 Mbps DTS core) as a distinct, labelled path |
| **DTS:X** | MA-based and IMAX Enhanced; passthrough; decode to the underlying bed |
| **DTS-HD High Resolution / DTS-ES / DTS 96-24 / DTS Express (LBR)** | Full |
| **LPCM** | Up to 32 channels, 8/16/20/24/32-bit int + 32/64-bit float, 8 kHz–768 kHz, big/little endian, signed/unsigned, all QuickTime variants (`twos`, `sowt`, `in24`, `fl32`), Blu-ray LPCM, DVD-Audio LPCM |
| **FLAC** | Up to 8 channels, 4–32-bit, up to 655 kHz; multichannel; embedded cue sheets; Ogg-FLAC; FLAC-in-MP4 |
| **ALAC** | Up to 8 channels, 16/20/24/32-bit |
| **WavPack** | Lossless, hybrid, and lossy modes; DSD in WavPack |
| **Monkey's Audio (APE)** | All compression levels incl. Insane |
| **TAK · TTA · OptimFROG · LA · Shorten · RealAudio Lossless** | Decode |
| **DSD** | DSD64/128/256/512; `.dsf`, `.dff`, DSD-in-WavPack, SACD ISO; **native DSD output (ASIO/WASAPI/ALSA) and DoP**, plus PCM conversion |
| **MPEG-4 ALS · MPEG-4 SLS · WMA Lossless** | Decode |

### 4.2 Lossy

`AC-3` · `E-AC-3` (incl. **JOC/Atmos**) · **`AC-4`** (incl. Atmos, dialogue enhancement) · `DTS` core ·
`AAC-LC` · `HE-AAC v1/v2` · **`xHE-AAC` (USAC)** · `AAC-LD/ELD` · `MP3` (incl. mp3PRO passthrough of the base layer) ·
`MP2` · `MP1` · `Opus` (incl. multichannel, ambisonics, low-delay) · `Vorbis` · `WMA v1/v2/Pro/Voice` ·
`ATRAC1/3/3+/9/X` · `Musepack (MPC) SV7/SV8` · `Speex` · `AMR-NB/WB/WB+` · `EVS` · `G.711/722/723/726/728/729` ·
`GSM`/`GSM-MS` · `QCELP` · `EVRC` · `iLBC` · `Nellymoser` · `ADPCM` (all ~40 variants: IMA, MS, XA, Yamaha,
Creative, Sanyo LD, THP, EA, SWF …) · `DPCM` variants · `Cook`/`RealAudio 28.8/G2/SIPR` · `SBC`/`aptX`/`LDAC`
(Bluetooth sinks) · `TwinVQ` · `QDM2` · `MACE`

### 4.3 Chiptune / tracker (via libopenmpt + game-music-emu plugins)

`MOD XM IT S3M MTM 669 AMF OKT PTM MED DBM` · `SID` · `NSF/NSFE` · `SPC` · `GBS` · `GYM` · `HES` · `VGM/VGZ` ·
`AY` · `KSS` · `PSF/PSF2/MiniPSF` · `USF` · `2SF` · `GSF` · `SNSF` · `MIDI` (with a bundled SoundFont)

### 4.4 Audio capability spectrum

| Dimension | Range that must work |
|---|---|
| Sample rate | 4 kHz → 768 kHz (incl. 44.1/48/88.2/96/176.4/192/352.8/384 and non-standard rates like 37.8 kHz) |
| Bit depth | 8, 16, 20, 24, 32 integer; 32/64-bit float |
| Channels | 1 (mono) → 24 (**22.2 / NHK Super Hi-Vision**), incl. 2.0, 2.1, 3.0, 4.0 quad, 5.0, 5.1, 6.1, 7.1, 7.1.2, 7.1.4, 9.1.6 |
| Channel layouts | All standard masks + **arbitrary/unknown layouts** + **ambisonics** (FOA/HOA, ACN/SN3D) + object-based beds |
| Bitrate | 4 kbps (AMR) → 40 Mbps (uncompressed 24ch/192k) |

---

## 5. Subtitle & caption matrix

| Format | Type | Requirements |
|---|---|---|
| **ASS / SSA** | Text, styled | **libass** — the correctness reference. Must honour MKV **attached fonts**, `\pos/\move/\clip/\t` animation, karaoke, `ScaledBorderAndShadow`, `PlayResX/Y` scaling, `LayoutRes`, drawing commands, embedded VSFilter quirks |
| **SubRip (SRT)** | Text | Encoding auto-detect (**uchardet**) for legacy CP1251/CP1252/Shift-JIS/GBK/Big5; inline HTML tags; malformed timing recovery |
| **WebVTT** | Text | Cue settings, regions, styling, chapters, metadata cues |
| **PGS / SUP** | Bitmap | Blu-ray presentation graphics; palette updates, cropping, forced flag, object composition |
| **VobSub (SUB/IDX)** | Bitmap | DVD subpictures, palette, cropping, forced |
| **DVB Subtitles** | Bitmap | Broadcast |
| **CEA-608 / CEA-708** | Caption | Embedded in H.264/HEVC SEI, in MPEG-TS user data, and in MP4 `c608`/`c708` tracks. **Roll-up, pop-on, paint-on modes.** Frequently the *only* subtitle track on US broadcast recordings — must work. |
| **Teletext** | Caption | EBU, incl. subtitle pages |
| **TTML / IMSC 1.1 / DFXP / SMPTE-TT** | Text, styled | Including image profile |
| **SAMI (SMI)** · **MicroDVD (SUB)** · **MPL2** · **TMP** · **VPlayer** · **JACOsub** · **SubViewer 1/2** · **AQTitle** · **PJS** · **RealText** · **STL (Spruce & EBU)** · **SCC** · **MCC** · **CAP** · **LRC** (lyrics) · **KAR** | Text | Decode |

**Subtitle behaviours that must be correct** (these are where players actually fail):
- **Forced-track auto-selection** when the audio language matches the user's preference.
- **Attached font extraction and use** from MKV — without this, ASS renders in the wrong font, which users experience as "broken subtitles."
- Per-user language preference chains with fallback (`ja > en`; `subs: en-forced > en-full > off`).
- Correct rendering at display resolution ≠ `PlayResX/Y`, including on 4K displays and when the video is scaled.
- Multiple simultaneous subtitle tracks (primary + secondary language for learners).
- External sidecar discovery: `<basename>.<lang>.<forced|sdh|cc>.srt`, subdirectories (`Subs/`, `Subtitles/`),
  and `.idx`+`.sub` pairs.
- Subtitle timing offset, per-file persistence, and speed/FPS correction for mismatched rips.

---

## 6. Quality and resolution spectrum

The player must be correct across the **entire** range, not just the common cases.

### 6.1 Resolution

| Class | Examples | Notes |
|---|---|---|
| Sub-QCIF → QCIF | 128×96, 160×120, 176×144 | Old phone video, security cameras. Must not break the scaler or the UI. |
| SD | 320×240, 352×288, 640×480, 704×576, 720×480 (NTSC), 720×576 (PAL) | **Anamorphic**: 720×480 with 16:9 DAR must display as 16:9, not 3:2 |
| ED / HD | 852×480, 1280×720, 1366×768 | |
| Full HD | 1920×1080, 1920×800/816/872 (letterboxed scope) | |
| 2K / QHD | 2048×1080 (DCI 2K), 2560×1440 | |
| 4K | 3840×2160 (UHD), **4096×2160 (DCI)**, 4096×1716 (DCI scope) | Both must be handled; DCI is wider than UHD |
| 5K / 6K / 8K | 5120×2880, 6144×3160, **7680×4320**, 8192×4320 | 8K HEVC/AV1 must at minimum software-decode without crashing, and hardware-decode where available |
| Beyond | 16K (15360×8640), 16384×16384 | Must not overflow. Guard allocations; degrade gracefully with a stated reason if memory is insufficient. |
| **Odd / non-conformant** | 1279×719, 853×480, odd widths, non-mod-2 dimensions, 1×1 | Must not crash the scaler or produce chroma artefacts |
| **Vertical / square / ultrawide** | 1080×1920, 1080×1080, 3840×1080 (32:9), 5120×1440 | Correct aspect handling and letterbox/pillarbox |
| **Mid-stream resolution change** | Resolution changes at an IDR (common in broadcast TS and in some WebM) | Renderer must reconfigure without stopping playback |
| **Anamorphic / non-square pixels** | SAR ≠ 1:1 (DVD, HDV 1440×1080, DVCPRO HD 1280×1080, XDCAM 1440×1080) | Honour SAR/PAR from `pasp`, MKV `DisplayWidth/Height`, and codec-level VUI |
| **Cropping metadata** | MKV `PixelCrop*`, MP4 `clap` (clean aperture) | Apply, don't ignore |
| **360° / VR** | Equirectangular, cubemap, EAC, fisheye; MKV `Projection` element, MP4 `st3d`/`sv3d` | Detect and offer a viewer; at minimum play flat with a note |
| **3D** | Side-by-side, top-and-bottom, frame-packed, anaglyph, **MVC** (Blu-ray 3D), MKV `StereoMode`, MP4 Apple `vexu` spatial video | Detect the layout; offer 2D extraction (left view) as the default |

### 6.2 Frame rate

Support **all** of: 1 fps and below (timelapse) · 12 · 15 · 16 · 18 · 23.976 (24000/1001) · 24 · 25 ·
29.97 (30000/1001) · 30 · 47.952 · 48 · 50 · 59.94 · 60 · 72 · 90 · 100 · 119.88 · 120 · 144 · 240 · 300 fps.

Plus these behaviours:
- **Exact rational frame rates** — never round 24000/1001 to 23.98 anywhere in the pipeline. Rounding causes
  cumulative A/V drift on long files, which is the classic "audio drifts out of sync after an hour" bug.
- **VFR (variable frame rate)** — phone recordings, screen captures, and MKV without `DefaultDuration`. Must play
  smoothly and seek accurately; must not be "corrected" to CFR on playback.
- **Telecine**: hard 3:2 pulldown, soft pulldown (repeat-field flags), and 2:2 pulldown. Offer inverse telecine.
- **PAL speedup** (24→25 fps with 4% audio pitch shift) — detect and offer pitch-corrected 25→24 playback.
- **Frame-rate matching**: switch the display mode to the content rate where the platform allows
  ([`03`](03-playback-engine.md) §4.2). This is the difference between smooth and judder on 23.976 content.

### 6.3 Bit depth, chroma, and colour

| Dimension | Required coverage |
|---|---|
| Bit depth | 8, 9, 10, 12, 14, 16-bit per component; float pixel formats for intermediates |
| Chroma subsampling | 4:0:0 (monochrome), 4:1:0, 4:1:1, 4:2:0, 4:2:2, 4:4:4 |
| Chroma siting | MPEG-1 (centered) vs MPEG-2 (left-sited) vs co-sited — honour `chroma_location`; getting this wrong shifts colour by half a pixel |
| Alpha | 4:4:4:4 with alpha (ProRes 4444, HAP Alpha, VP9/AV1 alpha, PNG/WebP/AVIF sequences) |
| Colour primaries | BT.709, BT.601 (525 & 625), BT.2020, DCI-P3, Display P3, SMPTE 240M, Film, sRGB, **and unspecified** (must infer sensibly from resolution) |
| Transfer | BT.709, BT.470M/BG, SMPTE 170M/240M, Linear, Log/Log-sqrt, IEC 61966-2-1 (sRGB), IEC 61966-2-4 (xvYCC), BT.1361, BT.2020-10/12, **SMPTE ST 2084 (PQ)**, SMPTE ST 428, **ARIB STD-B67 (HLG)** |
| Matrix | Identity/GBR, BT.709, BT.470BG, SMPTE 170M/240M, YCgCo, BT.2020 NCL, **BT.2020 CL**, SMPTE 2085, chromaticity-derived NCL/CL, ICtCp |
| Range | Limited (16–235) and Full (0–255), for both YUV and RGB; honour `color_range` and MKV `Colour/Range`. **Mismatched range handling is the #1 cause of "washed out" or "crushed blacks" complaints.** |
| HDR static metadata | `MasteringDisplayColourVolume` (ST 2086), `MaxCLL`/`MaxFALL` (ST 2094 CTA-861.3) |
| HDR dynamic metadata | **HDR10+ (ST 2094-40)** in SEI, in MKV BlockAdditions, and in MP4; **Dolby Vision RPU** (all profiles — see [`03`](03-playback-engine.md) §4.1) |

### 6.4 Bitrate and file-size spectrum

| Class | Bitrate | Handling requirement |
|---|---|---|
| Ultra-low | 8 kbps–500 kbps (3GP, AMR, old web video) | Must not over-buffer; must not stall on tiny reads |
| Streaming | 1–20 Mbps | Standard case |
| High-quality encode | 20–60 Mbps | Standard case |
| **Remux / UHD Blu-ray** | **60–150 Mbps sustained, 200 Mbps peak** | **Adaptive read-ahead sized from measured bitrate** (see §6.5). This is the case that breaks other players. |
| Intermediate / mastering | 150 Mbps–1.5 Gbps (ProRes 4444 XQ, uncompressed v210, DPX sequences) | Must play from fast local storage; must fail with a clear I/O-bandwidth reason rather than stuttering silently |
| File size | 1 KB → 2 TB+ | 64-bit offsets everywhere. Seeking in a 500 GB file must not require reading it. |
| Duration | 0.04 s (single frame) → 24 h+ | No 32-bit millisecond overflow (24.8-day limit). Use 64-bit nanosecond timestamps internally. |

### 6.5 Adaptive buffering (the remux-critical implementation detail)

```
readahead_bytes = clamp(
    measured_source_bitrate_bps / 8 * target_seconds,
    min = 8 MiB,
    max = user_cap (default 1 GiB)
)
target_seconds = 5   on fast local storage
               = 20  on LAN network sources
               = 45  on WAN / high-latency / high-jitter sources
```
Measure the source bitrate during the first seconds and re-tune. Expose the buffer fill level and measured
throughput in the Playback Report. **A stutter on a remux must always be attributable to a specific number the
user can see.**

---

## 7. What genuinely cannot work — the honest list

These are the **only** permitted causes of tier **T5**. Each must produce a specific, actionable message. Anything
else reaching T5 is a bug.

| Cause | Message shape | Why it's unfixable |
|---|---|---|
| **DRM-encrypted content** (Widevine, FairPlay, PlayReady, CENC with no key, PIFF, ASF-DRM, AACS/BD+ on an unripped disc) | "This file is DRM-protected. Playback requires the original service's app." | Requires certification and keys we deliberately do not ship ([`08`](08-legal-licensing.md) §4) |
| **Encrypted Matroska tracks** without a key (`ContentEncryption`) | "Track 2 is encrypted and no key is available." | Same |
| **Password-protected/encrypted archives** | "This archive is password-protected." | Same |
| **File truncated before any decodable frame** | "This file appears to be incomplete — no playable data was found in the first N MB." | Nothing to play |
| **Genuinely absent codec** (a format FFmpeg has no decoder for — the list is very short and shrinking) | "This file uses <codec>, which no decoder currently supports. [Report this file]" | Honest gap; the report path feeds the backlog |
| **Hardware insufficient and software too slow** (e.g. 8K AV1 on a 2018 phone) | "Your device can decode this at ~7 fps. Play anyway, or stream a transcoded version from your server?" | Physics — but note this is **T3/T5 with a choice**, never a silent failure |
| **Storage bandwidth insufficient** (1 Gbps ProRes over Wi-Fi) | "This file needs 180 MB/s; your connection is delivering 24 MB/s." | Physics, but stated as a number |

**Note what is _not_ on this list:** damaged headers, missing indexes, wrong extensions, non-conformant muxing,
exotic codec/container pairings, unknown boxes/elements, VFR, mid-stream parameter changes, huge track counts. Those
are all **T4 Recovered** — the player's job. See [`12`](12-container-conformance.md) §5.

---

## 8. Hardware decode policy

1. **Hardware decode is an optimisation, never a requirement.** Every stream has a software path.
2. **Probe real capability, don't assume.** Query `MediaCodecList` (Android), `ID3D11VideoDevice::CheckVideoDecoderFormat`
   (Windows), `VAQueryConfigProfiles` (VAAPI), `VTDecompressionSessionCanAcceptFormatDescription` (Apple) for the
   *exact* profile, level, bit depth, and chroma format — not just the codec name. A device that reports "HEVC"
   frequently means "HEVC Main 8-bit 4:2:0 only."
3. **Fall back mid-stream, not just at open.** A decoder that errors at 00:47:13 must transparently switch to
   software and keep playing, recording the switch in the Playback Report.
4. **Known software-only cases** to plan for: H.264 Hi10P/4:2:2/4:4:4, HEVC 12-bit and 4:4:4 on most GPUs, VC-1
   interlaced on modern GPUs, all Tier-B/C legacy codecs, lossless intermediates.
5. **Copy-back vs zero-copy**: prefer zero-copy surfaces; switch to `-copy` variants automatically when a filter
   chain (deinterlace, crop, shaders needing CPU access) requires system memory.
6. **Decoder slot exhaustion** (Android, 5–16 slots): release decoders eagerly, never hold one for a paused
   background session, and fall back to software rather than failing when slots are exhausted.

---

## 9. How the guarantee is verified

| Mechanism | What it covers |
|---|---|
| **Conformance corpus** ([`../conformance/`](../conformance/)) | ~180 curated vectors across containers, codecs, quality/resolution spectrum, and damage classes. Machine-readable manifests with expected tier, expected streams, expected decode path. Runs on every platform in CI. |
| **Standards conformance suites** | JVET/JCT-VC HEVC & VVC conformance bitstreams · AOM AV1 Argon test vectors · ITU H.264 conformance streams · Matroska test suite (matroska.org) · hubblec4 `Matroska-Playback` segment-linking test files · GPAC ISOBMFF test suite · media.xiph.org samples · FFmpeg FATE |
| **Wild corpus** | A rolling, opt-in-contributed set of real-world files that once broke the player. **Every user-reported playback bug adds a vector permanently.** This becomes the real moat. |
| **Fuzzing** | Structure-aware fuzzing of the demuxer/parser layer (`cargo-fuzz` + AFL++ on FFmpeg's API surface) with corpora derived from the above |
| **Property tests** | For any (source stream set × client capability set), the ladder must emit a plan the client can actually play |
| **Soak** | 24 h continuous playback across a randomized corpus; zero leaks, zero desync |

**Release gate:** 100% of the conformance corpus at its expected tier or better, on every shipped platform. No
exceptions, no waivers.
