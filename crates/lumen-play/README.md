# `lumen` — playable now

A working player and library test harness. Point it at your collection; it plays everything and
records what happened to each file.

This is the runnable thing, ahead of the GUI. It uses the real crates — `lumen-probe` for
content-based container detection, `lumen-match` for filename parsing — so running it against your
library is also the first real test of that code on data that is not a fixture.

## Install

**Download a build.** `.github/workflows/release.yml` produces one per platform — Windows and
Linux — on every push that touches the workspace (attached to a rolling `desktop-latest`
prerelease), on every `v*` tag, and on demand via workflow dispatch. Grab the bundle for your
platform from the release or the Actions run and unpack it.

The **Windows** bundle is self-contained: `lumen.exe`'s C runtime is statically linked
(`target-feature=+crt-static`, see `.cargo/config.toml`), so it needs no Visual C++ Redistributable
installed — and it carries `mpv.exe` beside it (statically linked FFmpeg and libplacebo), so the
folder runs from a USB stick with nothing installed and nothing touching the registry. Both of those
claims are checked in `release.yml` on every build — the binary's import table for a stray
VCRUNTIME/MSVCP dependency, and mpv's own reported decoder list for the full codec set, TrueHD and
DTS-HD MA included — rather than assumed to still hold from an earlier release.

**Linux** is self-contained too, the same way Windows is, just with different tools: mpv is
vendored beside `lumen`, along with the codec/format/subtitle layer a bare OS install does not
already have (FFmpeg, libx264/x265, dav1d, libass, and the rest). What is deliberately *not*
vendored is GL/Vulkan/VA-API, the display server, the audio server, and the security/identity stack
— those have to be this machine's own, or hardware decode, window rendering, audio routing and
certificate validation would all be running against a frozen copy nothing ever updates. `lumen`
finds the vendored copy the same way it finds a bundled Windows one — beside its own executable,
checked first. `release.yml` proves the vendored pair actually decodes a file, with
`LD_LIBRARY_PATH` unset, on every build.

No mpv on the machine and building your own bundle instead of downloading one? One prerequisite,
to vendor *from*:

```bash
apt install mpv     # or dnf / pacman / zypper
```

**Or build it yourself:**

```bash
cargo build --release -p lumen-play
./target/release/lumen doctor
```

Building a bundle with mpv vendored in, on the platform you're building for:

```bash
./crates/lumen-play/package-linux.sh --with-mpv    # -> dist/lumen-linux-x86_64.tar.gz  (needs patchelf)
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

## The four library commands

```bash
lumen scan  ~/Media                      # what is in there, and what looks wrong
lumen scan  ~/Media --identify           # also find duplicate content
lumen items ~/Media                      # the collection, grouped into films and seasons
lumen play  ~/Media                      # watch it
lumen test  ~/Media --seconds 20         # open every file briefly, report which fail
```

These four read a library and report on it or play it, then exit. `lumen serve` (next section) is
the one that stays up. `lumen reindex` and `lumen verify` — a persisted index that re-probes only
what changed, and a byte-level re-check of indexed files against their last digest — are described in
`lumen --help` rather than here.

**`test` is the one to run first on a large collection.** It opens each file for twenty seconds and
moves on, so a thousand files take a few hours rather than a few weeks, and the output is a list of
exactly which files failed and mpv's own reason for each.

```bash
lumen test ~/Media --seconds 15 --json library-report.json
```

Exit codes: `0` everything played, `1` at least one file failed, `2` usage or setup problem. The
failure code is deliberate — a run that could not open half the library must not report success to a
script.

## `lumen serve`: a LAN media server

```bash
lumen serve ~/Media                                   # prints a pairing code and a TLS fingerprint
lumen serve ~/Media --port 7890 --bind 0.0.0.0        # the defaults, spelled out
lumen serve ~/Media --dlna                            # also a DLNA MediaServer for TVs (see below)
lumen serve ~/Media --dlna --dlna-port 7891 --dlna-bind 0.0.0.0 --dlna-name "lumen"
lumen serve ~/Media -- --ao=pulse                     # everything after -- goes to mpv verbatim
lumen unpair                                          # list paired devices; <prefix> or --all revokes
```

Three things behind one command. One persistent mpv, launched idle and driven by paired clients
over the LAN — a phone browses the library and plays, pauses, seeks and changes volume on the
machine `serve` runs on. Media delivery on that same port, so a client can also pull a file's bytes
to itself. And, only with `--dlna`, a DLNA MediaServer that any TV or renderer on the LAN can browse
and stream from with no pairing at all. It runs in the foreground until killed; the first line it
prints is "do not forward this port through your router; it is meant for your own LAN", and that is
the deployment model — a home network, not the internet.

**Pairing.** At start the terminal shows a six-digit code, valid for ten minutes and single-use: the
moment a client sends the right one it is consumed, and the client gets back a 128-bit token to
present on every later connection instead. Wrong guesses are capped at five per minute across every
connection combined, so a million-way code is not brute-forceable inside its own lifetime. There is
exactly one code per server start; to pair a second device, restart `serve` and read the new code.
Tokens persist across restarts in `paired-clients.txt` under the lumen config directory
(`$XDG_CONFIG_HOME/lumen` or `~/.config/lumen`; `%APPDATA%\lumen` on Windows), and `lumen unpair`
edits that file without the server running. This is not a login system — no username, no password,
no per-device names — because the threat it is sized to is "someone on this LAN who was not shown the
code", and no further.

**The fingerprint, and why trust-on-first-use.** Right beside the code the terminal prints the
SHA-256 fingerprint of a self-signed certificate the server generates once (825-day validity, kept
in the same config directory as the tokens) and presents on every connection. There is no domain
name to issue a real certificate for and no CA a home LAN server should be trusting, so a client does
what SSH does with host keys: it pins the fingerprint the moment it pairs, and refuses to reconnect if
a later connection ever presents a different one. Someone who was not present for the first pairing
cannot read the token off the wire on a later reconnect, and cannot silently swap in their own server.
The fingerprint is printed so a person can compare what the client shows against what the server
says; a mismatch on first pair is the one moment pinning cannot catch on its own. The `health`
message below reports how long that certificate has left, because pinning has no rotation story yet
and a hard connection failure is a worse way to find out than a warning.

**What a client can do.** The wire protocol is newline-delimited JSON over TLS, one object per line,
the same shape as mpv's own IPC (`remote/protocol.rs` explains why not WebSocket). Every request
carries an `id` the reply echoes; the server also pushes a `state` line — what is playing, where it
is, and a `library_version` counter — whenever playback changes, unprompted. The messages: `pair`
and `auth` (the only two allowed before authentication), `library`, `play`, `pause`, `resume`,
`toggle`, `seek`, `volume`, `next`, `previous`, `health` (mpv round-trip time, certificate expiry,
last `reindex` time, free disk on the library volume, connected-client count) and `rescan`. `play`
only accepts a path that resolves to somewhere under the scanned root, so a paired client — or a
stolen token — can open a file this server was pointed at, and nothing else the mpv process could
read.

**Rescan, and the watcher.** `rescan` re-walks the library root right now, replaces the in-memory
listing, and bumps `library_version`; the reply carries the new file count and version so a client
does not have to wait for the next state push to learn whether anything happened. A background
filesystem watcher (`notify`, recursive) calls the same function automatically once a burst of
on-disk changes has been quiet for 1.5 seconds, so a dropped-in file shows up on the phone without
anyone asking. The honest caveat: every rescan, manual or automatic, re-walks and re-probes
everything and bumps the version whether or not anything actually changed — there is no on-disk
index behind `serve` and no diff against the previous walk (that is `lumen reindex`'s job, and the
two are not wired together yet). A watcher that cannot start (an exhausted inotify limit, an
unsupported filesystem) logs a warning and leaves automatic refresh absent for that session; the
manual command still works, and the server still starts.

**Media routes**, all on the same TLS port, all needing the same token:

```
/stream/<path>?token=<token>                   the file's bytes; Range requests honoured (206)
/hls/<token>/<path>/playlist.m3u8              HLS: fMP4 init.mp4 + seg_NNNNN.m4s, 6 s segments
/dash/<token>/<path>/manifest.mpd              DASH, same source, same cache model
/vr?path=<path>&token=<token>                  a WebXR cinema page that plays /stream/<path>
```

`<path>` is the file's path exactly as the `library` reply listed it, URL-encoded, and is put through
the same containment check `play` uses. `/stream/` also accepts `Authorization: Bearer <token>`; the
query form exists because a `<video src>` cannot set a header. HLS and DASH carry the token in the
URL *path* for a related reason: a player resolves a playlist's relative segment URIs against the
playlist's own URL, and RFC 3986's merge rules drop the base URL's query, so a `?token=` on the
playlist would vanish from every segment fetch the player makes on its own.

HLS and DASH **need ffmpeg**; `/stream/` and `/vr` do not. Segmenting is stream-copy, not a
re-encode — the segments carry the source's own codecs, so this makes a file seekable and
fetchable in pieces, it does not make an unplayable codec playable. Output is generated on the first
request for a file, cached on disk under the config directory keyed on (path, size, mtime), and
evicted best-effort past 25 GiB or 14 days. `lumen` looks for ffmpeg the way it looks for mpv:
`LUMEN_FFMPEG` first, then beside its own executable (or in `ffmpeg/` or `ffmpeg/bin/` next to it),
then the usual install locations, then `PATH`. Unlike mpv, **ffmpeg is not in the release bundle**
and `lumen setup` does not fetch it; without it the server still starts and prints where to get it —
once, in its own terminal, at startup. An HLS or DASH request then gets a `503` whose whole body is
the one line `ffmpeg is not installed on this server`; the download instructions never reach the
client, so a phone that sees that 503 has to be told by whoever can read the server's terminal.

`/vr` is deliberately a small thing: one flat screen in a dark void, hand-rolled WebGL with no
library, no room, no seat choice, no library browsing (the caller passes the one `path`), no spatial
audio — the `<video>` element's ordinary stereo. It needs no flag because it needs no token of its
own: the page is the same bytes for everyone and does nothing except request `/stream/<path>`, which
is the one place a token is actually checked.

**`--dlna`.** Off by default and on its own plain-HTTP port (7891), because DLNA is unauthenticated by
protocol design: a renderer must be able to discover and browse a MediaServer with no handshake at
all, which cannot be reconciled with the pairing-plus-pinning model above, so it is kept as
separate infrastructure rather than bolted onto the paired listener. The server says so when you
turn it on — *`--dlna` is unauthenticated by protocol design; any device on this LAN can browse and
stream this library with no pairing or token* — and an operator who never passes the flag has
exactly the posture they had before it existed. What it does: SSDP announcement (re-sent every
fifteen minutes), a `ContentDirectory` whose `Browse` presents the real folder hierarchy (only
directories that actually contain a playable file get a node), `Search` over the two criteria real
renderers send (`*`, and `dc:title contains "..."`), and streaming of the files it lists. It also
follows the library: `serve --dlna` runs its own instance of the same debounced filesystem watcher
the paired channel uses (shared code, not shared state — each side re-walks and swaps its own
snapshot), so a file added, removed, or renamed while the server is running shows up in
`Browse`/`Search` on its own, typically within a few seconds (a 1.5 s debounce after the last event,
then a 1.5 s quiet period). Every refresh increments `SystemUpdateID`, which is what every
`Browse`/`Search` reply carries as `UpdateID` and what `GetSystemUpdateID` — the third action the
`ContentDirectory` declares and answers — returns, so a renderer that polls it learns exactly when to
re-`Browse` its cached listings. Object ids, and therefore the `/dlna/stream/f<n>` URLs in `<res>`,
are issued once per path and never re-pointed by a refresh: a removed file's URL becomes an honest
`404`, never another file's bytes mid-playback, and a path that comes back is a new object. Ids and
the counter reset on restart, which the spec permits (clients cache neither across a `byebye`).
Anything other than those three actions gets a SOAP fault (HTTP 500 carrying UPnP error 401,
`Invalid Action`). The full UPnP search grammar and sorting are not implemented, on purpose —
`lumen-discovery`'s own docs say why.

**The Android client** lives in `android/app/src/main/kotlin/dev/lumen/player/remote/`: pairing,
the library, the playback controls — `play`, `pause`, `resume`, `toggle`, `seek`, `volume` — plus
`rescan` and `health`. "Rescan server" sits beside "Refresh list", labelled for what each does: the
first asks the server to re-walk its disk, the second only re-fetches the listing. A small "Server"
card shows the five `health` fields in human units — mpv round-trip, certificate expiry (or how long
ago it lapsed), last reindex ("never reindexed" when the server has none), free disk in GiB,
connected clients — with a `null` from the server shown as unknown, never as zero. It reacts to
`library_version`: every `state` push is compared with the version the listing was last requested
under on this connection, and a change re-fetches the listing on its own, so a file the server's
watcher just picked up appears without anyone tapping anything. `rescan` is the one request with a
longer reply timeout (ten minutes rather than eight seconds, because the server walks the whole
library synchronously before answering), and every write to the socket is serialised so two
concurrent requests can never share a line on the wire. It pins the fingerprint (`RemoteTls.kt`) and stores it beside the token, so
a reinstalled server is a "re-pair to trust the new one" message, not a silent reconnect. It is
built, and its JVM unit tests run, by `.github/workflows/android.yml` on every push that touches
`android/`; that workflow's emulator job proves the APK installs and launches, and nothing more.
Nothing in this repository's own runs has exercised the client against a real `lumen serve` on a
real device — the sandbox this is developed in cannot reach `dl.google.com`, so the Android
toolchain only exists on the CI runner.

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

The player and scanner lean on the workspace's own crates plus a hand-written JSON reader — mpv's
events carry file paths, and a path is exactly the kind of string full of braces, commas and quotes
that substring matching gets wrong silently. `lumen serve` is where the external dependencies live,
and they are there for two different reasons. `rustls`, `rcgen` and `sha2` — the pinned TLS
certificate, its generation, and its fingerprint — exist because std has no TLS, no X.509 and no
SHA-256, and none of those is something to write by hand for a security boundary; `time` rides along
because `rcgen`'s validity fields are typed with it and it was already in the graph. The rest are
each a platform call that `unsafe_code`, denied workspace-wide, rules out hand-rolling: `getrandom`
for pairing codes and tokens, `fs4` for the free-disk figure in `health`, `notify` for the library
watcher, and on Windows `interprocess` for the named-pipe IPC transport. `Cargo.toml` argues that
second group individually; the TLS trio carries no comment there, which is the one place this
paragraph is the only justification on record.

## Status

CI runs tests, clippy and rustfmt on Linux and Windows for every push, plus the ADR-0002 licence
gate. The platform matrix is not decoration: the mpv IPC transport is a Unix socket on one and a
named pipe on the other, and the environment probe shells out to different tools per OS.

269 tests in this crate (262 unit, 7 integration — the latter start a real `lumen serve` and a real
DLNA listener and talk to them over real sockets) and 831 across the workspace's `cargo test
--workspace`, doc-tests aside; plus end-to-end runs against real encoded media.

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
