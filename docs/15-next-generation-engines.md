# 15 — Four Engines That Close Real Gaps in the Shipped Code

This is not a restatement of [`09-roadmap.md`](09-roadmap.md)'s P2–P6. Those phases describe the
original Tauri/axum/Wasm-plugin vision. What actually shipped, across the three device forks
(`device/windows-pc`, `device/galaxy-z-fold-5`, `device/galaxy-tab-s9-fe`), is leaner: a plain Rust
CLI (`lumen`), a persistent `lumen serve` that a paired phone controls over a hand-rolled
newline-JSON-over-TLS protocol, and a Compose Android client. That codebase already has seven working
engines (detection & naming, the fidelity ladder, identity/subs/model, local playback, remote
control, Android playback, Android fold/posture) — see the Engine Ledger audit for the full accounting.

The four engines below are scoped against *that* real codebase — real crates, real structs, real
gaps found by reading the actual source, not aspirational architecture. Each one is buildable without
inventing new infrastructure: no new services, no accounts, no cloud dependency, nothing that
contradicts "this should function offline primarily."

**Engine A shipped** — [`crates/lumen-index`](../crates/lumen-index) plus `lumen reindex` in
`lumen-play`. One design call changed between this pitch and the real build: it persists to a flat,
hand-rolled, tab-separated file (the same convention `TokenStore` already uses in this workspace,
`persist.rs`'s own doc comment explains why) rather than SQLite via `rusqlite` as first proposed
below — no new dependency, no C toolchain risk added to a project that already cross-compiles Windows
via `mingw`, and a personal media library's file count never approaches the scale where a linear
diff/load actually matters. §A below is left as originally written for the reasoning that still
holds; only the storage engine changed.

---

## A. Library Index Engine

### The gap, precisely

`lumen scan` and `lumen serve` both call the same walker in
[`lumen-play/src/scan.rs`](../crates/lumen-play/src/scan.rs), and it is **stateless**: every
invocation re-walks the tree and re-probes every file from zero. `server.rs` holds the result as
`library: Arc<Mutex<Scan>>` — one snapshot taken at startup, held in memory, never refreshed. The
wire protocol already has a `library_version: u64` field on `PlaybackState`
([`protocol.rs`](../crates/lumen-play/src/remote/protocol.rs)) — its own doc comment says a client
uses it "to cheaply decide whether its cached listing is stale" — but `server.rs` hardcodes it to
`0`. The hook is designed in; nothing drives it.

Separately: `lumen-meta` (provider fragments, artwork selection, field merge with provenance) and
`lumen-subs` (subtitle acquisition ladder, sync correction) are both fully built, independently
tested crates — 38 and 62 tests respectively — that `lumen-play/Cargo.toml` **does not depend on**.
They exist; nothing calls them. A library index is the natural place to wire them in, because
enrichment is exactly the kind of work you do once per file and cache, not per request.

### Design

New crate, `lumen-index`, keeping the same one-crate-per-concern convention as `lumen-probe` /
`lumen-match` / `lumen-identity`. Backing store: SQLite via `rusqlite` (`bundled` feature — public
domain, no LGPL interaction, and already the documented choice for `lumen-server` in
[`02-architecture.md`](02-architecture.md)), one file next to the library root or in
`XDG_CONFIG_HOME` per platform, matching `TokenStore`'s existing config-directory convention.

**Incremental re-scan**, the actual point of "incremental":

1. Walk the tree; for every file, compute a cheap digest — `(path, size, mtime)` — no I/O beyond a
   `stat`.
2. Diff against the persisted index. `(path, size, mtime)` unchanged → skip re-probe entirely. On a
   library that hasn't changed since last run, this turns a full re-probe into a stat-only walk —
   the actual speedup, not just "now it remembers things."
3. Changed or new → run the existing pipeline for real: `lumen-probe` → `lumen-match` →
   `lumen-identity` (`ContentSketch`) → **`lumen-meta` (now actually wired)** → persist.
4. Missing from disk but present in the index → tombstone, never hard-delete. A resume position or
   watch-state keyed by `ContentKey` (the Android side already keys resume by content identity, not
   path — see `ResumeStore`) must survive a file being temporarily unavailable on a flaky SMB mount,
   not silently orphan.
5. `library_version` becomes real: a monotonic counter incremented on any committed index mutation.
   Paired clients' `library_version` check starts meaning what the protocol doc already claims it
   means.

**Failure isolation**, learned from this codebase's own history: the property test
`truncation_at_any_offset_never_panics` in `lumen-probe` found a panic-on-malformed-input bug —
described in the roadmap as "a denial of service on a watched folder, since anyone who can drop a
file into one controls its bytes." A re-index pass inherits that risk at scale: one bad file must
never abort a 20,000-file re-index. Per-file probe/match/enrich failures are recorded as a
`needs_review` status on that entry, not propagated as an error that kills the batch.

**CLI**: `lumen serve <path> --index <db-path>` — opt-in, so the existing stateless mode keeps
working unchanged for anyone who doesn't want a database file sitting in their library.
`lumen reindex <path>` for a manual trigger. A live filesystem watcher (`notify` crate) is real, but
deliberately **out of scope for the first cut** — periodic re-diff on `serve` startup plus the manual
command is a legitimate MVP, and a background watcher is the honest phase-2 item, not something to
claim as done before it exists.

**New wire messages**, once search is cheap because the index is real:
`ClientMessage::Search { id, query }` → `ReplyBody::SearchResults(Vec<LibraryEntry>)`, matched
against `lumen-match`'s already-parsed titles — no new parsing logic, just a query over what's
already extracted.

### Test strategy

- Property: re-indexing twice with no filesystem change produces zero probe/match/enrich calls
  (mock the pipeline, assert call count).
- Property: a file renamed with byte-identical content is recognized via `ContentSketch`, never
  treated as delete+create — this is the entire reason `lumen-identity`'s move-survival design
  exists, and right now nothing in the shipped code exercises that design end to end.
- A forced probe failure on one file in a batch of many never aborts the remaining files.

---

## B. Library Integrity & Self-Healing Engine

**Shipped** — [`Index::verify`](../crates/lumen-index/src/store.rs) plus `lumen verify` in
`lumen-play`. Three design calls changed between this pitch and the real build, all recorded honestly
below rather than left for the code to contradict silently:

- **Digest algorithm: `lumen_identity::FileDigest`/`digest_reader` (xxh3-128), not BLAKE3.** Same
  reasoning as Engine A's SQLite→flat-file swap — `xxhash-rust` is already in the dependency graph
  via `lumen-identity`, reusing it adds nothing new to the license gate or the build matrix. It isn't
  cryptographic, same as `ContentSketch` isn't — this defends against bit rot, not an adversary.
- **Rate limiting: a per-invocation byte budget, not a live playback signal.** `lumen serve` has no
  "something is currently playing" state a background pass could consult yet — that integration does
  not exist, so tying a limiter to it would have been claiming a thing that isn't real. `lumen verify`
  runs as its own standalone, budget-bounded invocation instead (`--budget`, default 8 GiB), meant to
  be scheduled the same way `Install-LumenServeTask.ps1` already keeps `serve` alive. Backing off
  specifically because a movie is playing is the honest phase-2 gap this leaves open, same shape as
  Engine A's missing live filesystem watcher.
- **Selection is tier-and-risk-prioritised, not flat oldest-first**, per explicit direction mid-build:
  an unresolved mismatch is always reselected first (regardless of the reverify interval, until a
  later pass confirms it or `reindex` sees the file legitimately change); then anything never
  verified at all; then anything `reindex` itself already flagged via `needs_review`; then everything
  else due, oldest-confirmed-first — and within that last tier, a larger file's effective interval is
  shortened (halved per size-doubling above 4 GiB, capped at a quarter) since it carries more bit-rot
  exposure for the same elapsed time. `mismatch_pending` is a new persisted field specifically to
  make "unresolved" survive across process restarts, not just within one run.

### The gap, precisely

This session already shipped `verify_duplicate_group` in
[`lumen-play/src/scan.rs`](../crates/lumen-play/src/scan.rs) — a chunked byte-for-byte comparison
that closes the gap between "these files' content sketches match" (implausible-but-not-impossible to
collide) and "these files are actually identical." It runs once, on demand, inside `lumen scan
--identify`, printing `(unconfirmed -- ...)` when two files only agree on the sketch. That is a
sound primitive with no scheduler around it: it verifies *agreement between files*, never *drift in
a single file over time* — bit rot, a botched network copy, a failing drive — which is the failure
mode that actually matters for a media library meant to sit on a disk for years (the stated target
per [`01-competitive-analysis.md`](01-competitive-analysis.md): people with libraries Plex/Jellyfin
already serve).

### Design

Depends on Engine A for persistence — verification needs somewhere to store "last confirmed good"
per file. For each index entry, store a full-file digest (BLAKE3, computed once, not the sampled
3-region sketch — this pass exists specifically to catch what the sketch is honest about not
catching) alongside the timestamp it was taken.

A background pass, run at `serve` idle time or via `lumen verify <path>`:

1. Select entries not verified in the last N days (configurable; default long — this is a background
   job, not a scan).
2. Re-read the file in 256 KiB chunks (reusing the exact `same_bytes` chunking pattern already
   written for `verify_duplicate_group`), recompute the digest, compare to the stored one.
3. Match → bump `last_verified`. Mismatch → **never silently re-index as if nothing happened.**
   Record a `RejectReason`-shaped diagnostic in the same voice as `lumen-playback`'s existing
   taxonomy: "this file's bytes have changed since it was verified on `<date>`; if this wasn't an
   intentional re-encode, the underlying media may be failing" — a Rule 3 (`CONTRIBUTING.md` — no
   silent degradation) violation is exactly what a naive "just re-hash and move on" implementation
   would commit, and the whole point of this engine is to be the thing that doesn't.
4. **Idle-scheduled, rate-limited I/O.** A byte-level pass across a multi-terabyte library competing
   with active playback for the same spinning disk is a real, concrete failure mode — this must never
   cause buffering during a movie someone is actually watching. Ties a token-bucket limiter to the
   same `Arc<Mutex<...>>` playback-state the server already holds, so "something is currently
   playing" is a real backpressure signal, not a guess.

### Test strategy

Direct analog to the four tests already written for `verify_duplicate_group` this session: a file
whose bytes change between two verify passes is always flagged, never silently accepted as
re-verified; a read failure is reported, never guessed at; verifying an empty or single-entry set is
trivially true; idle-scheduling never starves indefinitely if playback runs continuously (a fairness
property, not just a functional one).

---

## C. Fidelity Telemetry & Calibration Engine

**Shipped** — [`calibration.rs`](../crates/lumen-play/src/calibration.rs) in `lumen-play`, wired into
both `run()` (records after every `play`/`test` session) and `doctor()` (prints the running summary).
Two design calls changed between this pitch and the real build:

- **Scope narrowed to hardware video decode; audio passthrough left out entirely, not stubbed.**
  Investigating the IPC path this pitch assumed found that `session.rs` never passes `--audio-spdif`
  to mpv in the first place — mpv decodes every bitstream audio format (TrueHD, DTS-HD MA) to PCM
  regardless of what the AVR could do, so comparing `audio-out-params` against a passthrough
  prediction would make every single file "miss" for a reason that has nothing to do with the
  fidelity model's honesty. That would be evidence about a missing feature, not about the model —
  worth building once passthrough is actually requested, not before. The hardware-decode half ships
  because it checks something the codebase already does for real: `session.rs` already queries
  `hwdec-current` for every session, `fidelity::assess` already predicts a decode path for the same
  file on the same struct — the gap really was just the missing comparison, exactly as pitched.
- **Storage is hand-rolled JSON Lines via this crate's existing `json` module, not a new
  serialization dependency.** Same reasoning as Engine A and B's dependency-avoidance calls — `lumen`
  already parses and emits this shape of JSON for `--json` reports, so a fourth crate for one small
  append-only log would be pure overhead.

A real end-to-end run against actual mpv (a genuine H.264 file, headless, no hardware decoder
available) caught a real bug before this shipped: the first draft compared mpv's human-readable
`video-codec` property (`"H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10"`) against the codec-name table
`fidelity::assess` itself matches against short FFmpeg names (`"h264"`) — silently never matching
anything, exactly the kind of drift the module's own doc comment warns against. Fixed to read the
selected video track's short codec name from `r.tracks` instead, the same field
`fidelity::assess`'s own `mpv_selection` already reads — confirmed against that same real mpv run
afterward, which correctly flagged the (real, expected) software-decode miss.

### The gap, precisely

The fidelity module's own PR description says it plainly: fidelity tiers are **"modeled, not
measured"** — real demux data in, capability profiles applied, a projected tier out. The Engine
Ledger audit flagged the two genuinely large, hardware-dependent verification efforts (bitstream
Atmos/DTS:X + Dolby Vision detection; live 4-platform hardware-decode probing) as future work,
correctly, because they need real device labs. But there is a much smaller piece sitting right next
to it that needs no lab at all: **mpv already knows the truth about every real playback session, and
nothing asks it.**

`lumen-play`'s `ipc` module already speaks mpv's JSON IPC for local playback. mpv exposes
`hwdec-current` (did it actually hardware-decode, or silently fall back to software?),
`frame-drop-count`, and `audio-params`/`audio-out-params` (did passthrough actually happen, or did
mpv quietly downmix TrueHD to stereo PCM?) as ordinary queryable properties over that same
connection.

### Design

After any `lumen play`- or `serve`-driven playback session ends, query those three properties from
mpv over the already-open IPC connection — no new dependency, no new protocol, the exact same
mechanism `remote_serve.rs`'s existing integration test already drives against a real mpv process.
Compare the observed values against what the fidelity ladder predicted for that
file × client-capability combination going in. A mismatch — predicted hardware decode, mpv reports
software; predicted TrueHD passthrough, mpv reports a downmixed PCM format — is recorded as a
`CalibrationMiss`.

Storage is a strictly local, append-only log —
`~/.config/lumen/calibration.jsonl` on the same convention `TokenStore` already uses — **never
transmitted anywhere.** This is a calibration log a user can `cat`, not telemetry in the
surveillance sense; the distinction matters enough to state explicitly given this project's own
"should function offline primarily" stance, and it should say so in its own doc comment, not just in
this pitch.

This turns S5 from the roadmap ("audio passthrough... not started, needs a physical AVR") from a
blocked spike into something every real playback session on every real user's real hardware
contributes evidence toward, incrementally, with zero lab requirement. `lumen doctor` gains a new
section: predicted-vs-observed fidelity over the last N sessions — the capability model stops being
purely theoretical the moment this ships.

### Test strategy

Unit tests against a scripted fake-mpv IPC transcript (same fixture shape `remote_serve.rs` already
uses for its real-mpv assertions, just replayed instead of live) covering: a correct prediction
records nothing, a hardware-decode miss is flagged, a passthrough miss is flagged, and a session that
never reports a completion event does not silently mark the prediction as confirmed by omission.

---

## D. Paired-Server Health & Diagnostics Engine

### The gap, precisely

`DEVICE.md` documents `lumen serve` running as a Windows Scheduled Task specifically so it can run
headless, with no window open, restarting itself if it exits. That is the deployment model. But the
only thing a paired phone can currently ask the server is what it's playing — the wire protocol has
no health surface at all. If the scheduled task silently stops, if the disk holding the library fills
up, or — the freshly-relevant one — if the self-signed pinned TLS certificate this session just
shipped is approaching expiry, **the first sign is the phone's connection just failing**, with no
prior warning and no way to check without walking over to the PC.

### Design

Extend the existing `id`-echoed request/reply pattern with one new pair:
`ClientMessage::Health { id }` → `ReplyBody::Health(HealthReport)`, where `HealthReport` carries:

- mpv IPC round-trip time (a slow or hung response is a directly actionable signal — "the player is
  wedged" is different from "nothing is playing")
- TLS certificate expiry, read from the same cert this session's pinning work already generates —
  the one concrete follow-on gap that work left open: pinning has no rotation story yet, and a
  client finding out via a hard connection failure is a worse experience than a warning with days of
  notice
- library index freshness — last successful `reindex` timestamp, once Engine A exists
- free disk space on the library volume
- active paired-client count

### Why this is the right shape, not a bigger one

This is deliberately *not* a general telemetry/metrics system. It answers exactly the questions the
existing deployment model (headless, no console, phone-first control) makes otherwise unanswerable
from the phone, using data the server already has in memory or one syscall away — no new
subsystem, no persistence of its own beyond what Engines A–C already store.

### Test strategy

Extend `remote_serve.rs`'s existing real-mpv integration test with a `Health` round-trip assertion.
Android side reuses the established `RemoteClient`/`RemoteProtocol` pattern already proven by every
other message type in `RemoteScreen.kt` — a small "Server" card, not a new screen.

---

## Why these four and not others

Each engine above extends code that already exists and is already tested, rather than opening a new
front. None require an account, a cloud service, or new infrastructure — consistent with the
project's stated LAN-first, offline-primary posture. Each closes a gap that was found by reading the
actual source, not by extrapolating from the aspirational roadmap: a hardcoded `library_version: 0`,
two fully-built crates nothing depends on, a fidelity model with no feedback loop, and a headless
server with no way to ask it how it's doing. That is the bar for "engineering-grade" this document
tries to hold itself to — a precise, named gap in real code, a design that reuses this codebase's own
established patterns, and an explicit test strategy, for every item.
