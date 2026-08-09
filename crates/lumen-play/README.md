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

The **Windows** bundle is self-contained: `lumen.exe`'s C runtime is statically linked
(`target-feature=+crt-static`, see `.cargo/config.toml`), so it needs no Visual C++ Redistributable
installed — and it carries `mpv.exe` beside it (statically linked FFmpeg and libplacebo), so the
folder runs from a USB stick with nothing installed and nothing touching the registry. Both of those
claims are checked in `release.yml` on every build — the binary's import table for a stray
VCRUNTIME/MSVCP dependency, and mpv's own reported decoder list for the full codec set, TrueHD and
DTS-HD MA included — rather than assumed to still hold from an earlier release.

**Linux and macOS** are self-contained too, the same way Windows is, just with different tools:
mpv is vendored beside `lumen`, along with the codec/format/subtitle layer a bare OS install does
not already have (FFmpeg, libx264/x265, dav1d, libass, and the rest). What is deliberately *not*
vendored is GL/Vulkan/VA-API, the display server, the audio server, and the security/identity stack
— those have to be this machine's own, or hardware decode, window rendering, audio routing and
certificate validation would all be running against a frozen copy nothing ever updates. `lumen`
finds the vendored copy the same way it finds a bundled Windows one — beside its own executable,
checked first. `release.yml` proves the vendored pair actually decodes a file, with
`LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH` unset, on every build.

No mpv on the machine and building your own bundle instead of downloading one? Same one prerequisite
either way, to vendor *from*:

```bash
#   macOS     brew install mpv
#   Linux     apt install mpv     (or dnf / pacman / zypper)
```

**Or build it yourself:**

```bash
cargo build --release -p lumen-play
./target/release/lumen doctor
```

Building a bundle with mpv vendored in, on the platform you're building for:

```bash
./crates/lumen-play/package-linux.sh --with-mpv    # -> dist/lumen-linux-x86_64.tar.gz  (needs patchelf)
./crates/lumen-play/package-macos.sh  --with-mpv    # -> dist/lumen-macos-<arch>.tar.gz
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
- **Fidelity tier per file, on two endpoints.** See below.

## Fidelity: how well it played, and what it would cost elsewhere

"It played" is the floor, not the charter. Every file that opens is put through the real decision
ladder (`lumen-playback`) against two declared capability profiles (`lumen-caps`), and the run
reports the T0–T5 tier from `docs/11` §1.1 that each would reach:

```
fidelity (7 files) — modelled from what each file demuxed, not measured
  native   T0 3  T1 3  T3 1
  browser  T1 5  T3 2
  1 play untouched on a native client and cannot in a browser

below T2 natively (1) — adapted even on a fully capable client
  Old Movie (1998)  T3
      This device has no decoder for Mpeg4Part2.
```

The two profiles are the ends of the range a multi-platform product has to live across: a native
client with Matroska, hardware HEVC/AV1, an HD AVR and an HDR display; and a browser, which is
fMP4/WebM only, stereo PCM, SDR, text subtitles. A UHD remux that reaches T0 on the first and T3 on
the second is not a defect — it is the fact you need before promising the file plays "everywhere".

**Modelled, not measured, and labelled that way.** The stream description is a real demux of a real
file, so the input is observation. The endpoint is a declared profile, so the output is what those
capabilities *would* yield, not what any particular device did. Every degraded outcome carries the
ladder's own reasons (guarantee G1, `docs/11`), so a tier is never a number without a cause.

Files that did not open get `null` rather than `T5`: a tier for a file that never demuxed would be
fiction, and `null` says so where a default would not.

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

**The fidelity assessment uses mpv's own track selection, not ours.** This file really played, and
describing a selection that did not happen would be a worse answer than the one in front of us.
`lumen-playback`'s automatic selector stands in only when mpv marked nothing selected at all.

One external dependency, reached transitively: `lumen-identity` uses `xxhash-rust` for the content
sketch. Everything else is the workspace's own crates plus a hand-written JSON reader — mpv's
events carry file paths, and a path is exactly the kind of string full of braces, commas and quotes
that substring matching gets wrong silently.

## Status

CI runs tests, clippy and rustfmt on Linux, macOS and Windows for every push, plus the ADR-0002
licence gate. The platform matrix is not decoration: the mpv IPC transport is a Unix socket on one
and a named pipe on the other, and the environment probe shells out to different tools per OS.

96 tests in this crate and 474 across the workspace, plus end-to-end runs against real encoded media.

The Windows binary under Wine (H.264 in Matroska and MP4, MPEG-4 part 2 in AVI, and a deliberately
corrupt file): five files, four played, one correctly reported as `unrecognized file format`, every
resolution and codec attributed to the right file, exit code 1.

The Linux binary against an eight-file corpus encoded for the purpose — H.264/AAC in Matroska and
MP4, H.264/FLAC, H.264/AC-3, VP9/Opus in WebM, HEVC 10-bit PQ with TrueHD in Matroska, MPEG-4 part 2
with AC-3 in AVI, and a corrupt file: seven played, one correctly failed, and the fidelity model
answered as it should on each. The HEVC/PQ/TrueHD remux reached T0 natively and T3 in a browser; the
XviD-in-AVI file reached T3 on both, correctly attributed to the absent MPEG-4 part 2 decoder rather
than to the container.

Three bugs the real runs caught, all invisible to unit tests:

- mpv given a playlist **on the command line** starts playing before this process can connect, so
  the first `start-file` event is gone before anything is listening.
- Reading the `path` property on `start-file` looks like a fix and is not: at that moment mpv still
  reports the *previous* file.

Together those shifted every result one position — a report in which a corrupt 18-byte file was
credited with 320x240 MPEG-4 video belonging to the next file in the list. Plausible, entirely
wrong, and invisible without checking the output against known inputs. The playlist is now sent
over IPC after connecting, and results are keyed on `playlist_entry_id`, which rides on the events
themselves and cannot race.

The Linux corpus run caught a second one, in the scanner rather than the player: **every WebM file
was being identified as Matroska.** WebM and Matroska share the EBML magic and are separated only by
the header's `DocType`, so both come back as candidates with equal confidence — and the tie-break
used `max_by_key`, which returns the *last* maximum, quietly reversing the order `sniff` had
deliberately sorted them into. The cost was not cosmetic: a browser opens WebM and cannot open
Matroska, so every `.webm` in a library was reported as needing a remux it does not need. The probe
now reads the `DocType`, and the scanner takes the first candidate rather than the last-strongest.

**Not yet exercised:** real GPU decoding and rendering. Every verification so far ran with
`--vo=null` on a machine with no GPU, so hardware decode paths, `gpu-next`, HDR tone mapping and
frame pacing are still untested. The fidelity tiers are likewise modelled against declared profiles
rather than measured on the devices they name. `LUMEN_DEBUG_EVENTS=1` dumps the raw mpv event stream
if something looks wrong.
