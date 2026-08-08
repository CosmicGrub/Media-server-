# This branch: Samsung Galaxy Z Fold 5

One of three device branches (`device/galaxy-z-fold-5`, `device/galaxy-tab-s9-fe`,
`device/windows-pc`) kept alongside the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`, merging toward `main`). Read
[**How this branch works**](#how-this-branch-works) before pushing anything to it — the short
version is: probably don't, push to the development branch instead.

## Hardware this targets

| | Cover screen | Inner screen |
|---|---|---|
| Size | 6.2" | 7.6" |
| Resolution | 2316×904 | 2176×1812 |
| Aspect | ~23.1:9 | ~6:5 |

Snapdragon 8 Gen 2 (Galaxy variant). Launched on Android 13 (One UI 5.1.1), upgradable through
whatever One UI version Samsung currently ships for it. The app's `minSdk 24` / `targetSdk 35`
comfortably spans that whole range.

## What the app does specifically because of this hardware

The Fold 5 is the device the adaptive layout in `android/app/src/main/kotlin/dev/lumen/player/ui/`
was designed against, so everything here has been exercised on real hardware, not just an emulator:

- **Tabletop posture** (half-open, standing on a surface): video fills the top half above the
  crease, transport controls and the library live below it — hands-free, and the one arrangement
  that only exists because this device folds.
- **Inner display, flat**: video and library side by side (`SideBySide`).
- **Cover screen**: video on top, library filling the rest (`Stacked`), scaled to whatever share of
  the height the user sets — not the fixed 16:9 box an early version used, which double-letterboxed
  a scope-ratio film on this screen specifically.
- **View modes** (Split / Theater / Immersive), gestures (swipe-seek, brightness, volume,
  double-tap), Picture-in-Picture, resume position keyed on file content rather than path, audio and
  subtitle track selection, and the remote-control client for pairing with a `lumen serve` instance
  — see `device/windows-pc` for the other end of that connection.

## Known gaps on this device specifically

- **TrueHD, DTS-HD MA, DTS, DTS:X do not decode.** No Android OEM licenses hardware or software
  decode for these; a remux carrying one of them plays picture with no sound on this codec path
  until a decoder extension is built. Deferred rather than silently shipped — the fidelity-tier
  system on the desktop side already reports this as a known cost rather than hiding it.
- **PGS subtitle rendering exists** (`androidx.media3.extractor.text.pgs`) — earlier documentation
  in this project incorrectly claimed it did not; that was corrected once checked against the actual
  Media3 source. There is a tracked upstream bug about PGS being stretched to the wrong aspect ratio
  in Media3 1.8.0. Not confirmed present or absent on this device without a PGS-carrying file to
  test against.

## Install

**https://github.com/CosmicGrub/Media-server-/releases/tag/android-latest** — open on the phone,
tap `lumen.apk`, allow installation from the browser. The release refreshes on every push to the
development branch that touches `android/`; this device branch does not carry its own separate
build.

Over USB instead:
```bash
adb install -r lumen.apk
```

## How this branch works

**This branch does not contain device-specific code.** The Fold 5, the Tab S9 FE and any future
Android device all run the same APK; what changes per device is the runtime posture the adaptive
layout resolves to, not the source. Forking the Kotlin source three ways would mean every future
bugfix has to be applied three times and would drift the moment one of those applications is missed
— exactly the failure mode `docs/` warns against for the playback ladder, applied here to source
control instead of runtime logic.

So: this branch exists to be a **stable, discoverable landing point** for "what does Lumen do on a
Fold 5" — real documentation, not a bare pointer — and it tracks the development branch's tip rather
than diverging from it. If you're changing behaviour, change it on
`claude/multiplatform-media-player-server-b7dwe7` (or `main`, once this reaches it) and let this
branch's tip be fast-forwarded to include it, the same way the other two device branches pick up the
same fix automatically. A change committed only here would silently stop existing on every other
device.
