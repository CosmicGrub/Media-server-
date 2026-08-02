# `lumen` — playable now

A working player and library test harness. Point it at your collection; it plays everything and
records what happened to each file.

This is the runnable thing, ahead of the GUI. It uses the real crates — `lumen-probe` for
content-based container detection, `lumen-match` for filename parsing — so running it against your
library is also the first real test of that code on data that is not a fixture.

## Install

**Download a build.** `.github/workflows/release.yml` produces one per platform — Windows,
Linux, and both Mac architectures — on every `v*` tag, and on demand via workflow dispatch. Grab
the artifact for your platform from the Actions run and unpack it.

The **Windows** bundle is self-contained: it carries `mpv.exe` (statically linked FFmpeg and
libplacebo) beside `lumen.exe`, so the folder runs from a USB stick with nothing installed. Nothing
touches the registry; deleting the folder undoes everything.

**Linux and macOS** bundles ship the binary alone, because mpv there is dynamically linked against
a long dependency chain and shipping that correctly means shipping a distribution — which the
package manager already is:

```bash
#   macOS     brew install mpv
#   Linux     apt install mpv     (or dnf / pacman / zypper)
```

**Or build it yourself:**

```bash
cargo build --release -p lumen-play
./target/release/lumen doctor
```

Cross-compiling a Windows bundle from Linux, which is how the first one was made:

```bash
apt-get install -y gcc-mingw-w64-x86-64 mingw-w64-x86-64-dev
rustup target add x86_64-pc-windows-gnu
./crates/lumen-play/package-windows.sh --with-mpv   # -> dist/lumen-windows-x86_64.zip
```

`lumen` finds mpv beside its own executable first, then on `PATH`, then in the usual install
locations. `LUMEN_MPV=/path/to/mpv` overrides all of it. Bundled-first is deliberate: a copy you
put next to the binary is one you chose, and quietly preferring an older system install would show
up only as a file that mysteriously fails to play.

`doctor` tells you whether mpv is present, whether it has the `gpu-next` video output, and whether
hardware decoding is available. Worth reading before drawing conclusions from anything else: a
library that stutters on a machine with no hardware decoder is a driver finding, not a file finding.

## The four commands

```bash
lumen scan  ~/Media                      # what is in there, and what looks wrong
lumen scan  ~/Media --identify           # also find duplicate content
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
- **Duplicate content** (with `--identify`). The same bytes under two different release names —
  invisible to any filename-based check, which is exactly why a library accumulates them. Identity
  is content-derived, so it survives rename, move and remount. Off by default: it reads up to 3 MiB
  a file against the sniffer's 4 KiB, which over a network share is the difference between seconds
  and an afternoon.
- **HDR.** Decided by the transfer function (`pq`, `hlg`), not the primaries — BT.2020 with a
  conventional gamma curve is wide-gamut SDR, and conflating the two would misreport a distinction
  this product exists to get right.

## Options

```
--seconds <n>       play only n seconds of each file (default 20 for `test`)
--limit <n>         stop after n playable files
--depth <n>         maximum directory depth
--identify          content identity per file; finds duplicates (extra I/O)
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

**Results are keyed on mpv's `playlist_entry_id`, not on a counter or a path lookup.** mpv need not
play the playlist in the order it was written, and the entry id rides on the events themselves so it
cannot race a property read. See Status below for the bug this replaced.

**Events are never dropped while a command is in flight.** mpv interleaves events with replies
freely, so a property read at the wrong moment would swallow the `end-file` event carrying the reason
a file failed, and the outcome would silently become "unknown".

One external dependency, reached transitively: `lumen-identity` uses `xxhash-rust` for the content
sketch. Everything else is the workspace's own crates plus a hand-written JSON reader — mpv's
events carry file paths, and a path is exactly the kind of string full of braces, commas and quotes
that substring matching gets wrong silently.

## Status

CI runs tests, clippy and rustfmt on Linux, macOS and Windows for every push, plus the ADR-0002
licence gate. The platform matrix is not decoration: the mpv IPC transport is a Unix socket on one
and a named pipe on the other, and the environment probe shells out to different tools per OS.

76 tests in this crate and 453 across the workspace, plus an end-to-end run of the Windows binary
against real encoded media (H.264 in
Matroska and MP4, MPEG-4 part 2 in AVI, and a deliberately corrupt file) under Wine: five files,
four played, one correctly reported as `unrecognized file format`, every resolution and codec
attributed to the right file, exit code 1.

That last point was a bug the real run caught, and it is worth knowing about:

- mpv given a playlist **on the command line** starts playing before this process can connect, so
  the first `start-file` event is gone before anything is listening.
- Reading the `path` property on `start-file` looks like a fix and is not: at that moment mpv still
  reports the *previous* file.

Together those shifted every result one position — a report in which a corrupt 18-byte file was
credited with 320x240 MPEG-4 video belonging to the next file in the list. Plausible, entirely
wrong, and invisible without checking the output against known inputs. The playlist is now sent
over IPC after connecting, and results are keyed on `playlist_entry_id`, which rides on the events
themselves and cannot race.

**Not yet exercised:** real GPU decoding and rendering. The Wine verification ran with `--vo=null`
on a machine with no GPU, so hardware decode paths, `gpu-next`, HDR tone mapping and frame pacing
are still untested. `LUMEN_DEBUG_EVENTS=1` dumps the raw mpv event stream if something looks wrong.
