# This branch: Windows PC

This is an **independent fork**, not a landing page. It started from the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`) at commit `9cb8b0d` and now diverges from it on
purpose: this branch's own history is the record of what changed here, and nothing about it is
fast-forwarded from elsewhere automatically. Two sibling forks exist the same way —
`device/galaxy-z-fold-5` and `device/galaxy-tab-s9-fe` — each moving independently from the same
starting point.

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

**Not yet exercised end to end on either target:** `release.yml` has not produced a real tagged
release — tag pushes are blocked (403) in the environment this was built in. The
`package-windows.sh` cross-compiled path *has* been verified, including a five-file real-media run
under Wine on a machine with no GPU. Real GPU rendering, on either build path, remains unverified.

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

## How this branch works

This is a real fork: commit here, and the history diverges from the development branch and from the
other two device forks starting at their common ancestor `9cb8b0d`. Nothing rebases or
fast-forwards this branch automatically.

That independence cuts both ways:

- A fix or addition made only here (a Windows-tooling script like the one above, a
  `package-windows.sh` change) stays local to this fork unless someone deliberately cherry-picks or
  merges it elsewhere. It will not appear on either Android fork or the shared development branch on
  its own — nor should most of it, since most of what belongs here is Windows-only by nature.
- Conversely, a fix landed on the development branch (a protocol change in `remote/`, a shared bug
  fix elsewhere in `crates/`) does not reach this fork automatically either. Pulling those in is a
  deliberate merge (`git merge claude/multiplatform-media-player-server-b7dwe7`), not something that
  happens by default — this matters more here than on the Android forks, since this is the *server*
  side of the pairing protocol and a protocol change upstream needs to be merged in before it can
  speak to an Android fork built against the newer wire format.

In short: propagation between forks is manual and intentional in both directions, not automatic in
either.
