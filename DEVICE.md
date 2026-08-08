# This branch: Samsung Galaxy Tab S9 FE

One of three device branches (`device/galaxy-z-fold-5`, `device/galaxy-tab-s9-fe`,
`device/windows-pc`) kept alongside the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`, merging toward `main`). Read
[**How this branch works**](#how-this-branch-works) before pushing anything to it — the short
version is: probably don't, push to the development branch instead.

## Hardware this targets

10.9" flat display, 2304×1440, ~8:5, up to 90Hz. Exynos 1380. No hinge, no folding posture — this
is a plain large tablet, which the adaptive layout treats as always `Posture.Flat`. Launched on
Android 13 (One UI 5.1.1). The app's `minSdk 24` / `targetSdk 35` comfortably spans it.

## What the app does specifically because of this hardware

The Fold 5's inner display, flat and unfolded, is close to a small tablet in shape — 7.6" at
~6:5 versus this device's 10.9" at ~8:5 — and the adaptive layout logic in
`android/app/src/main/kotlin/dev/lumen/player/ui/PlayerScreen.kt` was written to key off window
*size and shape*, not the specific device, for exactly this reason. On this hardware it always
resolves to:

- **`SideBySide`** in landscape — video and library across from each other, the same arrangement
  built for and verified against the Fold 5's inner display.
- **`Stacked`** in portrait, or if a split-screen multitasking window narrows it enough — video
  above the library, sized by the same adjustable height share rather than a fixed box.

Never `Tabletop`, since that arrangement exists only for a physical hinge this device does not have.

Everything else in the app — view modes, gestures, Picture-in-Picture, resume position, track
selection, the remote-control client for pairing with a `lumen serve` instance (see
`device/windows-pc`) — is identical to the Fold 5 branch, because it is the same code.

**Worth being honest about:** the arrangement logic was designed and tested against the Fold 5.
This device is a real large flat screen and the *first* one the `SideBySide`/`Stacked` split has
run on outside an assumption written for a folding phone's inner display — it should hold, since
the logic keys on `screenWidthDp`/`screenHeightDp` rather than anything Fold-specific, but it has
not been confirmed on this exact hardware the way the Fold 5's own postures have been.

## Known gaps on this device

Same two as the Fold 5 branch, because it is the same codec/subtitle stack: TrueHD/DTS-HD MA/DTS/
DTS:X do not decode (deferred, disclosed rather than hidden), and PGS subtitle rendering exists but
has an upstream Media3 aspect-ratio bug tracked for 1.8.0 that is not confirmed present or absent
here without a PGS file to test against.

## Install

**https://github.com/CosmicGrub/Media-server-/releases/tag/android-latest** — open on the tablet,
tap `lumen.apk`, allow installation from the browser. The release refreshes on every push to the
development branch that touches `android/`; this device branch does not carry its own separate
build.

Over USB instead:
```bash
adb install -r lumen.apk
```

## How this branch works

**This branch does not contain device-specific code.** See `device/galaxy-z-fold-5`'s copy of this
section for the full reasoning — the short version is that one APK serves every Android device this
project targets, and forking the source per device would mean applying every future fix three times
and drifting the moment one of those applications is missed. This branch is a documentation landing
point that tracks the development branch's tip, not an independent line of development.
