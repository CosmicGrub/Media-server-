# 13 — Remux & Transcode Compatibility Matrices

Direct Play is the goal; remuxing is the cheap fallback; transcoding is the last resort. This document specifies
exactly what can be remuxed into what, what must be transcoded and to what, and how quality is preserved at every
step.

Governing rule from [`11-compatibility-charter.md`](11-compatibility-charter.md): **always achieve the lowest tier
number the chain supports, and state every compromise.**

---

## 1. Remux legality matrix — codec → container

Remuxing rewrites the container without touching elementary streams. It costs ~1–3% of one CPU core and is
**always** preferable to transcoding. The constraint is which codecs each container can legally carry, and — more
practically — which combinations real decoders accept.

Legend: ✅ standard · 🟡 legal but poorly supported by third-party clients · ❌ not possible

| Codec | MKV | MP4/fMP4 | MPEG-TS | WebM | HLS-TS | HLS-CMAF | DASH |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| **H.264 / AVC** | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |
| **HEVC** | ✅ | ✅ | ✅ | ❌ | 🟡 | ✅ | ✅ |
| **HEVC + Dolby Vision RPU** | ✅ | ✅ (`dvh1`/`dvhe`) | 🟡 | ❌ | ❌ | 🟡 | 🟡 |
| **AV1** | ✅ | ✅ (`av01`) | 🟡 | ✅ | ❌ | ✅ | ✅ |
| **VP9** | ✅ | ✅ (`vp09`) | ❌ | ✅ | ❌ | 🟡 | ✅ |
| **VP8** | ✅ | 🟡 | ❌ | ✅ | ❌ | ❌ | 🟡 |
| **VVC / H.266** | ✅ | ✅ (`vvc1`) | 🟡 | ❌ | ❌ | 🟡 | 🟡 |
| **MPEG-2 video** | ✅ | 🟡 | ✅ | ❌ | 🟡 | ❌ | ❌ |
| **MPEG-4 ASP** | ✅ | ✅ | 🟡 | ❌ | ❌ | ❌ | ❌ |
| **VC-1** | ✅ | 🟡 | ✅ | ❌ | ❌ | ❌ | ❌ |
| **ProRes / DNxHR / FFV1** | ✅ | ✅ (MOV) | ❌ | ❌ | ❌ | ❌ | ❌ |
| | | | | | | | |
| **AAC (LC/HE/xHE)** | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |
| **AC-3** | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |
| **E-AC-3 (+JOC/Atmos)** | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |
| **AC-4** | ✅ | ✅ | ✅ | ❌ | 🟡 | ✅ | ✅ |
| **Dolby TrueHD (+Atmos)** | ✅ | 🟡 (`mlpa`) | 🟡 | ❌ | ❌ | ❌ | ❌ |
| **DTS / DTS-HD MA / DTS:X** | ✅ | 🟡 (`dtsc`/`dtsh`/`dtsl`) | ✅ | ❌ | ❌ | ❌ | ❌ |
| **FLAC** | ✅ | ✅ (`fLaC`) | ❌ | ❌ | ❌ | 🟡 | 🟡 |
| **ALAC** | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Opus** | ✅ | ✅ (`Opus`) | 🟡 | ✅ | ❌ | 🟡 | ✅ |
| **Vorbis** | ✅ | 🟡 | ❌ | ✅ | ❌ | ❌ | 🟡 |
| **MP3** | ✅ | ✅ | ✅ | ❌ | ✅ | 🟡 | 🟡 |
| **LPCM** | ✅ | ✅ | 🟡 | ❌ | ❌ | ❌ | ❌ |
| | | | | | | | |
| **SRT** | ✅ | 🟡 (`tx3g` lossy conv.) | ❌ | ❌ | ✅ (WebVTT) | ✅ (WebVTT) | ✅ |
| **ASS / SSA** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **WebVTT** | ✅ | ✅ (`wvtt`) | ❌ | ✅ | ✅ | ✅ | ✅ |
| **PGS (Blu-ray)** | ✅ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **VobSub** | ✅ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **CEA-608/708** | ✅ (in-band) | ✅ (in-band/`c608`) | ✅ | ❌ | ✅ | ✅ | ✅ |
| **TTML / IMSC** | ✅ | ✅ (`stpp`) | ❌ | ❌ | 🟡 | ✅ | ✅ |

### 1.1 The consequential rows

Three cells in that table drive most real-world transcoding, and each has a better answer than "transcode":

**MKV is the universal container.** Anything remuxes *into* MKV. When a client can take MKV, remuxing is almost
always available — this is why MKV-capable clients (libmpv-based ones, i.e. all of ours) reach T0/T1 far more often
than browser-based ones.

**ASS/SSA and PGS cannot enter MP4/HLS.** Do **not** burn them in as the first response. Ladder:
1. Send the subtitle as a **separate out-of-band stream** the client renders itself (our clients all can — libass
   natively, WASM libass on web). **This is the answer 95% of the time.**
2. Convert to WebVTT if the source is text-based and styling loss is acceptable (log the loss).
3. For PGS to a client that cannot render bitmap subs: send as a **separate image-based track** (IMSC image profile)
   where supported.
4. Burn in **only** as a last resort, only with the reason stated, and never for the default track.

**TrueHD/DTS-HD cannot enter HLS/DASH.** Ladder:
1. Direct Play the original container (MKV over HTTP range) — no HLS involved. Available to every native client.
2. Passthrough to the sink if it decodes ([`03`](03-playback-engine.md) §5.4).
3. Decode to **LPCM at full channel count** and remux — preserves every bit of the lossless audio; only loses
   object-based Atmos/DTS:X positioning. This is **T2**, not T3.
4. **DTS-HD MA → extract the DTS core** (1.5 Mbps, lossy, but the original bitstream, no re-encode).
5. TrueHD → its embedded **AC-3 core** where present, same principle.
6. Transcode to E-AC-3 5.1/7.1 (keeps channels; **only path that can carry Atmos to tvOS**, via JOC — see §4.3).
7. Transcode to AAC. Never the first choice; never to stereo unless the sink is stereo.

---

## 2. The remux decision procedure

```
remux_target(source_streams, client_caps) -> Option<RemuxPlan>:

  1. If client accepts the source container AND every selected stream:
         → None  (Direct Play, T0)

  2. Candidate containers = client.containers ∩ containers_that_accept(selected_streams)
     Rank by: MKV > fMP4 > MP4 > TS   (fewest constraints first)

  3. For the best candidate:
       a. every selected stream legal in that container?     → RemuxPlan (T1)
       b. only subtitles illegal?                            → RemuxPlan + out-of-band subtitle delivery (T1)
       c. only audio illegal, and a bitstream-preserving
          extraction exists (DTS core, AC-3 core, LPCM decode)? → RemuxPlan + audio adaptation (T2)
       d. otherwise                                          → fall through to transcode (T3)

  4. Record the reason for every candidate rejected.
```

**Cheap wins that avoid transcoding entirely and are frequently missed:**
- **Stream subsetting** — dropping 9 unused audio tracks and 30 subtitle tracks from a remux cuts the delivered
  bitrate meaningfully and requires no re-encode.
- **`faststart` rewrite** — moving `moov` to the front of an MP4 costs one file copy and turns an unstreamable file
  into a streamable one. Cache the result.
- **Index injection** — writing a Cues table into a Cues-less MKV makes it seekable over HTTP for thin clients.
- **AnnexB ↔ AVCC/HVCC repackaging** — a bitstream filter (`h264_mp4toannexb`, `hevc_mp4toannexb`, and the reverse),
  not a re-encode. This is what makes MP4↔TS↔MKV interchange free.
- **Timestamp normalization** — regenerating PTS/DTS for a file with broken timing is a remux, not a transcode.

---

## 3. Transcode decision matrix — video

Reached only when remuxing cannot satisfy the client. Every entry preserves as much as possible.

| Source | Target client capability | Action | Tier |
|---|---|---|:--:|
| Any | Accepts source codec/profile/level/resolution/bitrate | **Copy** | T0/T1 |
| HEVC Main10 4K HDR10 | HEVC Main10 + HDR display | Copy | T0 |
| HEVC Main10 4K HDR10 | HEVC Main10, **SDR display** | Copy video, **tone map at the client** if it can (libplacebo); server-side tone map only if not | T1/T3 |
| HEVC Main10 | H.264 only | Transcode → H.264 High 10 if supported, else High 8-bit + **dithered** 10→8 conversion | T3 |
| HEVC 4:2:2 / 4:4:4 / 12-bit | Anything mainstream | Transcode → same codec 4:2:0 10-bit, or H.264 High | T3 |
| **H.264 Hi10P** | 8-bit-only decoder | Transcode → H.264 High 8-bit with dithering (never naive truncation — it bands) | T3 |
| AV1 | No AV1 | Transcode → HEVC if available, else H.264 | T3 |
| VP9 | No VP9 | Transcode → H.264/HEVC | T3 |
| VC-1 / MPEG-2 / MPEG-4 ASP | Modern client | Transcode → H.264 (these are low-bitrate sources; transcode is cheap) | T3 |
| **VC-1 or MPEG-2 interlaced** | Any | **Deinterlace (bwdif) then encode.** Detect field order from the container and the bitstream; never assume TFF. | T3 |
| Resolution > client max | — | Downscale with a high-quality scaler (`lanczos`/`spline36`), preserving DAR, never upscaling | T3 |
| Bitrate > client/network max | — | Re-encode with a **CRF-first, VBV-capped** rate control. Never fixed-bitrate. Never exceed source bitrate. | T3 |
| **Dolby Vision P5/P7/P8** | Non-DV client | P8/P7: deliver the **HDR10-compatible base layer** (a remux, not a transcode). P5: convert to HDR10 or tone map — P5 has no HDR10-compatible base. | T1/T3 |
| HDR10+ | Non-HDR10+ client | Strip dynamic metadata, keep HDR10 static (remux) | T1 |
| 3D MVC / SBS / TAB | 2D client | Extract/crop the left view — a filter, not a full re-encode where the layout allows | T3 |
| Anamorphic (SAR≠1) | Client that ignores SAR | Scale to square pixels once, at the right size | T3 |
| VFR | Client requiring CFR | Convert with `fps` filter + correct frame duplication; **never** by naive timestamp rewriting | T3 |

### 3.1 Encoder selection (LGPL-safe — see [`adr/0002`](adr/0002-lgpl-only-build.md))

| Priority | Encoder | Notes |
|---|---|---|
| 1 | **Hardware**: NVENC · QSV · VAAPI · AMF · VideoToolbox | Real-time, LGPL-compatible, the right answer on a server. Quality tuning matters: use VBR-HQ / ICQ / CQP modes, not CBR. |
| 2 | **SVT-AV1** (BSD) | Excellent quality/speed for AV1 targets |
| 3 | **libaom** (BSD) | Reference AV1; slow |
| 4 | **libvpx** (BSD) | VP8/VP9 |
| — | ~~x264 / x265~~ | **GPL — not shipped.** See ADR-0002. If CPU-only H.264 quality proves inadequate in spike S4, ship x264 as a separately-distributed, user-installed GPL component invoked across a process boundary. |

**Rate control policy:** CRF/CQ-first with a VBV cap derived from the client's declared max bitrate. Two-pass only
for offline "optimize" jobs, never for live sessions. Never upscale resolution or increase bitrate above source.

### 3.2 HDR tone mapping (transcode path)

Tone mapping HDR→SDR during transcode is where servers visibly fail — "washed out" and "grey" complaints trace to a
naive clip.

| Requirement | Detail |
|---|---|
| **Hardware-accelerated** | libplacebo (Vulkan) / OpenCL. The software `zscale`+`tonemap` path is 10–20× slower and cannot sustain real-time 4K. |
| **Curve** | BT.2390 EETF by default; expose `spline`, `mobius`, `hable`. Never a naive clamp. |
| **Peak detection** | Use static metadata (`mdcv`/`clli`) when present; dynamic peak detection otherwise |
| **Gamut** | Explicit BT.2020 → BT.709 gamut mapping (perceptual or relative), not a matrix multiply with clipping |
| **Dynamic metadata** | Consume HDR10+/DV RPU per-scene where available — better results than static |
| **Verification** | Automated: transcode a reference HDR clip, compare against a golden SDR render with a perceptual metric (SSIMULACRA2 / Butteraugli). Regression = P1. |

---

## 4. Transcode decision matrix — audio

**Governing principle: audio transcoding costs ~2% of a CPU core; video transcoding costs a GPU. Always prefer
adapting audio over touching video.**

| Source | Sink capability | Action | Tier |
|---|---|---|:--:|
| TrueHD Atmos 7.1 | Sink decodes TrueHD (IEC 61937 HBR) | **Bitstream passthrough** | T0 |
| TrueHD Atmos 7.1 | LPCM 7.1 sink (or macOS/iOS) | **Decode to LPCM 7.1 at source rate/depth.** Lossless; loses only Atmos object positioning. | T2 |
| TrueHD Atmos 7.1 | Stereo sink / headphones | Decode → downmix with the user's chosen matrix (Lt/Rt or Lo/Ro), or **binaural render** | T3 |
| TrueHD | HLS delivery required | Embedded AC-3 core (remux) → else E-AC-3 → else AAC | T2/T3 |
| DTS-HD MA 7.1 | Sink decodes DTS-HD | Passthrough | T0 |
| DTS-HD MA 7.1 | DTS core only | **Extract the DTS core** — original bitstream, no re-encode | T2 |
| DTS-HD MA | LPCM sink | Decode to LPCM at full channels | T2 |
| DTS:X | Non-DTS:X sink | Fall back to the MA lossless bed, then the core | T2 |
| E-AC-3 JOC (Atmos) | E-AC-3 sink | Passthrough — **Atmos survives** | T0 |
| **Any Atmos source** | **tvOS / Apple TV** | Only Atmos-capable path is **E-AC-3 JOC or AC-4**. Requires a licensed Dolby encoder — a licensing decision, not an engineering one ([`08`](08-legal-licensing.md) §2). Otherwise decode → LPCM/AAC multichannel. | T2/T3 |
| FLAC / ALAC / LPCM 24/192 | Bit-perfect-capable device | **Exclusive mode, no resampling, sample-rate switch** | T0 |
| FLAC 24/192 | 48 kHz-max sink | Resample with a high-quality SoX/`swr` resampler (never a naive drop), dither to target depth | T2 |
| DSD | Native DSD or DoP sink | Native / DoP | T0 |
| DSD | PCM-only | Convert to 24/176.4 PCM | T2 |
| Multichannel AAC/Opus | Fewer channels | Downmix with the correct matrix + LFE handling + dialogue-clarity option | T3 |
| Any | Web browser | AAC-LC or Opus; **preserve channel count where the browser supports it** | T3 |
| 22.2 / ambisonics | Standard sink | Render/fold down with the correct decoder matrix | T3 |

### 4.1 Audio quality rules
- **Never resample unnecessarily.** If the sink supports 44.1 kHz, do not force 48 kHz.
- **Always dither** when reducing bit depth (TPDF, optionally noise-shaped).
- **Never go straight to stereo AAC.** The ladder is: copy → core extraction → LPCM → E-AC-3 (keeps channels) →
  multichannel AAC → stereo AAC.
- **Preserve loudness metadata** (dialnorm, R128) rather than re-normalizing.
- **Never pitch-shift** to correct A/V drift; correct with video timing or high-quality resampling.

---

## 5. Subtitle handling matrix

| Source | Client capability | Action | Loss |
|---|---|---|---|
| ASS/SSA | Renders ASS (all our native clients, web via WASM libass) | **Send as-is + all attached fonts** | None |
| ASS/SSA | Text subtitles only | Convert to WebVTT/SRT | Styling, positioning, animation — **log it** |
| ASS/SSA | No subtitle support | Burn in — **last resort only** | Everything; irreversible for that session |
| SRT | Any text-capable | Send as-is or convert charset to UTF-8 | None |
| PGS | Renders bitmap subs | Send as a separate stream | None |
| PGS | Bitmap-capable via IMSC image profile | Convert to IMSC-image | None (repackaging) |
| PGS | Text-only client | **OCR to text** as a background job (offer it; never do it inline), else burn in | OCR errors, or burn-in |
| VobSub | Same as PGS | Same ladder | Same |
| CEA-608/708 embedded | Client extracts in-band | Leave in-band | None |
| CEA-608/708 | Client cannot | Extract to WebVTT server-side | Roll-up animation |
| Any | HLS/DASH delivery | Out-of-band WebVTT or IMSC segments (`stpp`/`wvtt`) | Per above |

**Burn-in is always tier T3 and always requires a stated reason.** It also forces a video transcode, which is why
avoiding it is worth real engineering effort.

---

## 6. Segmented delivery (HLS / DASH / CMAF)

| Concern | Specification |
|---|---|
| **Packaging** | **CMAF/fMP4** as the single segment format, served as both LL-HLS and DASH from one segment set. Avoids duplicate storage and duplicate bugs. |
| Segment duration | 2 s default (4 s for high-bitrate remux copy sessions to reduce overhead). Measure the seek/latency/overhead trade-off — research item R17. |
| Init segments | One per rendition; regenerate on any codec/resolution change |
| **Copy sessions** | When only the container changes, run a **remux-to-CMAF** session — no encoder, ~1% CPU, full quality |
| Seeking ahead | Reuse the existing session and jump the encoder's input position; keep an LRU of produced segments so rewinds are free |
| Throttling | Pause the encoder when the client is buffered ahead by N segments; adaptive, not a fixed sleep |
| Discontinuities | Signal `EXT-X-DISCONTINUITY` on parameter changes rather than forcing a full re-encode |
| Subtitles | Separate WebVTT/IMSC segment tracks; never muxed into the media segments |
| Multi-rendition | Generate on demand, not eagerly; prefer a single high rendition + client-side adaptation on LAN |
| **Session ownership** | One supervised subprocess per session, hard-killed on disconnect, with an orphan reaper. A transcoder crash never affects the server ([`05`](05-server-library.md) §8.1). |

---

## 7. Offline "optimize" jobs (explicit, user-initiated)

Distinct from live transcoding: these are batch jobs the user asks for, and they may take as long as needed.

| Job | Purpose |
|---|---|
| **Create a compatibility version** | Generate an H.264/AAC MP4 alongside the remux so thin clients Direct Play instead of live-transcoding. The single highest-value optimization for a mixed-client household. |
| **DV P7 → P8.1 conversion** | Via `dovi_tool` (plugin) — makes Profile 7 remuxes single-layer and far more widely playable. Lossless to the base layer. |
| **Faststart rewrite** | `moov` to the front, cached |
| **Cues/index injection** | Makes Cues-less MKVs seekable for thin clients |
| **Stream subsetting** | Strip unwanted language tracks into a lean copy |
| **Subtitle extraction + OCR** | PGS → SRT as a durable sidecar |
| **Loudness analysis** | R128 measurement stored as metadata; applied as gain at playback, **never re-encoded** |
| **Repair** | Rebuild a damaged file's index/moov into a clean copy, preserving all streams |

All are queued, resumable, idle-scheduled, and **never destructive** — they write new files and never modify the
source. Source deletion is always a separate, explicit user action.

---

## 8. Verification

| Check | Method | Gate |
|---|---|---|
| **Remux fidelity** | For every remux path in §1: remux, then compare elementary streams byte-for-byte against the source (`ffmpeg -c copy` + stream hash). **Any mismatch is a P0.** | Blocking |
| **Ladder correctness** | Property test: for every (source stream set × client capability set) in the corpus, the emitted plan must be playable by that capability set. Exhaustive over the corpus. | Blocking |
| **No-needless-transcode** | For each corpus file × each of our own clients, assert the achieved tier equals the expected tier in the manifest. A regression from T1 to T3 fails the build. | Blocking |
| **Transcode quality** | SSIMULACRA2 / VMAF against golden renders for the tone-mapping and downscale paths | P1 regression |
| **Audio bit-exactness** | For T0/T1 audio paths, capture the sink-bound bitstream and compare to source | Blocking |
| **A/V sync** | Automated drift measurement over 2 h playback of each corpus file; fail above ±20 ms drift | Blocking |
| **Direct Play rate** | Track the T0+T1+T2 percentage across the corpus per client. **Target > 95% for native clients.** Published per release. | Tracked |
