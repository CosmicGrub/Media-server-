# This branch: Windows PC

One of three device branches (`device/galaxy-z-fold-5`, `device/galaxy-tab-s9-fe`,
`device/windows-pc`) kept alongside the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`, merging toward `main`). Read
[**How this branch works**](#how-this-branch-works) before pushing anything to it — the short
version is: probably don't, push to the development branch instead.

## What this targets, and the Windows 10 / 11 claim specifically

`lumen.exe` — the player and library test harness in `crates/lumen-play` — plus `lumen serve`, the
persistent remote-controllable player the two Android device branches pair with over the LAN.

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
release — tag pushes are blocked (403) in the environment this was built in, and `workflow_dispatch`
needs the workflow file to exist on `main`, which as of this writing it does not (`main` is several
commits behind this line of work; the workflow lives only on the development branch and its
descendants, including this one). The `package-windows.sh` cross-compiled path *has* been verified,
including a five-file real-media run under Wine on a machine with no GPU. Real GPU rendering, on
either build path, remains unverified.

## How this Windows PC fits with the two Android branches

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

The bundle is self-contained: `mpv.exe` (statically linked FFmpeg and libplacebo) sits beside
`lumen.exe`, so the folder runs from a USB stick with nothing installed and nothing touches the
registry. `lumen.exe doctor` checks the machine; `lumen.exe serve <path>` starts the server the
phone pairs with.

## How this branch works

**This branch does not contain platform-specific code either.** The Rust workspace is already one
source tree building for Linux, macOS and Windows from CI on every push — see `.github/workflows/
rust.yml`'s three-platform matrix — so there was never a reason to fork it for "the Windows one."
This branch exists as a documentation landing point (what does Windows compatibility actually mean
here, how do the two build paths differ, how does this machine relate to the two phone/tablet
branches) that tracks the development branch's tip. A platform-specific fix belongs upstream on
`claude/multiplatform-media-player-server-b7dwe7` (or `main`, once this reaches it), same as the two
device branches.
