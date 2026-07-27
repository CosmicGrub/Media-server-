# S1 shell — the composited stage

> **Status: unverified template.** Every other file in this spike is compiled and tested. This one is
> not. It was written on a machine with no GPU, no display server, and no graphics drivers, so it has
> never been built or run. Treat it as a starting point that encodes the right *design*, not as
> working code — expect to fix build errors and API drift on first contact.
>
> The rest of the harness does not depend on it. `run` without `--shell` measures the baseline stage
> and says so, which is useful on its own: it tells you whether the machine can play the clip at all
> before you spend time on a shell.

## What this has to do

The harness measures a *pair*: mpv alone, then the same clip inside the shell with an HTML overlay on
top. For the comparison to mean anything, this shell must satisfy three constraints:

1. **Identical player configuration.** The mpv options come from `mpv_ipc::common_mpv_args`. Changing
   any of them here — a different `--vo`, a different `--hwdec`, the user's own `mpv.conf` leaking in
   — turns the comparison into a measurement of the configuration difference.
2. **`LUMEN_S1_IPC` must reach mpv's `input-ipc-server`.** That socket is how the harness reads the
   counters. Without it the composited stage cannot be measured at all, and the harness will say so
   rather than guess.
3. **`LUMEN_S1_CLIP` is the file to play.** Both arrive by environment so the shell needs no argument
   parsing of its own.

## The two ways to composite, and which one this tests

**A — overlay window.** mpv gets its own window; the WebView is a second, transparent, always-on-top
window sized to match. Simple, and it works today on all three desktop platforms.

**B — shared surface.** mpv renders through the render API (`mpv/render.h`) into a texture the shell
owns, and the WebView composites over it inside one window. This is what the product wants: one
window, one swapchain, no chance of the two surfaces desynchronising when the window moves.

This scaffold implements **A′** — mpv embedded into the Tauri window via `--wid`, with the WebView
transparent on top. That is the honest middle: one window, so it measures real per-frame compositing
cost rather than two independent windows that happen to overlap, but without the render-API work that
only pays off once the architecture is chosen.

**If A′ passes, B is very likely fine.** A′ composites through the platform compositor on every
frame; B does the composite itself and skips a copy. A failure in A′ is therefore not conclusive
against B — it is the cue to build the render-API version before concluding anything, because
`--wid` embedding is known to be the weaker path on Windows in particular.

## OSD latency

The overlay's responsiveness is half the reason to use a WebView, so the harness treats it as a
pass/fail criterion when a profile sets `max_osd_latency_ms`. `index.html` measures it: on a keypress
it stamps `performance.now()`, toggles the OSD, then waits for the first `requestAnimationFrame`
*after* layout has settled and stamps again. The delta is the number.

The shell prints it to stderr as `LUMEN_S1_OSD_LATENCY_MS=<n>`, which the harness inherits. Pass it
back on the next run:

```
cargo run -p s1-compositing -- run --profile ... --clip ... --shell ... --osd-latency 34
```

A run with no OSD measurement is a `PASS (with notes)`, never a clean pass — an incomplete result
should not be able to close the spike.

## Building

```
cd spikes/s1-compositing/ui/src-tauri
cargo build --release
```

This directory is `exclude`d from the workspace on purpose: pulling Tauri's dependency tree into
`cargo test` at the repository root would make the tested crates unbuildable on a machine without
WebKitGTK. Prerequisites are Tauri v2's own — see https://v2.tauri.app/start/prerequisites/ — plus
`libmpv` development headers if you later move to approach B.

Then point the harness at the binary:

```
cargo run -p s1-compositing -- run \
  --profile spikes/s1-compositing/profiles/desktop.toml \
  --clip /path/to/clip.mkv \
  --shell spikes/s1-compositing/ui/src-tauri/target/release/lumen-s1-shell
```
