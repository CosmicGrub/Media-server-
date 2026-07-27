# Working on this repository

## Layout

```
docs/          Architecture, research, and the compatibility specification (00-13 + ADRs)
crates/        The shared Rust core — ADR-0004
conformance/   The test corpus that proves docs/11-13
native/        LGPL-only build recipes for FFmpeg / mpv / libplacebo — ADR-0002
ci/            Blocking gates
```

## Quick start

```bash
cargo test --workspace          # 78 tests incl. 8 property tests over the playback ladder
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
ci/license-gate.sh              # ADR-0002 — must pass before any native build merges
python3 conformance/runner/coverage.py
```

## The four rules

**1. The playback ladder has exactly one implementation.**
`crates/lumen-playback` compiles into every client and the server. If a client needs different
behaviour, that difference belongs in its `ClientCapabilities`, never in a second copy of the
decision logic. Divergence here reaches users as unexplained transcoding — the loudest complaint
about every product we are competing with.

**2. Never `--enable-gpl`.**
One GPL flag makes the whole combined work GPL and permanently forecloses App Store distribution.
`ci/license-gate.sh` blocks it. If you believe you need a GPL component, read
[ADR-0002](docs/adr/0002-lgpl-only-build.md) §1.3 first — there is almost certainly a permissive
substitute, and for encoders there definitely is.

**3. Degradation is never silent.**
Any code path that reduces fidelity must emit a `RejectReason`. `explain()` is a product surface
with the same status as a UI string: it names the actual device, format, and numbers. The property
test `degradation_is_never_silent` enforces this and will fail your PR if you add a path that
skips it.

**4. A conformance vector never regresses.**
Corpus vectors assert `achieved <= expected` tier, so improvements pass and regressions fail. A
platform limitation is expressed as a documented `overrides:` entry matching the published capability
matrix ([`docs/04`](docs/04-platform-strategy.md) §7) — never as a skipped vector.

## Adding a capability limit

Real hardware limits (macOS cannot bitstream TrueHD; browsers have no lossless audio) are modelled,
not hidden:

1. Express it in `ClientCapabilities` — usually `AudioSinkCaps.passthrough_encodings` or
   `VideoDecodeCaps`.
2. Add an `overrides:` block on the affected conformance vectors with the honest expected tier.
3. Make sure it matches the capability matrix in `docs/04` §7, which ships **in the app**.

## Adding a `RejectReason`

1. Add the variant with the data a user needs to act on — device names, formats, actual numbers.
2. Add the `explain()` arm. Write the sentence you would want to read on your own TV.
3. Add the `key()` arm. Keys are the contract with `conformance/corpus.yaml`; they are stable.
4. `every_reason_produces_a_non_empty_explanation_and_a_stable_key` will check the shape.

## Property tests

`crates/lumen-playback/tests/ladder_props.rs` holds the invariants. `check_playable` is a referee
written independently of the ladder, so it cannot inherit the ladder's bugs — **when a property
fails, suspect the ladder first, and only weaken a property when you can state precisely why the
stronger claim was never true.** Every weakening so far is documented inline with its reason.

These properties found six real bugs on their first run, including a plan that emitted a container
the client could not open. That is what they are for.

## What is deliberately not here

- No DRM, no Widevine/FairPlay/PlayReady ([`docs/02`](docs/02-architecture.md) §8).
- No AACS/BD+/CSS keys anywhere in the tree or build ([`docs/08`](docs/08-legal-licensing.md) §4).
- No bundled API keys for metadata providers — users supply their own
  ([`docs/08`](docs/08-legal-licensing.md) §5).
- No torrent/usenet client. Integrate with the *arr stack via plugins; do not become one.
