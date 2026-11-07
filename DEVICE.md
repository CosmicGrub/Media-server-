# This branch: Windows PC

This document was written while `device/windows-pc` was an **independent fork**, not a landing
page: it started from the shared development branch (`claude/multiplatform-media-player-server-b7dwe7`)
at commit `9cb8b0d` and diverged from it on purpose, with its own history as the record of what
changed here. That fork has since been merged into `main` (PR #4, `db27e6a`), along with
`device/galaxy-z-fold-5` (PR #3, `ee9648b`) — of the three sibling forks, only
`device/galaxy-tab-s9-fe` still moves independently. What follows describes how this Windows-specific
work was built and what it adds; treat the file paths and build details as current, and the
fork-independence framing as history.

## What this targets, and the Windows 10 / 11 claim specifically

`lumen.exe` — the player and library test harness in `crates/lumen-play` — plus `lumen serve`, the
persistent remote-controllable player the two Android device forks pair with over the LAN.

Built two ways, both plain console Win32 programs with no Windows-11-only API in the source:

- **`.github/workflows/release.yml`** builds natively on a `windows-latest` GitHub runner, target
  `x86_64-pc-windows-msvc`.
- **`crates/lumen-play/package-windows.sh`** cross-compiles from Linux with `mingw-w64`, target
  `x86_64-pc-windows-gnu`, and verifies the resulting `.exe` imports only Windows system DLLs — no
  MinGW runtime to ship alongside, checked by `objdump` against `libgcc`/`libwinpthread`/`libstdc++`
  as part of the build itself rather than assumed.

Neither target, nor anything in the source, calls a Windows-11-specific API — this is a console
tool that walks a filesystem, speaks JSON over a socket, and launches `mpv.exe`. **Windows 10 and 11
compatibility is a property of not doing anything version-specific, not a feature that needed
building**, and that has been true since the first Windows build in this project.

`release.yml`'s actual release mechanism is the rolling `desktop-latest` prerelease it publishes on
every push to this branch, not a `vX.Y.Z` tag — the workflow was built that way deliberately, so a
build always exists at the tip of active work rather than only at the versions someone remembered to
tag. That path **has** been exercised for real, confirmed against the GitHub API rather than
assumed: `desktop-latest` has been live since 2026-08-09, its `lumen-windows-x86_64.zip` and
`lumen-linux-x86_64.tar.gz` assets rebuilt and re-uploaded by a real `windows-latest`/`ubuntu-latest`
run on every push since, most recently confirmed 2026-08-31.

A genuine `vX.Y.Z` tag push, separately, is still blocked (403) — confirmed again on 2026-08-31,
after the repo went public, so this is a property of the environment this project is developed in,
not of the repo's visibility. It would not itself produce a different or more-verified artifact even
if it worked, since nothing in `release.yml` treats a tag push differently from the rolling release
above; it just is not exercisable from here.

**Still not exercised on either build target:** real GPU rendering. The `package-windows.sh`
cross-compiled path *has* been verified beyond compiling, including a five-file real-media run under
Wine on a machine with no GPU; the natively-built `release.yml` path decodes a real file as part of
its own CI run (`Verify real playback`) but that CI runner has no GPU either. Neither has been run on
a machine with one.

## What is actually specific to this fork

- **`scripts/windows/Install-LumenServeTask.ps1`** — a genuinely Windows-specific addition that has
  no equivalent on the other forks, because it's solving a Windows-only problem: `lumen serve`
  started from a console window dies the moment that window closes or the user signs out, and a
  server meant to sit on a LAN for a phone to pair with whenever it likes needs to outlive that.
  This registers it as a Windows Scheduled Task instead — starts automatically at sign-in, restarts
  itself if it ever exits, needs no window left open — using only the built-in `ScheduledTasks`
  module, so it runs unchanged on both Windows 10 and 11. `.\Install-LumenServeTask.ps1
  -LibraryPath "D:\Media"` to install, `.\Install-LumenServeTask.ps1 -Uninstall` to remove it; see
  the script's own comment-based help (`Get-Help .\Install-LumenServeTask.ps1 -Full`) for the rest,
  including running more than one library on different ports.
- **`crates/lumen-play/package-windows.sh`** now copies that script into the release bundle
  alongside `lumen.exe`, and `START-HERE.txt` documents it as step 6.
- The rest of `crates/`, `spikes/`, and the CI workflows here are currently identical to the
  development branch at the point this fork diverged — the Rust workspace already builds for
  Linux and Windows from the same source (`.github/workflows/rust.yml`'s two-platform matrix), so
  there was never a reason to fork *that* two ways. What this fork is for is a place to land
  genuinely Windows-specific tooling like the task-scheduler script above, without it needing to
  make sense on a phone.

## How this Windows PC fits with the two Android forks

This is the server half. `lumen serve <library path>` runs a persistent mpv session behind a
paired, token-authenticated TCP connection (`crates/lumen-play/src/remote/`); the Android app on
either `device/galaxy-z-fold-5` or `device/galaxy-tab-s9-fe` is the client that pairs with it,
browses its library, and drives its playback. See `crates/lumen-play/src/remote/protocol.rs` for
the wire format both sides speak.

## Install

Grab a build from `.github/workflows/release.yml`'s artifacts once it has run (see the caveat
above), or build it yourself — no Windows machine required:

```bash
apt-get install -y gcc-mingw-w64-x86-64 mingw-w64-x86-64-dev
rustup target add x86_64-pc-windows-gnu
./crates/lumen-play/package-windows.sh --with-mpv   # -> dist/lumen-windows-x86_64.zip
```

The bundle is self-contained: `mpv.exe` (statically linked FFmpeg and libplacebo) and
`Install-LumenServeTask.ps1` sit beside `lumen.exe`, so the folder runs from a USB stick with
nothing installed and nothing touches the registry until you deliberately ask for the Scheduled
Task. `lumen.exe doctor` checks the machine; `lumen.exe serve <path>` starts the server the phone
pairs with; `Install-LumenServeTask.ps1 -LibraryPath <path>` makes that survive reboots and sign-outs.

## How this branch worked

This was a real fork: commits here diverged the history from the development branch and from the
other two device forks starting at their common ancestor `9cb8b0d`, and nothing rebased or
fast-forwarded it automatically.

That independence cut both ways, while it lasted:

- A fix or addition made only here (a Windows-tooling script like the one above, a
  `package-windows.sh` change) stayed local to this fork unless someone deliberately cherry-picked or
  merged it elsewhere — it did not appear on either Android fork or the shared development branch on
  its own, nor should most of it have, since most of what belongs here is Windows-only by nature.
- Conversely, a fix landed on the development branch (a protocol change in `remote/`, a shared bug
  fix elsewhere in `crates/`) did not reach this fork automatically either; pulling those in took a
  deliberate merge, not something that happened by default — this mattered more here than on the
  Android forks, since this is the *server* side of the pairing protocol and a protocol change
  upstream needed to be merged in before it could speak to an Android fork built against the newer
  wire format.

That manual, intentional propagation is now settled history: `device/windows-pc` was merged into
`main` via PR #4 (`db27e6a`, with `ea89755` and `44f3604` carrying the cross-fork merges), and
`device/galaxy-z-fold-5` via PR #3 (`ee9648b`, `4c93722`) — both landing changes that had crossed
these fork boundaries, such as the Android "Book posture" feature (`24b5712`). `device/windows-pc`'s
own history, including this file, is part of that merged state. Only `device/galaxy-tab-s9-fe`
still propagates manually, the way described above.
