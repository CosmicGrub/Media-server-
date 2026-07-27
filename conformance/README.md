# Conformance Corpus

The machine-readable proof of the compatibility guarantee in
[`../docs/11-compatibility-charter.md`](../docs/11-compatibility-charter.md).

Every requirement in [`11`](../docs/11-compatibility-charter.md),
[`12`](../docs/12-container-conformance.md), and [`13`](../docs/13-remux-transcode-matrix.md) maps to one or more
vectors here. The runner executes every vector on every shipped platform in CI.

**Release gate:** 100% of vectors achieve their `expect.tier` or better, on every platform. No waivers.

---

## 1. Layout

```
conformance/
├─ README.md          ← this file
├─ corpus.yaml        ← the vector manifest (source of truth)
├─ schema.json        ← JSON Schema for corpus.yaml
├─ media/             ← clipped samples (git-lfs), 3–10 s each, ≤ 20 MB
│  ├─ mkv-core/
│  ├─ mp4-core/
│  └─ …
├─ golden/            ← reference frame hashes + golden SDR/HDR renders
└─ runner/            ← the harness (see §4)
```

**Media files are not committed to git directly.** They live in git-lfs (or an object store referenced by
`sha256`). Each vector records the checksum so the runner can verify it fetched the right bytes.

**Clip, don't ship features.** Vectors are 3–10 second excerpts. A 90 Mbps UHD remux vector is a 10-second clip
(~110 MB) — the whole corpus stays under ~4 GB. Where a property only manifests at length (index handling in a
500 GB file, 33-bit PTS wraparound), use a **synthetic generator** declared via `generate:` instead of a stored file.

---

## 2. Vector schema

```yaml
- id: mkv-core-lacing-xiph          # stable, unique, kebab-case
  group: mkv-core                   # see §3
  title: "Vorbis audio with Xiph lacing"
  spec: ["12#2.2"]                  # doc anchors this vector proves
  severity: blocking                # blocking | high | medium
  source:
    file: media/mkv-core/lacing-xiph.mkv
    sha256: "…"
    # OR:
    # generate: "ffmpeg -f lavfi -i testsrc2=… -c:a libvorbis …"
    # OR:
    # origin: "matroska.org test suite, test5.mkv"   # provenance for third-party vectors
  properties:                       # what makes this vector interesting
    container: matroska
    lacing: xiph
  expect:
    plays: true
    tier: T1                        # highest tier that must be achievable
    recovery_rung: 0                # max acceptable rung from 12#5
    streams:                        # streams that must be detected and decodable
      - { type: video, codec: h264, width: 1920, height: 1080 }
      - { type: audio, codec: vorbis, channels: 2, sample_rate: 48000 }
    duration_s: { value: 10.0, tolerance: 0.05 }
    frame_hashes: golden/mkv-core-lacing-xiph.txt   # optional, for decode correctness
    av_sync_ms: { max_drift: 20 }
    reasons_absent: [VideoCodecUnsupported]         # reasons that must NOT appear
  platforms: [windows, macos, linux, android, ios, tvos, web]
  # platform-specific expectation overrides:
  overrides:
    web: { tier: T3, reasons_present: [ContainerUnsupported] }
```

Field notes:
- **`tier`** is the *best* tier that must be reachable on that platform with a fully capable sink. The runner asserts
  `achieved <= expected` (lower number = better), so an improvement never fails the build.
- **`recovery_rung`** asserts the file opened without needing more recovery than expected — this catches silent
  regressions where a file still plays but only via a slow fallback path.
- **`reasons_absent` / `reasons_present`** assert against the structured `RejectReason` values from
  [`03#6`](../docs/03-playback-engine.md) — this is how "no needless transcoding" is enforced mechanically.
- **`overrides`** encode honest platform limits (macOS cannot bitstream TrueHD; the web tier has no MKV Direct
  Play). An override is a documented limitation, not a waiver — it must match the published capability matrix in
  [`04#7`](../docs/04-platform-strategy.md).

Validate with `schema.json` in CI before the runner starts, so a malformed manifest fails fast.

---

## 3. Groups and target coverage

| Group | Defined | Listed | Target | Covers |
|---|---:|---:|---:|---|
| `mkv-core` | 19 | 24 | 24 | EBML, lacing (all 3), unknown-size, TimestampScale, BlockAdditions, CodecDelay/SeekPreRoll, header stripping, attachments/fonts, colour elements, crop/display size |
| `mkv-chapters` | 6 | 12 | 12 | Ordered chapters, hard/soft segment linking, edition selection, nested chapters, **loop detection** |
| `mkv-damage` | 5 | 10 | 10 | Truncation, missing/broken Cues, corrupt SeekHead, inter-cluster garbage, concatenation |
| `mp4-core` | 17 | 23 | 23 | Edit lists (all forms), ctts v0/v1, multiple stsd, avc3/hev1, QuickTime PCM, chapters ×3, mdat-before-moov, clap/pasp/colr |
| `mp4-fragmented` | 4 | 9 | 9 | moof/tfdt/sidx, init switching, live, orphan segments |
| `mp4-damage` | 4 | 8 | 8 | moov reconstruction, bad stco, truncation, concatenation |
| `codec-video` | 9 | 17 | 38 | The §3 matrix of [`11`](../docs/11-compatibility-charter.md) |
| `codec-audio` | 9 | 15 | 26 | Lossless family, passthrough, DSD, 22.2, implicit SBR, ambisonics |
| `subtitles` | 5 | 14 | 14 | ASS+fonts, PGS, CEA-608/708, forced logic, legacy encodings |
| `spectrum` | 9 | 18 | 18 | Resolution extremes, VFR, telecine, anamorphic, 3D, 360, HDR variants |
| **Total** | **87** | **150** | **182** | |

- **Defined** — full vectors with `source` and `expect`. These are the Phase 1 release gate.
- **Listed** — defined plus `status: planned` stubs, which make coverage gaps visible rather than implicit.
- **Target** — the coverage the corpus must reach before the compatibility claim in
  [`11`](../docs/11-compatibility-charter.md) is fully proven.

`codec-video` and `codec-audio` carry the largest gaps because they enumerate the full codec matrix; the structural
container groups (which is where "every MP4 and MKV plays" actually lives) are nearly complete. Run
`runner/coverage.py` to regenerate this table.

---

## 4. Runner contract

```
runner/run --platform <p> [--group <g>] [--vector <id>] [--update-golden]
```

Per vector, on the target platform:

1. **Fetch & verify** the media by `sha256` (or run `generate:`).
2. **Open** through `lumen-core`'s real playback path — not a bespoke test harness. The runner must exercise the
   same code the product ships.
3. **Assert open-time expectations**: streams detected, codecs/geometry/channel counts, duration, recovery rung.
4. **Build the plan** against a synthetic client capability set (fully capable, plus the platform's real caps) and
   assert `tier`, `reasons_absent`, `reasons_present`.
5. **Play to completion** with the renderer attached, capturing: decoded frame hashes (where `frame_hashes` is set),
   audio bitstream hash for T0/T1 audio paths, dropped-frame count, measured A/V drift.
6. **Seek suite**: seek to 0%, 33%, 66%, 99%, then backwards; assert each lands within tolerance and decodes a clean
   frame. Seeking is where index handling regressions surface.
7. **Emit** a JSON result row and, on failure, a diagnostic bundle (probe output, plan, `PlaybackReport`, logs, the
   first divergent frame as a PNG).

Runs headless on Windows/macOS/Linux; on a physical device rack for Android/iOS/tvOS
([`04#8`](../docs/04-platform-strategy.md)); in a browser matrix (Chrome/Firefox/Safari) for web.

---

## 5. Sourcing the media

Prefer freely-redistributable and standards-body material; generate what you can; clip the rest.

| Source | Use |
|---|---|
| **[Matroska test suite](https://www.matroska.org/downloads/test_suite.html)** | `mkv-core` baseline — the canonical 8 test files |
| **[hubblec4/Matroska-Playback](https://github.com/hubblec4/Matroska-Playback)** | `mkv-chapters` — ordered chapters, segment linking, and the endless-loop cases |
| **JVET / JCT-VC conformance bitstreams** | HEVC and VVC profile coverage, incl. SCC |
| **AOM AV1 Argon test vectors** | AV1 profile/bit-depth/film-grain coverage |
| **ITU-T H.264 conformance streams** | Hi10P, 4:2:2, 4:4:4, MVC, interlaced |
| **[GPAC test suite](https://github.com/gpac/testsuite)** | ISOBMFF box coverage, fragmented MP4, DASH |
| **[media.xiph.org](https://media.xiph.org/)** | Uncompressed and reference sources for generation |
| **FFmpeg FATE samples** | Legacy codecs, damage classes, obscure containers |
| **Blender open movies** (Big Buck Bunny, Tears of Steel, Sintel, Cosmos Laundromat) | CC-BY; good for remux/HDR/high-bitrate vectors you generate yourself |
| **Generated** (`ffmpeg` + `mkvmerge` + `MP4Box` + `dovi_tool` + `bento4`) | Everything structural: lacing variants, edit lists, damage injection, VFR, telecine, odd timescales |
| **Damage injection** | A script that truncates, corrupts, zeroes, and shuffles bytes at controlled offsets to produce reproducible `*-damage` vectors |

**Licensing:** every vector records `license` and `origin`. Nothing enters the corpus without a redistributable
license or a generation recipe. Do not commit ripped commercial content — generate an equivalent instead.

---

## 6. The wild corpus (the real moat)

Separate from the curated set: an **opt-in, user-contributed** collection of files that once broke the player.

- Every user-reported playback bug adds a permanent vector after triage.
- Contributors submit a **structural sample** by default — the first and last 2 MB plus a header dump, enough to
  reproduce most container bugs without redistributing content. Full clips only with explicit permission and a
  redistributable-license attestation.
- Wild vectors run in a nightly job (not the blocking PR gate) and graduate into the curated corpus when a
  generation recipe or a redistributable equivalent is produced.

This is the asset a competitor cannot copy from a repository, and it compounds with every user.
