# `lumen` — playable now

A working player and library test harness. Point it at your collection; it plays everything and
records what happened to each file.

This is the runnable thing, ahead of the GUI. It uses the real crates — `lumen-probe` for
content-based container detection, `lumen-match` for filename parsing — so running it against your
library is also the first real test of that code on data that is not a fixture.

## Install

```bash
# The only prerequisite. Everything plays through mpv.
#   Windows   winget install mpv.net
#   macOS     brew install mpv
#   Linux     apt install mpv     (or dnf / pacman / zypper)

cargo build --release -p lumen-play
./target/release/lumen doctor
```

`doctor` tells you whether mpv is present, whether it has the `gpu-next` video output, and whether
hardware decoding is available. Worth reading before drawing conclusions from anything else: a
library that stutters on a machine with no hardware decoder is a driver finding, not a file finding.

## The four commands

```bash
lumen scan  ~/Media                      # what is in there, and what looks wrong
lumen items ~/Media                      # the collection, grouped into films and seasons
lumen play  ~/Media                      # watch it
lumen test  ~/Media --seconds 20         # open every file briefly, report which fail
```

**`test` is the one to run first on a large collection.** It opens each file for twenty seconds and
moves on, so a thousand files take a few hours rather than a few weeks, and the output is a list of
exactly which files failed and mpv's own reason for each.

```bash
lumen test ~/Media --seconds 15 --json library-report.json
```

Exit codes: `0` everything played, `1` at least one file failed, `2` usage or setup problem. The
failure code is deliberate — a run that could not open half the library must not report success to a
script.

## What it tells you

Beyond pass/fail, four things a play-through by hand would not surface:

- **Extension/content mismatches.** A `.mkv` whose bytes are not Matroska is usually a truncated
  download. Content decides what a file is here; the extension is only a hint, and disagreement is
  recorded rather than obeyed.
- **Software decoding.** Files that played, but with no hardware decoder. Not a failure — but a
  library that plays only because the CPU is carrying it will not play on a phone or a tablet, which
  is the whole question for a multi-platform product.
- **Unseekable files.** Plays forward, cannot be navigated: a lost Matroska Cues element or an
  unusable MP4 `moov`. A play-through test never notices, because playing forward still works. You
  find out the first time you try to skip.
- **HDR.** Decided by the transfer function (`pq`, `hlg`), not the primaries — BT.2020 with a
  conventional gamma curve is wide-gamut SDR, and conflating the two would misreport a distinction
  this product exists to get right.

## Options

```
--seconds <n>       play only n seconds of each file (default 20 for `test`)
--limit <n>         stop after n playable files
--depth <n>         maximum directory depth
--include-samples   keep files that look like sample clips
--shuffle           play in random order
--windowed          do not go fullscreen
--paused            start paused
--vo <name>         video output (default gpu-next)
--hwdec <mode>      hardware decoding (default auto-safe)
--dry-run           print the mpv command and playlist, launch nothing
--json <path>       write the machine-readable report here
--                  everything after this goes to mpv verbatim
```

`--dry-run` prints the exact mpv invocation. That is the difference between "the player did
something odd" and a diagnosis.

Playback controls are mpv's own — space, arrows, `f`, `q`. There is no UI of ours in the way.

## Design notes

**One broken file must never end the run.** `--keep-open=no` with `--idle=yes` means mpv steps past a
file it cannot open and waits at the end instead of exiting, so a thousand-file scan finishes even if
the first fifty are corrupt. That is the "no refusal" guarantee (`docs/11` §G2) applied to a playlist.

**Results are attributed by path, not by a counter.** mpv does not have to play the playlist in the
order it was written — `--shuffle` reorders it and mpv may skip an entry — so counting would record
every file's outcome against the wrong file.

**Events are never dropped while a command is in flight.** mpv interleaves events with replies
freely, so a property read at the wrong moment would swallow the `end-file` event carrying the reason
a file failed, and the outcome would silently become "unknown".

Zero dependencies beyond the workspace's own crates, including a hand-written JSON reader — mpv's
events carry file paths, and a path is exactly the kind of string full of braces, commas and quotes
that substring matching gets wrong silently.

## Status

64 tests. Everything that does not need a display is covered: the scanner against real directory
trees, the JSON parser against malformed and hostile input, the IPC event queueing, the playlist
ordering, and the report renderers.

**Playback itself has not been run.** It was built in an environment with no mpv, no GPU and no
display. The scan, parse, playlist, protocol and report layers are all tested; what has not been
exercised is the part where mpv actually opens a file. Expect to hit something on first run — start
with `lumen doctor`, then `lumen test <folder> --limit 5` before pointing it at everything.
