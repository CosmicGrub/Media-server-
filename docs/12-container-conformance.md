# 12 — Container Conformance: "Every MP4 and MKV Plays Perfectly"

This is the specification behind the strongest compatibility claim in the product. MP4 and Matroska are ~95% of a
real library, and "it mostly works" is where every competitor sits. This document enumerates the **complete feature
surface** of both containers, marks what must be handled, and names the specific behaviours that cause real players
to fail.

Each row maps to one or more conformance vectors in [`../conformance/`](../conformance/). A row with no vector is a
gap in the test plan, not a gap in the spec.

---

## 1. The two rules that precede everything

### Rule 1 — Content probing, never extension trust
```
open(path) →
  1. read first 64 KiB + last 64 KiB
  2. run full demuxer probe (FFmpeg probe with escalating probesize/analyzeduration)
  3. if score < threshold, retry with probesize=200MB, analyzeduration=200s
  4. if still ambiguous, try the ranked forced-demuxer list (§5.2)
  5. only then report
```
A `.mkv` that is really an MP4 plays. A `.mp4` that is really Matroska plays. A file named `movie` plays. A file
named `movie.txt` plays. Extension informs the *ranking* of probe attempts and nothing else.

### Rule 2 — Unknown is not fatal
Unknown EBML elements, unknown MP4 boxes, unknown codec IDs on a track you don't need, unknown brands, private
extension data — **skip and continue**. A container is a bag of things; only the things you selected must be
understood. A player that aborts on an unrecognised box is broken by design.

---

## 2. Matroska / MKV / WebM conformance surface

### 2.1 EBML layer

| Feature | Requirement | Why players fail |
|---|---|---|
| Variable-length integers, all widths | Full | — |
| **Unknown-size elements** (`Segment`, `Cluster` with size = all-ones) | **Required** | Live/streamed MKV and some muxers emit these. A player that requires known sizes cannot play streamed Matroska at all. |
| `Void` elements | Skip | — |
| `CRC-32` elements | Skip (optionally verify; **never reject on mismatch** — warn) | Verifying and rejecting breaks files that play fine |
| Unknown/future elements | **Skip silently** | Forward compatibility |
| `EBMLMaxIDLength` / `EBMLMaxSizeLength` non-default | Honour | Rare muxers |
| DocType `matroska` **and** `webm` | Both | Must also accept out-of-WebM-subset codecs in a `webm` DocType file — muxers do this |
| DocTypeVersion 1–4+ | All | — |
| **Damaged EBML header** | Recover: scan for the first valid `Segment` or `Cluster` and start there | Partial downloads, bad copies |
| Multiple `Segment` elements in one file | Play the first; expose the rest | Concatenated files |

### 2.2 Blocks, lacing, timestamps

| Feature | Requirement | Why players fail |
|---|---|---|
| `SimpleBlock` | Full, incl. keyframe/invisible/discardable flags | — |
| `BlockGroup` + `Block` | Full | — |
| `BlockDuration` | Honour (required for subtitles and for VFR) | Subtitles vanish or persist forever without it |
| `ReferenceBlock` / `ReferencePriority` | Use for keyframe inference when `SimpleBlock` flags are absent | Old muxers |
| **`BlockAdditions` / `BlockMore` / `BlockAddID`** | **Required** — this is how **HDR10+ dynamic metadata and Dolby Vision RPU are carried in MKV**, and how VP9 alpha is carried | Ignoring these silently loses HDR10+/DV. Very common failure. |
| `BlockAdditionMapping` (track-level) | Parse; map `BlockAddIDType` to the right consumer | — |
| **Xiph lacing / EBML lacing / fixed-size lacing** | **All three required** | Common for audio (esp. Vorbis/AAC/MP3 in MKV). Unhandled lacing = garbled or missing audio. |
| `TimestampScale` — **non-default values** | **Required.** Default is 1,000,000 ns (1 ms) but files exist with 100 ns, 10⁶, 10⁹ | Hard-coding 1 ms breaks timing entirely on these files |
| `TrackTimestampScale` (deprecated) | Honour if present | Old files |
| Negative block timestamps (relative to cluster) | Full 16-bit signed | B-frame ordering |
| **`CodecDelay`** and **`SeekPreRoll`** | **Required** for Opus (and generally) | Opus plays with a click/offset without them |
| Timestamp discontinuities / non-monotonic clusters | Tolerate; resync rather than abort | Concatenated or repaired files |
| Cluster not starting on a keyframe | Tolerate; decode from the previous keyframe | Some muxers |
| `Cluster/Position` and `PrevSize` | Use for recovery when `Cues` are broken | — |

### 2.3 Seeking and indexing

| Feature | Requirement |
|---|---|
| `Cues` present and correct | Fast path |
| **`Cues` absent** | **Required to still seek**: binary search over `Cluster` timestamps by scanning, then refine. Never disable seeking. |
| `Cues` incomplete (only some tracks, or sparse) | Use what exists, scan for the rest |
| `Cues` **wrong** (offsets don't land on clusters) | Detect (element ID mismatch at the target offset), fall back to scanning, and log |
| `Cues` at the end of the file (non-streamable layout) | Over HTTP: range-fetch the tail to find `SeekHead`/`Cues` before playing; do not download the whole file |
| `SeekHead` present / absent / wrong / chained | Use as a hint only; always validate the target element ID |
| Files > 4 GB / > 100 GB | 64-bit offsets throughout |
| Seek in a file with unknown-size Segment (live) | Seek within the buffered range only; report the limit |

### 2.4 Chapters, editions, and segment linking

This is the area with the **worst cross-player support** and is a genuine differentiator. Ordered chapters and linked
segments are widely described as a pain point, and only a handful of players implement them correctly.

| Feature | Requirement | Notes |
|---|---|---|
| Simple chapters (`ChapterTimeStart` only) | Full | Baseline |
| `ChapterTimeEnd` | Honour | — |
| **`EditionEntry`, multiple editions** | Full — expose an edition picker; honour `EditionFlagDefault` | Anime releases ship theatrical/TV editions in one file |
| `EditionFlagOrdered` — **ordered chapters** | **Required.** Build a virtual timeline from chapter entries in order, skipping non-included regions. Evaluate the logical linking **before** playback begins, per the Matroska recommendation. | This is how "same episode, different OP/ED" releases work |
| `ChapterFlagHidden` / `ChapterFlagEnabled` | Honour | — |
| **`ChapterSegmentUID` — hard linking** | **Required.** Resolve to another file in the same directory by Segment UID (scan sibling files, cache the UID→path map). | Multi-file releases |
| **`ChapterSegmentEditionUID`** | **Required.** Per spec: when present, the **entire content of the linked edition must be played**, the chapter's start/end times are **ignored**, and the player must integrate the whole edition's duration into the virtual timeline. | Very commonly implemented wrong |
| `PrevUID` / `NextUID` (soft linking, `SegmentFamily`) | Support: play sequentially across linked segments as one timeline | — |
| **Linking loop detection** | **Required.** Test files exist that construct endless loops. Maintain a visited-UID set with a depth cap; on a cycle, break, play what's resolvable, and warn. | A player without this hangs or OOMs |
| Nested chapters (`ChapterAtom` inside `ChapterAtom`) | Parse the tree; present hierarchically | — |
| `ChapterDisplay` with multiple languages | Select by user preference, fall back sensibly | — |
| `ChapterPhysicalEquiv` | Expose (disc/side/track hints) | Low priority |
| Unresolvable linked segment (file missing) | **Play what exists**, insert a gap or skip, and state it clearly | Never abort |

### 2.5 Tracks and codec mapping

| Feature | Requirement |
|---|---|
| `CodecID` → decoder mapping, **complete table** | `V_MPEG4/ISO/AVC`, `V_MPEGH/ISO/HEVC`, `V_MPEGI/ISO/VVC`, `V_AV1`, `V_VP8`, `V_VP9`, `V_MPEG1`, `V_MPEG2`, `V_MPEG4/ISO/{SP,ASP,AP}`, `V_MPEG4/MS/V3`, `V_REAL/RV*`, `V_THEORA`, `V_PRORES`, `V_QUICKTIME`, `V_DIRAC`, `V_UNCOMPRESSED`, **`V_MS/VFW/FOURCC`** (legacy — parse the embedded `BITMAPINFOHEADER` in `CodecPrivate`), `A_AAC/*`, `A_AC3`, `A_EAC3`, `A_TRUEHD`, `A_MLP`, `A_DTS`, `A_DTS/EXPRESS`, `A_DTS/LOSSLESS`, `A_FLAC`, `A_ALAC`, `A_OPUS`, `A_VORBIS`, `A_PCM/*`, `A_MPEG/L{1,2,3}`, `A_WAVPACK4`, `A_TTA1`, `A_REAL/*`, `A_QUICKTIME`, **`A_MS/ACM`** (parse `WAVEFORMATEX`), `S_TEXT/{UTF8,SSA,ASS,WEBVTT}`, `S_HDMV/PGS`, `S_HDMV/TEXTST`, `S_VOBSUB`, `S_DVBSUB`, `S_KATE`, `S_IMAGE/BMP`, `B_VOBBTN` |
| **Unknown `CodecID`** | Skip that track, play the rest. **Never fail the file.** |
| `CodecPrivate` per codec | Full parsing (AVCC, HVCC, VVCC, AV1C, `vorbis` 3-packet header, Opus head, FLAC STREAMINFO, `BITMAPINFOHEADER`, `WAVEFORMATEX`, DTS/AC3 sync info) |
| Missing/empty `CodecPrivate` | Fall back to in-band parameter sets (AnnexB extraction from the first frames) |
| Track flags: `FlagDefault`, `FlagForced`, `FlagEnabled`, `FlagHearingImpaired`, `FlagVisualImpaired`, `FlagTextDescriptions`, `FlagOriginal`, `FlagCommentary` | **All required** — this is what makes automatic track selection correct |
| `Language` (ISO 639-2) and `LanguageBCP47` | Prefer BCP-47 when present; handle `und`, `mis`, `zxx`, and empty |
| `Name` with UTF-8 including CJK, RTL, emoji | Render correctly in the track picker |
| Very high track counts (100+ audio/sub tracks) | UI and selection logic must scale; grouped, searchable picker |
| Duplicate `TrackNumber` | Tolerate — disambiguate by `TrackUID` |
| `DefaultDuration` present / absent | Absent ⇒ treat as VFR; derive from block timestamps |
| `MinCache`/`MaxCache`, `MaxBlockAdditionID` | Parse |
| `TrackOperation` / `TrackCombinePlanes` (3D plane combining) | Parse; at minimum play the base plane |

### 2.6 Video, audio, and colour elements

| Element | Requirement |
|---|---|
| `PixelWidth`/`PixelHeight` vs **`DisplayWidth`/`DisplayHeight`/`DisplayUnit`** | **Required** — this is Matroska's aspect-ratio mechanism. Units: pixels, cm, inches, DAR. Ignoring it displays anamorphic content at the wrong shape. |
| **`PixelCropTop/Bottom/Left/Right`** | **Required** — crop before display. Ignoring it shows garbage edges. |
| `FieldOrder` | Honour for deinterlacing (progressive, TFF, BFF, and the interleaved variants) |
| **`StereoMode`** (0–14) | Detect 3D layout; default to left-eye 2D extraction with a picker |
| `AlphaMode` + alpha in BlockAdditions | Composite where the renderer supports it |
| **`Colour`** sub-elements | **All required**: `MatrixCoefficients`, `BitsPerChannel`, `ChromaSubsampling{Horz,Vert}`, `CbSubsampling*`, `ChromaSitingHorz/Vert`, `Range`, `TransferCharacteristics`, `Primaries`, `MaxCLL`, `MaxFALL`, and `MasteringMetadata` (all 10 sub-values). Container values **override** in-band VUI when they disagree? **No — prefer in-band, warn on mismatch, expose an override.** (Muxers get this wrong in both directions; in-band is more often right.) |
| **`Projection`** | Detect 360/VR (`rectangular`, `equirectangular`, `cubemap`, `mesh`) with pose; offer a viewer |
| `Audio/SamplingFrequency` vs **`OutputSamplingFrequency`** | **Required** — SBR/HE-AAC doubles the rate. Using the wrong one plays at half speed. |
| `Audio/Channels`, `BitDepth`, `Emphasis` | Honour |
| `Video/FrameRate` (deprecated) | Ignore in favour of computed rate |

### 2.7 Attachments, tags, and encoding

| Feature | Requirement |
|---|---|
| **`Attachments` — font files** (`application/x-truetype-font`, `x-font-ttf`, `font/ttf`, `font/otf`, `application/vnd.ms-opentype`, and the many wrong MIME types muxers emit) | **Required.** Extract and register with libass **before** rendering the first subtitle. Detect by extension *and* magic bytes, not MIME. **Without this, ASS subtitles render in the wrong font — one of the most common "broken subtitles" reports.** |
| Attachments — cover art (`cover.jpg`, `small_cover.png`, and variants) | Use as artwork |
| Attachments — arbitrary files | Expose for extraction; never choke on them |
| `Tags` with nested `Targets`, `TargetTypeValue` (10/20/30/40/50/60/70) | Parse the full hierarchy: collection/season/episode/track-level tags |
| Per-track and per-chapter tags | Parse |
| **`ContentEncoding` → `ContentCompression`** | **Required.** `ContentCompAlgo` 0 = zlib, 3 = **header stripping** (very common for AVC/AAC — the muxer strips a constant prefix from every frame and stores it in `ContentCompSettings`). **Unhandled header stripping = every frame is corrupt.** Also handle bzlib(1) and lzo1x(2) where present. |
| `ContentEncoding` → `ContentEncryption` | Detect, identify the scheme, and report clearly (T5 with a specific message). Never fail mysteriously. |
| Multiple chained `ContentEncoding` with `ContentEncodingOrder` | Apply in order |

### 2.8 Matroska damage and recovery classes

| Damage | Recovery |
|---|---|
| Truncated mid-cluster | Play up to the last complete block; report duration as approximate |
| Truncated before `Cues`/`Tags` (common with partial downloads) | Play fully; seek by scanning |
| Corrupt `SeekHead` offsets | Validate element ID at target; fall back to scan |
| Missing `Duration` | Estimate from the last cluster timestamp or from bitrate × size |
| Zero/absent `TimestampScale` | Assume the 1 ms default |
| Garbage between clusters | Resync by scanning for the `Cluster` element ID |
| Interleaving pathologies (all video then all audio) | Large demux buffer; do not assume tight interleave |
| Overlapping/duplicate blocks | Deduplicate by (track, timestamp) |
| `mkvmerge` "repaired" files with odd structure | Tolerate |

---

## 3. ISOBMFF / MP4 / MOV conformance surface

### 3.1 Box structure

| Feature | Requirement | Why players fail |
|---|---|---|
| 32-bit and **64-bit (`largesize`) box sizes** | Both | >4 GB files |
| `size == 0` (box extends to EOF) | **Required** — legal and common for the final `mdat` | Files with a streaming-written `mdat` |
| `size == 1` + largesize | Required | — |
| **Unknown boxes** | **Skip by size, continue** | Vendor extensions everywhere |
| `uuid` extension boxes | Skip unless recognised (PIFF, Sony, GoPro, Dolby) | — |
| `free` / `skip` / `wide` | Skip | — |
| **`mdat` before `moov`** (non-faststart) | **Required.** Locally: seek to find `moov`. **Over HTTP: range-request the tail** rather than downloading the whole file. | The single most common cause of "the file takes 10 minutes to start streaming" |
| `moov` split or duplicated | Use the last valid one |
| Deeply nested / malformed box trees | Depth cap; skip malformed subtrees, keep the rest |
| Zero-size or negative-computed boxes | Detect and resync by scanning for a known box type |

### 3.2 Timing — the #1 A/V-sync bug source in MP4

| Feature | Requirement |
|---|---|
| **`elst` edit lists** | **Required, fully.** Handle: (a) an **empty edit** at the start = initial delay/silence (extremely common — iTunes/AAC encoder delay, and video/audio offset in phone recordings); (b) media start offset (trim); (c) dwell edits (`media_rate == 0`); (d) multiple edits; (e) `media_rate != 1`; (f) version 1 (64-bit). **Ignoring edit lists is why MP4s play out of sync in lesser players.** |
| **`ctts` composition offsets** | Version 0 (unsigned) **and version 1 (signed)** |
| `cslg` composition-to-decode | Honour when present |
| Track vs movie **timescale mismatch** | Convert exactly with rational arithmetic; never via float |
| `mvhd`/`tkhd`/`mdhd` duration = 0 or `0xFFFFFFFF` | Estimate from sample tables |
| Fractional/odd timescales (1001, 30000, 90000, 44100, 48000, 10000000) | Exact rational handling |
| Negative `ctts` producing negative first PTS | Normalize to zero-based, preserving relative offsets |
| `stts` with many entries (VFR) | Handle efficiently; do not assume CFR |
| **`stss` absent** | Treat all samples as sync (all-intra) — correct for ProRes/DNxHR/MJPEG |
| `sdtp` sample dependency | Use for better seeking |
| **Open-GOP** (`sgpd`/`sbgp` with `roll`/`prol` groups) | Honour the roll distance when seeking — otherwise you get corrupt frames after a seek |

### 3.3 Sample description (`stsd`) and codec configuration

| Feature | Requirement |
|---|---|
| **Multiple `stsd` entries** | **Required** — resolution, codec, or parameter set changes mid-file. The renderer must reconfigure without stopping. |
| `avc1` vs **`avc3`** | `avc1` = out-of-band SPS/PPS in `avcC`; `avc3` = **in-band** parameter sets. Both required. |
| `hvc1` vs **`hev1`** | Same distinction for HEVC |
| `vvc1` / `vvi1` | VVC |
| **`dvh1` / `dvhe` / `dvav` / `dva1` / `dav1`** + `dvcC`/`dvvC`/`dvwC` | Dolby Vision configuration boxes — parse profile/level, locate the RPU, handle dual-track (BL+EL) layouts via `tref` |
| `av01` + `av1C` | AV1 |
| `vp08`/`vp09` + `vpcC` | VP9 |
| `mp4v` + `esds` (all object types) | MPEG-4 ASP and friends |
| Legacy QuickTime video: `rle `, `SVQ1/3`, `cvid`, `jpeg`, `png `, `raw `, `2vuY`, `v210`, `apc*`/`ap4h` (ProRes), `AVdn` (DNxHD), `CFHD` | Required |
| Audio `mp4a` + `esds` (AAC-LC, HE-AAC explicit and **implicit** SBR/PS signalling, xHE-AAC/USAC, MP3-in-MP4 object type 0x6B) | Required — implicit SBR is a classic half-speed bug |
| `ac-3`+`dac3`, `ec-3`+`dec3` (incl. **JOC**), **`ac-4`+`dac4`** | Required |
| **`mlpa`** (TrueHD in MP4) | Required |
| `dtsc`/`dtsh`/`dtsl`/`dtse` + `ddts` | DTS family in MP4 |
| `Opus`+`dOps`, `fLaC`+`dfLa`, `alac`+`alac` | Required |
| **PCM**: `ipcm`/`fpcm`+`pcmC`, and QuickTime `twos`, `sowt`, `in24`, `in32`, `fl32`, `fl64`, `lpcm`+`wave`/`chan` | Required — QuickTime PCM variants are a common gap |
| `sowt`/`chan` channel layout box | Honour for >2 channel PCM |
| `wave` atom with embedded `esds`/`frma`/`enda` (QuickTime) | Parse |
| `pasp` (pixel aspect ratio) | **Required** — anamorphic |
| **`clap` (clean aperture)** | **Required** — crop to it |
| `colr` (`nclx`, `nclc`, `prof` ICC, `rICC`) | **Required** — colour tagging; `nclc` lacks a range flag, so infer |
| **`mdcv` + `clli`** (and legacy `SmDm`/`CoLL`) | HDR static metadata |
| `st3d` / `sv3d` (Google spherical/3D) | 360 and stereo detection |
| Apple **`vexu`** spatial video | Detect; play as 2D by default |
| `btrt` bitrate box | Informational |

### 3.4 Fragmented MP4, DASH, CMAF

| Feature | Requirement |
|---|---|
| `moof`/`mfhd`/`traf`/`tfhd`/`trun` | Full, incl. all optional flags and default-base-is-moof |
| **`tfdt`** (track fragment decode time) | Required — baseline for segment timing |
| `mvex`/`trex` defaults | Required |
| `sidx` / `ssix` segment index | Use for seeking |
| `mfra`/`tfra`/`mfro` | Use for seeking in complete fMP4 files |
| `styp` segment type | Parse |
| Init segment + media segments (separate files) | Required — CMAF/DASH |
| Init segment **changing mid-stream** (codec/resolution switch) | Reconfigure without stopping |
| Live/dynamic manifests, multi-period, gaps | Required |
| Fragments with no `moov` at all (raw segment handed over) | Recover using a supplied or inferred init |

### 3.5 Tracks, references, chapters, metadata

| Feature | Requirement |
|---|---|
| Disabled tracks (`tkhd` flags bit 0 clear) | Do not auto-select; still expose |
| `tref` types: **`chap`** (chapter track), `hint`, `cdsc`, `fall`, `subt`, `dpnd`, `ipir`, `mpod`, `vdep`, `scal` | Parse; use `chap` for chapters, skip `hint` tracks |
| **Chapters, all three mechanisms**: QuickTime text track via `tref chap`, Nero **`chpl`**, and `HDLR`/`meta`-based | **All required** — different tools write different ones |
| Timed metadata tracks (`meta`/`mebx`, GoPro **GPMF**, camera telemetry) | Parse or skip cleanly; expose where useful |
| `udta`/`meta`/`ilst` iTunes metadata (`©nam`, `©ART`, `covr`, `desc`, `ldes`, `tvsh`, `tven`, `stik`, `hdvd`, custom `----` atoms) | Read for library metadata |
| 3GPP metadata (`titl`, `auth`, `perf`, `yrrc`, `loci`) | Read |
| **Cover art** in `covr` (JPEG/PNG, multiple) | Extract |
| Multiple `trak` with the same ID | Tolerate, disambiguate |
| **Reference movies** (`rmra`/`rmda`/`rdrf` pointing to external files) | Resolve relative paths; if unresolvable, report clearly rather than showing a black screen |
| Brands (`ftyp`): `isom iso2 iso4 iso5 iso6 iso8 mp41 mp42 avc1 M4V M4A M4B qt 3gp4 3gp5 3g2a mmp4 dash cmfc cmf2 dby1 msdh msix heic mif1 avif` and unknown brands | Never gate behaviour on brand alone |
| `ftyp` **absent** | Probe anyway — many QuickTime files lack it |

### 3.6 Encryption detection (must be explicit, never mysterious)

| Feature | Requirement |
|---|---|
| `sinf`/`frma`/`schm`/`schi`, `pssh`, `senc`, `saiz`/`saio`, `tenc` | Detect and identify the scheme |
| CENC schemes `cenc`, `cbc1`, `cens`, `cbcs`; PIFF `uuid` | Identify by name in the message |
| Partially-encrypted files (clear leader) | Play the clear portion, then report at the boundary |
| **Behaviour** | T5 with: *"This file is DRM-protected (Widevine `cenc`). Playback requires the original service's app."* Never a decode-garbage-and-crash path. |

### 3.7 MP4 damage and recovery classes

| Damage | Recovery |
|---|---|
| **Truncated during recording** (`moov` never written — camera/phone crash, unfinished download) | **Required to recover**: reconstruct sample tables by scanning `mdat` for codec sync patterns; this is the `untrunc`/`recover_mp4` technique. High-value: these are irreplaceable user files. |
| Truncated `mdat` (moov intact) | Play to the last complete sample; report duration honestly |
| `stco`/`co64` offsets wrong (file was edited/concatenated) | Detect (offset doesn't land on a plausible sample), rebuild by scanning |
| `stsz` sample sizes inconsistent with `mdat` | Clamp; skip bad samples |
| Interleaving pathologies (audio at the end) | Large buffer; do not assume interleave |
| Concatenated MP4s (two `ftyp`/`moov` pairs) | Play the first; expose the second |
| Zero-duration or negative-duration tracks | Recompute from sample tables |
| `mdat` with `size == 0` and no EOF | Read to EOF |

---

## 4. Track auto-selection — correctness rules

Getting this wrong is experienced as "the player picked the wrong audio/subtitles," which users rate as a
compatibility failure even though everything decoded fine.

**Audio selection**, in priority order:
1. User's explicit per-file override (persisted by file identity)
2. Per-library / per-user preferred language chain, matched against `Language` and `LanguageBCP47`
3. `FlagOriginal` when the user prefers original-language audio
4. Prefer the **highest-fidelity** track in the preferred language, using the ladder: lossless > lossy-HD > lossy;
   then higher channel count; then higher bitrate — **but only if the sink can actually take it** ([`03`](03-playback-engine.md) §5.4)
5. Exclude `FlagCommentary` and `FlagVisualImpaired` unless explicitly preferred
6. `FlagDefault`
7. First track

**Subtitle selection**, in priority order:
1. User's explicit per-file override
2. **Forced-subtitle rule**: if the selected audio language matches the user's preferred language **and** a
   `FlagForced` track exists in that language → select it. (This is the "foreign dialogue only" case and it is the
   single most-appreciated automatic behaviour in a media player.)
3. If the audio language does **not** match the preference → select a full subtitle track in the preferred language
4. Prefer non-SDH over SDH unless the user prefers SDH (`FlagHearingImpaired`)
5. Prefer text (ASS/SRT) over bitmap (PGS/VobSub) when both exist in the same language — bitmap can't be restyled
6. `FlagDefault`, then off

**Never** select a track the current sink/renderer cannot handle without first checking, and always record the
selection rationale in the Playback Report.

---

## 5. The universal recovery ladder

Applied on open and continuously during playback. Each rung is attempted automatically; the rung reached is recorded
and, if above 0, surfaced as tier **T4** with an explanation.

| Rung | Action |
|---|---|
| **0** | Normal probe and open |
| **1** | Escalate probe: `probesize=200M`, `analyzeduration=200M`, re-probe |
| **2** | Enable tolerant demux flags: `+genpts +igndts +discardcorrupt +nobuffer` as appropriate; ignore the index and scan |
| **3** | Force the ranked alternative demuxer list for the detected magic bytes (e.g. try `mov,mp4,m4a` → `matroska` → `mpegts` → `avi` → `asf` → `flv`) |
| **4** | **Index reconstruction**: for MP4, rebuild sample tables by scanning `mdat` for codec sync patterns; for MKV, scan for `Cluster` IDs and build a synthetic Cues table; for AVI, rebuild from `idx1` or scan chunk headers |
| **5** | **Raw elementary stream fallback**: probe the payload as a headerless ES (H.264/HEVC/MPEG-2/AC-3/AAC/MP3/DTS) and play with generated timestamps |
| **6** | **Codec parameter inference**: no extradata → extract SPS/PPS/VPS from the first frames; unknown resolution → take it from the first decoded frame |
| **7** | **Per-packet error concealment**: on decode error, conceal (previous-frame copy / motion-compensated), continue; on repeated errors, skip to the next keyframe |
| **8** | **Decoder escalation**: hardware decoder error → recreate the decoder → still failing → fall back to software at the next keyframe, without interrupting audio |
| **9** | **Stream isolation**: if one track is unrecoverable, drop it and keep playing the rest (video-only or audio-only is far better than nothing) |
| **10** | **Repair transcode**: as a last resort, pipe through a repair pipeline (`-err_detect ignore_err -fflags +genpts+discardcorrupt`) to a temporary playable stream |

**Invariant:** reaching rung 10 and failing is the *only* path to T5 for a non-DRM file, and it must produce a
specific message plus a one-click "send this file's first 2 MB as a bug report" action (opt-in, with the media
payload stripped — headers only).

---

## 6. Timestamp and A/V-sync correctness

These are the bugs users describe as "it plays but it's broken," and they are compatibility failures.

| Rule | Detail |
|---|---|
| **64-bit rational timestamps end to end** | Never store time as 32-bit ms (overflows at 24.8 days) or as `float`/`double` seconds (accumulates error). Use `i64` nanoseconds or an explicit rational. |
| **Never round frame rates** | 24000/1001 stays rational from demux to render. Rounding to 23.976 drifts ~1 frame per 40 minutes. |
| **Honour every offset mechanism** | MP4 `elst` + `ctts` + `cslg`; MKV `CodecDelay` + `SeekPreRoll` + block timestamps; TS PCR/PTS/DTS; encoder delay in AAC/MP3 (LAME/iTunes gapless tags) |
| **Discontinuity handling** | On a PTS jump beyond a threshold, resync rather than stall or fast-forward. Common in TS captures and concatenated files. |
| **Wraparound** | MPEG-TS 33-bit PTS wraps every ~26.5 h — detect and unwrap |
| **A/V drift monitoring** | Continuously measure audio-clock vs video-clock divergence; correct by dropping/duplicating video or by audio resampling (never by pitch shift). Expose the measured drift in the Playback Report. |
| **Audio-clock master by default** | Video follows audio, except in display-sync mode |
| **Gapless** | Honour LAME/iTunes/Ogg/Opus pre-skip and padding for gapless album playback |

---

## 7. Conformance vectors for this document

Every requirement above maps to at least one vector in [`../conformance/corpus.yaml`](../conformance/corpus.yaml).
Coverage summary:

| Group | Defined | Target | Covers |
|---|---:|---:|---|
| `mkv-core` | 19 | 24 | Lacing, unknown-size, TimestampScale, BlockAdditions, CodecDelay, header stripping, attachments |
| `mkv-chapters` | 6 | 12 | Ordered chapters, hard/soft linking, edition selection, loop detection |
| `mkv-damage` | 5 | 10 | Truncation, missing Cues, corrupt SeekHead, garbage, concatenation |
| `mp4-core` | 17 | 23 | Edit lists, ctts v0/v1, multiple stsd, avc3/hev1, PCM variants, chapters ×3, mdat-before-moov |
| `mp4-fragmented` | 4 | 9 | moof/tfdt/sidx, init switching, live, raw segments |
| `mp4-damage` | 4 | 8 | Missing moov reconstruction, bad stco, truncation, concatenation |
| `codec-video` | 9 | 38 | The §3 matrix of [`11`](11-compatibility-charter.md), incl. Hi10P, 4:4:4, VC-1 interlaced, VVC, MVC |
| `codec-audio` | 9 | 26 | Lossless family, passthrough, DSD, 22.2, implicit SBR, ambisonics |
| `subtitles` | 5 | 14 | ASS+fonts, PGS, CEA-608/708, forced logic, encodings |
| `spectrum` | 9 | 18 | Resolution extremes, VFR, telecine, anamorphic, 3D, 360, HDR variants |
| **Total** | **87** | **182** | |

Vectors below the target are declared as `status: planned` stubs in the manifest, so gaps are visible rather than
implicit. The structural container groups — where "every MP4 and MKV plays" actually lives — are near complete; the
codec groups carry the remaining gap because they enumerate the full matrix from [`11`](11-compatibility-charter.md).

See [`../conformance/README.md`](../conformance/README.md) for the manifest schema and the runner contract.

## Sources
- [Matroska — Chapters (ordered, linked, ChapterSegmentEditionUID semantics)](https://www.matroska.org/technical/chapters.html)
- [Matroska — Element Ordering](https://www.matroska.org/technical/ordering.html)
- [Matroska — Data Layout](https://www.matroska.org/technical/diagram.html)
- [hubblec4/Matroska-Playback — ChapterSegmentLinking, incl. endless-loop test cases](https://github.com/hubblec4/Matroska-Playback/blob/master/src/ChapterSegmentLinking.md)
- [mpv issue #11780 — Matroska playback edge cases](https://github.com/mpv-player/mpv/issues/11780)
- [MKV linked segment playback — VideoHelp](https://forum.videohelp.com/threads/347531-MKV-Linked-Segment-Playback)
- [FFmpeg 8.0 "Huffman" — APV, ProRes RAW, RealVideo 6, VVC SCC, VVC VA-API, Vulkan compute codecs](https://ffmpeg.org/pipermail/ffmpeg-devel/2025-August/347886.html)
- [FFmpeg 8.0 release coverage — AV1 Vulkan encode, VVC VA-API decode](https://9to5linux.com/ffmpeg-8-0-huffman-released-with-av1-vulkan-encoder-vvc-va-api-decoding)
