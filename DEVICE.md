# This branch: Samsung Galaxy Tab S9 FE

This is an **independent fork**, not a landing page. It started from the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`) at commit `9cb8b0d` and now diverges from it on
purpose: this branch's own history is the record of what changed here, and nothing about it is
fast-forwarded from elsewhere automatically. Two sibling forks exist the same way —
`device/galaxy-z-fold-5` and `device/windows-pc` — each moving independently from the same starting
point.

## Hardware this targets

10.9" flat display, 2304×1440, ~8:5, up to 90Hz. Exynos 1380. No hinge, no folding posture — this
is a plain large tablet, which the adaptive layout treats as always `Posture.Flat`. Launched on
Android 13 (One UI 5.1.1). The app's `minSdk 24` / `targetSdk 35` comfortably spans it.

## What is actually specific to this fork

- **`applicationId dev.lumen.player.tabs9fe`** (`android/app/build.gradle.kts`), distinct from the
  shared build's `dev.lumen.player`. This is what makes the difference real rather than cosmetic: a
  Tab S9 FE can have this build and the shared development build installed side by side, and the
  two are separately signed, separately updated packages as far as Android is concerned.
  `namespace` stays `dev.lumen.player` — that only controls the compile-time `R`/`BuildConfig`
  package, not anything the OS or the user sees, and no Kotlin file in this app references `R.*`
  directly, so there is nothing for changing only the `applicationId` to break.
- **App label** "Lumen (Tab S9 FE)" (`android/app/src/main/res/values/strings.xml`), so the two
  builds are distinguishable in the launcher and app switcher rather than showing as two icons both
  named "Lumen".
- **Its own release channel.** `.github/workflows/android.yml` on this branch publishes to
  `android-latest-tabs9fe` (see [Install](#install)), gated on pushes to `device/galaxy-tab-s9-fe`
  specifically, instead of sharing the `android-latest` tag the development branch publishes to.
  Pushing here cannot clobber that release, and pushing to the development branch cannot clobber
  this one.

## What the app does because of this hardware (inherited, not forked)

The Fold 5's inner display, flat and unfolded, is close to a small tablet in shape — 7.6" at
~6:5 versus this device's 10.9" at ~8:5 — and the adaptive layout logic in
`android/app/src/main/kotlin/dev/lumen/player/ui/PlayerScreen.kt` was written to key off window
*size and shape*, not the specific device, for exactly this reason. On this hardware it always
resolves to:

- **`SideBySide`** in landscape — video and library across from each other, the same arrangement
  built for and verified against the Fold 5's inner display.
- **`Stacked`** in portrait, or if a split-screen multitasking window narrows it enough — video
  above the library, sized by the same adjustable height share rather than a fixed box.

Never `Tabletop` or `Book`, since both arrangements exist only for a physical hinge this device does
not have — `Tabletop` for a horizontal one, `Book` for a vertical one (added this pass: `FoldState.kt`
had detected `Posture.Book` since it was first written, but no layout ever consumed it until now).

Everything else in the app — view modes, gestures, Picture-in-Picture, resume position, track
selection, the remote-control client for pairing with a `lumen serve` instance (see
`device/windows-pc`) — is the same shared Kotlin as the Fold 5 fork, not duplicated per fork.
Posture detection keys on window size/shape at runtime, not on which branch built the APK, so
forking the *build identity* (above) does not mean forking this logic three ways — that would just
mean every future bugfix has to be applied three times. What forking buys is an independent place to
land Tab-S9-FE-specific fixes without those changes shipping to every device the moment they're
committed, and an independent release so this fork's build can lag or lead the shared one.

**Worth being honest about:** the arrangement logic was designed and tested against the Fold 5.
This device is a real large flat screen and the *first* one the `SideBySide`/`Stacked` split has
run on outside an assumption written for a folding phone's inner display — it should hold, since
the logic keys on `screenWidthDp`/`screenHeightDp` rather than anything Fold-specific, but it has
not been confirmed on this exact hardware the way the Fold 5's own postures have been.

## Known gaps on this device

Same two as the Fold 5 fork, because it is the same codec/subtitle stack: TrueHD/DTS-HD MA/DTS/
DTS:X do not decode (deferred, disclosed rather than hidden), and PGS subtitle rendering exists but
has an upstream Media3 aspect-ratio bug tracked for 1.8.0 that is not confirmed present or absent
here without a PGS file to test against.

## Install

**https://github.com/CosmicGrub/Media-server-/releases/tag/android-latest-tabs9fe** — open on the
tablet, tap `lumen.apk`, allow installation from the browser. The release refreshes on every push to
*this* branch that touches `android/` or this workflow file — not on pushes to the development
branch or either sibling fork.

Over USB instead:
```bash
adb install -r lumen.apk
```

## How this branch works

This is a real fork: commit here, and the history diverges from the development branch and from the
other two device forks starting at their common ancestor `9cb8b0d`. Nothing rebases or
fast-forwards this branch automatically.

That independence cuts both ways:

- A fix made only here (a Tab-S9-FE-specific layout tweak, a threshold correction for its
  particular screen size) stays local to this fork unless someone deliberately cherry-picks or
  merges it elsewhere. It will not appear on the Fold 5 fork, the Windows fork, or the shared
  development branch on its own.
- Conversely, a fix landed on the development branch (a protocol change, a shared bug fix in
  `ui/` or `remote/`) does not reach this fork automatically either. Pulling those in is a deliberate
  merge (`git merge claude/multiplatform-media-player-server-b7dwe7`), not something that happens by
  default — so this fork can go a while without picking up unrelated upstream churn, at the cost of
  someone having to remember to merge in the fixes that do matter.

In short: propagation between forks is manual and intentional in both directions, not automatic in
either.
