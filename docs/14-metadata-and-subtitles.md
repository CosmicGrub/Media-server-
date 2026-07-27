# 14 — Metadata Providers, Artwork & Subtitle Acquisition

How the parser's output becomes real artwork, real descriptions, and — the hard part — accurate English
subtitles for a foreign-language film that has none.

Builds on [`05-server-library.md`](05-server-library.md) §4.4–§6 and
[`06-plugin-system.md`](06-plugin-system.md). Implemented in
[`../crates/lumen-meta`](../crates/lumen-meta) and [`../crates/lumen-subs`](../crates/lumen-subs).

---

## 1. Provider compatibility: one interface, many scrapers

[`lumen-match`](../crates/lumen-match) answers *which title is this*. Providers answer *what does it
look like and what is it about*. The boundary between them is a single interface every scraper
implements, whether it ships first-party or as a Wasm plugin.

```
ParsedName ──► match ──► ExternalId ──► ┌─ TMDB ────────┐
                                        ├─ TheTVDB ─────┤
                                        ├─ AniList ─────┤──► [MetadataFragment] ──► merge ──► Bundle
                                        ├─ MusicBrainz ─┤        (per-field
                                        ├─ Fanart.tv ───┤         provenance)
                                        └─ NFO sidecar ─┘
```

**Every provider is a plugin** ([ADR-0003](adr/0003-plugin-runtime.md)) — a licensing requirement as
much as an architectural one, since TMDB and TheTVDB both gate commercial use behind revenue
thresholds ([`08`](08-legal-licensing.md) §5). Shipping no bundled API keys and letting users supply
their own is what keeps the default install out of the commercial tier.

### 1.1 Fragments, not answers

A provider returns a **fragment**: the fields it happens to know, each tagged with where it came from.
Nothing merges inside a provider, so no provider can overwrite another's work, and the merge policy
lives in one auditable place.

### 1.2 Merge precedence

Highest wins, and a locked field wins over everything:

| Rank | Source | Rationale |
|---:|---|---|
| 0 | **Field lock** | A user who edited a title said so deliberately. Kodi and Jellyfin both learned this the hard way. |
| 1 | **Local NFO sidecar** | The filesystem is the source of truth ([`02`](02-architecture.md) §1.5). Portable, git-able, survives a reinstall. |
| 2 | **Ranked providers**, per field group | User-configurable: "titles from TMDB, ratings from Trakt, artwork from Fanart.tv". |
| 3 | **Derived** | Computed from the file: runtime from the probe, colour from the stream. |

Every merged field records its provenance, so the UI can show *why* a description says what it does,
and re-running a scrape is a diff rather than a guess.

### 1.3 Language fallback for descriptions

A provider asked for `pt-BR` may return nothing. The chain is **exact tag → base language → user's
fallback list → the work's original language → any**. Returning an empty overview when a Portuguese
one exists is a failure; silently returning Japanese to a user who reads neither is worse.

## 2. Artwork

Types: poster, backdrop, logo, clearart, banner, thumb, disc, season poster, episode still, actor
headshot, album cover, artist background.

Selection is not "highest rated wins":

| Artwork kind | Language preference | Why |
|---|---|---|
| **Poster, logo, clearart** | User's language, then original, then textless | Posters carry the title; a Japanese poster for an English-speaking user is worse than a lower-rated English one. |
| **Backdrop, fanart** | **Textless first**, always | A backdrop sits behind UI text. Baked-in titles collide with it. Providers tag these `language: null`, and that null is a *feature*. |
| **Episode still, headshot** | Irrelevant | No text. |

Then rank by: resolution ≥ a per-kind minimum, aspect ratio within tolerance of the kind's ideal,
provider rating weighted by vote count (a 10/10 from one voter is noise), and finally provider order.

Storage is content-addressed and deduplicated ([`05`](05-server-library.md) §6) — a 50k library shares
a great many actor headshots.

## 3. Subtitles: the acquisition ladder

The request is always the same shape: *give me subtitles I can read for this file*. The ladder spends
the least effort that satisfies it, and **every rung is labelled** so a user always knows whether they
are reading a human translation or a machine one.

| # | Rung | Cost | Fidelity |
|---|---|---|---|
| 0 | **Embedded track** in the container | free | human, authored |
| 1 | **Sidecar file** next to the media (`Movie.en.srt`, `Subs/`, `.en.forced.srt`) | free | human, authored |
| 2 | **Provider search**, exact language tag | network | human, authored |
| 3 | **Provider search**, dialect fallback (`pt-BR` → `pt`) | network | human, authored |
| 4 | **Extract in-video captions** (CEA-608/708) — often the *only* captions on a broadcast recording | cheap CPU | human, authored |
| 5 | **OCR a bitmap track** (PGS/VobSub → text) | GPU-minutes | human wording, OCR errors |
| 6 | **Translate an existing subtitle** in another language | seconds | machine translation of human text |
| 7 | **Transcribe the audio, then translate** (ASR → MT) | GPU-minutes | machine end to end |
| — | **Nothing available** | — | say so plainly |

### 3.1 Rung 6 before rung 7, always

This is the important ordering decision. If a film has Spanish subtitles but no English ones,
**translating the Spanish text is materially better than transcribing the audio**, because it removes
an entire error stage:

- Rung 6: `human transcription → machine translation` — one lossy step.
- Rung 7: `machine transcription → machine translation` — two lossy steps, and the second compounds the
  first. Whisper large-v3 sits around 2.7% WER on clean benchmarks but **8–12% in real-world
  conditions**, and film audio — music beds, overlapping dialogue, accents, whispering — is firmly
  real-world. A 10% word error rate feeding a translator produces confidently wrong sentences.

Rung 6 also inherits **human timing**, which is worth as much as the wording. Rung 7 has to derive
timing from scratch.

### 3.2 Rung 7 done properly

When ASR is genuinely the only option:

| Concern | Approach |
|---|---|
| **Model** | Whisper `large-v3` for quality, `turbo`/`distil` for speed (~6× faster, ~1% WER penalty). No v4 exists as of mid-2026; large-v3 remains the production-safe checkpoint. |
| **Timing** | Whisper's native segment timestamps are too coarse for subtitles. Use **forced phoneme alignment** (the WhisperX approach: faster-whisper backend + wav2vec2 alignment) for word-level timing. |
| **Speaker turns** | Diarization (pyannote) to split overlapping dialogue into separate cues, and optionally to prefix speaker labels for SDH. |
| **Audio prep** | Feed the **centre channel** where a 5.1 track exists — dialogue lives there, and excluding music and effects measurably improves WER. Downmix only as a fallback. |
| **Language detect** | Detect from audio rather than trusting the container's language tag, which is wrong often enough to matter. Cross-check against the tag and flag disagreement. |
| **Translation** | A dedicated MT model (NLLB-200, MADLAD-400, or a cloud API by opt-in) rather than Whisper's own `translate` task, which is weaker than a purpose-built translator. |
| **Segmentation** | Re-segment for *reading*, not for speech: split on clause boundaries within the readability limits in §4, never mid-word, and prefer splits at punctuation. |

### 3.3 Everything machine-generated is labelled, permanently

- Track title carries the origin: `English (machine translated)`, `English (auto-transcribed)`.
- The subtitle file gets a header comment recording model, version, source rung, and timestamp.
- The UI shows a badge on first display of a machine-generated track.
- It is **never** selected as the default when a human-authored track in the same language exists.
- The database stores origin, so a later human subtitle appearing at rung 2 supersedes it
  automatically.

Confidently wrong subtitles are worse than none: a viewer cannot tell a mistranslation from the
script, and will believe it.

## 4. Quality gating — the part that makes "high quality" mean something

A generated subtitle is rejected or flagged, never shipped blind. The thresholds are the published
broadcast standards, not invented ones.

| Metric | Limit | Source |
|---|---|---|
| **Characters per second** | ≤ 17 comfortable, ≤ 20 hard cap (adult); ≤ 15 for children's and accessibility profiles | Netflix caps at 20 (17 for children's); BBC recommends under 15 |
| **Characters per line** | ≤ 42 | Netflix Timed Text Style Guide |
| **Lines per cue** | ≤ 2 | Netflix |
| **CEA-608 closed captions** | ≤ 32 chars/line, ≤ 4 lines | CEA-608 |
| **Minimum cue duration** | ≥ 5/6 s (~833 ms) | Netflix |
| **Maximum cue duration** | ≤ 7 s | Broadcast convention |
| **Gap between cues** | ≥ 2 frames, or merge | Broadcast convention |
| **Overlap** | none permitted | — |

Plus checks that only apply to machine output:

- **ASR confidence floor** per cue; low-confidence cues are marked, and a track whose mean confidence
  is below threshold is offered as a draft rather than applied.
- **Timing sanity** against speech activity: a cue with no detected speech under it is a hallucination,
  which Whisper produces on silence and music. Drop it.
- **Repetition detection** — Whisper's characteristic failure is looping a phrase. A cue repeated more
  than N times consecutively is a decode failure, not dialogue.
- **Length ratio** vs the source when translating: a translation 3× the source length is a runaway.
- **Round-trip spot check** (optional): translate back and compare similarity; a low score flags the
  cue for review.

## 5. Sync verification

An out-of-sync subtitle is experienced as a broken subtitle. Providers routinely return files timed
for a different cut or frame rate.

- **Frame-rate mismatch** (23.976 vs 25) produces linear drift. Detect by comparing first and last cue
  positions against speech activity and correct by rescaling — the ratio is a known small set.
- **Constant offset** from a different intro length: estimate by cross-correlating cue starts against
  a voice-activity track, then apply one shift.
- **Verify before applying**, and record the correction so it can be undone.

## 6. Closed captions vs subtitles

Distinct things, and conflating them is an accessibility failure:

| | Subtitles | Closed captions (SDH) |
|---|---|---|
| Assumes | You can hear | You cannot |
| Contains | Dialogue translation | Dialogue **plus** speaker IDs and non-speech sound (`[door slams]`) |
| Flag | — | `hearing_impaired` / SDH |

For a foreign-language film with no captions at all, generating **both** is possible from the same ASR
pass: diarization supplies speaker labels, and an audio-event classifier supplies non-speech cues. The
SDH variant is a separate track, separately labelled.

## 7. Verification

| Check | Method |
|---|---|
| Merge determinism | Property: same fragments in any order produce the same bundle |
| Locks are absolute | Property: no provider fragment can alter a locked field |
| Artwork selection | Backdrops chosen are textless whenever a textless option exists |
| Language fallback | Table-driven over BCP-47 tags including dialects and `und` |
| Readability | Every threshold in §4 tested at and either side of its boundary |
| Machine-generated labelling | Property: any generated track reports a machine origin and is never a default over a human track |
| ASR quality gates | Fixtures for hallucination-on-silence, repetition loops, and runaway length |
