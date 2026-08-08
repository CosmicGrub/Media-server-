# This branch: Samsung Galaxy Z Fold 5

This is an **independent fork**, not a landing page. It started from the shared development branch
(`claude/multiplatform-media-player-server-b7dwe7`) at commit `9cb8b0d` and now diverges from it on
purpose: this branch's own history is the record of what changed here, and nothing about it is
fast-forwarded from elsewhere automatically. Two sibling forks exist the same way —
`device/galaxy-tab-s9-fe` and `device/windows-pc` — each moving independently from the same starting
point.

## Hardware this targets

| | Cover screen | Inner screen |
|---|---|---|
| Size | 6.2" | 7.6" |
| Resolution | 2316×904 | 2176×1812 |
| Aspect | ~23.1:9 | ~6:5 |

Snapdragon 8 Gen 2 (Galaxy variant). Launched on Android 13 (One UI 5.1.1), upgradable through
whatever One UI version Samsung currently ships for it. The app's `minSdk 24` / `targetSdk 35`
comfortably spans that whole range.

## What is actually specific to this fork

- **`applicationId dev.lumen.player.fold5`** (`android/app/build.gradle.kts`), distinct from the
  shared build's `dev.lumen.player`. This is what makes the difference real rather than cosmetic: a
  Fold 5 can have this build and the shared development build installed side by side, and the two
  are separately signed, separately updated packages as far as Android is concerned. `namespace`
  stays `dev.lumen.player` — that only controls the compile-time `R`/`BuildConfig` package, not
  anything the OS or the user sees, and no Kotlin file in this app references `R.*` directly, so
  there is nothing for changing only the `applicationId` to break.
- **App label** "Lumen (Fold 5)" (`android/app/src/main/res/values/strings.xml`), so the two builds
  are distinguishable in the launcher and app switcher rather than showing as two icons both named
  "Lumen".
- **Its own release channel.** `.github/workflows/android.yml` on this branch publishes to
  `android-latest-fold5` (see [Install](#install)), gated on pushes to `device/galaxy-z-fold-5`
  specifically, instead of sharing the `android-latest` tag the development branch publishes to.
  Pushing here cannot clobber that release, and pushing to the development branch cannot clobber
  this one.

## What the app does because of this hardware (inherited, not forked)

The Fold 5 is the device the adaptive layout in `android/app/src/main/kotlin/dev/lumen/player/ui/`
was designed against:

- **Tabletop posture** (half-open, standing on a surface): video fills the top half above the
  crease, transport controls and the library live below it.
- **Inner display, flat**: video and library side by side (`SideBySide`).
- **Cover screen**: video on top, library filling the rest (`Stacked`), scaled to whatever share of
  the height the user sets.
- **View modes** (Split / Theater / Immersive), gestures (swipe-seek, brightness, volume,
  double-tap), Picture-in-Picture, resume position keyed on file content rather than path, audio and
  subtitle track selection, and the remote-control client for pairing with a `lumen serve` instance
  — see `device/windows-pc` for the other end of that connection.

This layout logic lives in shared Kotlin, not duplicated per fork — posture detection keys on window
size/shape at runtime, not on which branch built the APK. Forking the *build identity* (above) does
not mean forking this logic three ways; that would just mean every future bugfix has to be applied
three times. What forking buys is an independent place to land Fold-5-specific fixes (a wrong crease
offset, a posture threshold that's wrong on this exact screen) without those changes shipping to
every device the moment they're committed, and an independent release so this fork's build can lag
or lead the shared one.

## Known gaps on this device specifically

- **TrueHD, DTS-HD MA, DTS, DTS:X do not decode.** No Android OEM licenses hardware or software
  decode for these; a remux carrying one of them plays picture with no sound on this codec path
  until a decoder extension is built. Deferred rather than silently shipped.
- **PGS subtitle rendering exists** (`androidx.media3.extractor.text.pgs`). There is a tracked
  upstream bug about PGS being stretched to the wrong aspect ratio in Media3 1.8.0. Not confirmed
  present or absent on this device without a PGS-carrying file to test against.

## Install

**https://github.com/CosmicGrub/Media-server-/releases/tag/android-latest-fold5** — open on the
phone, tap `lumen.apk`, allow installation from the browser. The release refreshes on every push to
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

- A fix made only here (a Fold-5-specific posture tweak, a crease-offset correction) stays local to
  this fork unless someone deliberately cherry-picks or merges it elsewhere. It will not appear on
  the Tab S9 FE fork, the Windows fork, or the shared development branch on its own.
- Conversely, a fix landed on the development branch (a protocol change, a shared bug fix in
  `ui/` or `remote/`) does not reach this fork automatically either. Pulling those in is a deliberate
  merge (`git merge claude/multiplatform-media-player-server-b7dwe7`), not something that happens by
  default — so this fork can go a while without picking up unrelated upstream churn, at the cost of
  someone having to remember to merge in the fixes that do matter.

In short: propagation between forks is manual and intentional in both directions, not automatic in
either.
