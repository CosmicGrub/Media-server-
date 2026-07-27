# Spike S1 — compositing a WebView over hardware video

**Question:** can a WebView UI composite over hardware-decoded video without tearing, stutter, or
input lag?

**Why it matters:** the answer decides the desktop shell. A pass keeps Tauri v2 (`docs/09-roadmap.md`
§2); a fail triggers the documented fallback to Qt 6 (ADR-0001). Getting this wrong is expensive in
one direction only — building the whole desktop client on Tauri and discovering at beta that 4K HDR
stutters behind the UI means rewriting the shell.

## The design decision that matters

**The measurement is a paired comparison, not an absolute.**

Running only the composited stage conflates two findings that look identical in the numbers:

| what you see | what it means | consequence |
| --- | --- | --- |
| composited stage drops frames | compositing costs frames | **invalidates Tauri** |
| composited stage drops frames | this machine cannot decode the clip at all | **invalidates nothing** |

So the harness runs mpv bare first, with byte-identical options, and reports the *delta*. If the
baseline itself cannot sustain playback, the verdict is `INCONCLUSIVE` — never `FAIL`. Reporting an
unusable baseline as a failure would wrongly indict the architecture, and that error is not
self-correcting: nobody re-runs a spike that already gave an answer.

## Two profiles, because a desktop and a laptop are different questions

`profiles/desktop.toml` and `profiles/laptop.toml`. The laptop profile is not a looser desktop
profile — it adds three checks a desktop run **can never surface**, and all three have bitten real
players:

1. **Hybrid graphics park the discrete GPU on battery.** The frame rate halves without a single frame
   being reported "late", so a delayed-frame count alone shows nothing. `test_on_battery = true`.
2. **Thermal throttling arrives minutes in.** Ninety seconds of clean playback is not a pass.
   `thermal_soak_minutes = 10`.
3. **Compositing CPU cost is battery life and fan noise**, not just a number — so the laptop's CPU
   budget is *tighter* (0.60 cores) than the desktop's (1.00) even though its frame budget is looser
   (5.0 vs 2.0 added late presents/min).

The desktop profile is stricter where a desktop has no excuse: `min_fps_ratio = 0.99` against the
laptop's 0.95.

## Running it

```bash
# 1. Install what's needed and verify the mpv build can measure the right pipeline.
./bootstrap/linux.sh          # or macos.sh, or  powershell -File bootstrap\windows.ps1

# 2. Look at what the machine is before measuring it.
cargo run -p s1-compositing -- probe

# 3. Baseline only — useful on its own, and it needs no shell.
cargo run -p s1-compositing -- run \
  --profile spikes/s1-compositing/profiles/desktop.toml \
  --clip /path/to/clip.mkv

# 4. The real thing, once the shell in ui/ builds.
cargo run -p s1-compositing -- run \
  --profile spikes/s1-compositing/profiles/desktop.toml \
  --clip /path/to/clip.mkv \
  --shell spikes/s1-compositing/ui/src-tauri/target/release/lumen-s1-shell \
  --osd-latency 34
```

Exit codes: `0` pass (or pass with notes), `1` fail, `2` bad usage, `3` inconclusive or no composited
stage. Inconclusive is deliberately *not* zero — an unanswered question must not read as a pass in
CI or in a script.

## Choosing a clip

The clip should be the hardest thing the product claims to play, because the spike is about headroom:

- **4K HDR10 remux, high bitrate** — 60–100 Mbit/s HEVC Main10. This is the case that matters.
- 23.976 fps if possible, since film cadence is where pacing errors are visible.
- At least 3 minutes, so the run is steady state rather than warmup. The harness loops a shorter clip
  (`--loop-file=inf`), but a loop point is a seek, and seeks reset mpv's counters — the harness
  handles that (`monotonic_delta`), at the cost of losing the frames around the seam.

Do not use a synthetic test pattern. Flat gradients and static frames compress to almost nothing and
give the decoder no work, which is the opposite of the case under test.

## Before believing any result

The harness prints environment warnings for these; do not skip them.

- **Refresh rate.** 23.976 fps on a 60 Hz panel judders with no compositor involved. That judder
  looks exactly like a compositing failure. Set the panel to a multiple of the clip's rate.
- **Which GPU rendered.** On a hybrid machine, a render that landed on the iGPU is measuring a
  different question. The harness can see that two adapters exist; it cannot see which one was used.
- **`gpu-next`.** The product ships the libplacebo renderer. A result from the older `gpu` output
  measures a different pipeline. Some distribution mpv packages are built without it.
- **X11 unredirect.** If the compositor unredirects fullscreen windows, the *baseline* bypasses
  compositing entirely while the shell stage does not, inflating the measured cost.
- **macOS ProMotion and Low Power Mode.** Both change the GPU's behaviour mid-run without appearing
  in a frame counter.

## Layout

```
src/pacing.rs     the verdict — pure logic, fully tested, no hardware needed
src/profile.rs    zero-dependency `key = value` parser for the profiles
src/probe.rs      what this machine is, per-OS, degrading to None rather than failing
src/mpv_ipc.rs    JSON-IPC client and the shared mpv arguments
src/report.rs     console and JSON rendering
src/main.rs       CLI and stage orchestration
profiles/         desktop.toml, laptop.toml
bootstrap/        per-platform install and verification
ui/               the composited stage — UNVERIFIED TEMPLATE, see ui/README.md
```

The crate has **zero dependencies**, so it builds on a fresh machine with nothing but a Rust
toolchain. Everything that can be tested without a GPU is tested: 54 tests covering the verdict
logic, the profile parser, the IPC reply parser, the environment warnings, and the report renderers.
What cannot be tested here is the part that needs a display, which is exactly why this is a spike to
be run on your own hardware rather than a CI job.

## Recording the result

`--out` writes a JSON report (default `s1-report.json`) carrying the environment, the profile, both
stages, the warnings, and the verdict with its findings. Attach it to the S1 entry in
`docs/09-roadmap.md`. A verdict with no record of which GPU drove it, what the display was doing, or
whether the machine was on battery is not a result — it is an anecdote.
