# Lumen for Android — Galaxy Z Fold 5

A sideloadable media player targeting the Z Fold 5's three postures. Kotlin, Compose, Media3,
Gradle KTS.

> **Read this first: the APK in this commit has never been compiled.** It was written in an
> environment where `dl.google.com` and Google's Maven repo are blocked — no Android SDK, no NDK, no
> build-tools, no AndroidX artifacts — so `assembleDebug` could not be run even once. What *was*
> verified is listed under [Verification](#verification). Expect to fix something on the first
> build; the CI workflow exists so the first build happens somewhere with the SDK rather than on
> your machine.

## Getting the APK

**Option 1 — CI builds it for you** (no Android Studio needed):

Push this branch. `.github/workflows/android.yml` builds debug and release APKs on a GitHub runner,
inspects them with `aapt2` and `apksigner`, and uploads both as a run artifact called
`lumen-android-apk`. Download it from the Actions tab, unzip, and install:

```bash
adb install -r app-release.apk
```

Or copy the APK to the phone and open it — Android will prompt to allow installing from that source.

**Option 2 — build locally:**

```bash
cd android
./gradlew assembleDebug            # -> app/build/outputs/apk/debug/app-debug.apk
```

The Gradle wrapper is committed and pinned to 8.11.1. Android Studio Ladybug or newer opens the
`android/` directory directly. A release build needs a debug keystore first:

```bash
keytool -genkeypair -v -keystore app/debug.keystore \
  -storepass android -keypass android -alias androiddebugkey \
  -keyalg RSA -keysize 2048 -validity 10000 \
  -dname "CN=Android Debug,O=Android,C=US"
```

That keystore is deliberately not committed. It is not a secret — it is the same well-known key
every developer machine generates — but a repository is still the wrong place for a signing key of
any kind.

## What makes this a Fold 5 build

A Fold 5 is three devices, and the layout follows the hinge rather than assuming a phone:

| Posture | Geometry | Layout |
| --- | --- | --- |
| Cover screen | 6.2", 2316×904, ~23.1:9 | Video pinned to 16:9 at the top, library fills the rest |
| Inner, flat | 7.6", 2176×1812, ~6:5 | Video and library side by side, 62/38 |
| **Tabletop** | Half-open, horizontal hinge | Video entirely above the crease, controls entirely below |

Tabletop is the posture that justifies the work: the phone stands on a table and plays hands-free.
`FoldState.kt` reads the hinge bounds from `WindowInfoTracker` and **nothing is drawn across the
crease** — on a Fold 5 it is a visible, slightly recessed line, and a control placed on it is hard
to read and unreliable to press.

Three manifest attributes carry most of the weight:

- `resizeableActivity="true"` — without it the system runs the app in a compatibility box on the
  inner display.
- `configChanges` covers `screenSize|smallestScreenSize|screenLayout|orientation|density`. Those are
  exactly what change when the device opens, and handling them keeps **playback running across the
  fold** instead of tearing down the Activity and restarting the video. That is the most visible
  foldable bug there is.
- **No `screenOrientation`.** Pinning an orientation on a device whose natural orientation changes
  when it opens is how apps end up sideways on the inner display.

## Known limitation: codec coverage

This build uses **Media3/ExoPlayer**, which decodes through the device's `MediaCodec` plus Media3's
software fallbacks. That is narrower than the desktop player: no libmpv, no full FFmpeg. Expect
some remuxes — particularly exotic audio (TrueHD, DTS-HD MA) and less common containers — to fail
here while playing fine on the desktop build.

That is a deliberate first step, not an oversight. Media3 is pure Kotlin and builds in one command;
libmpv on Android needs the NDK and a cross-compiled FFmpeg, which is spike **S2** in
`docs/09-roadmap.md`. Getting a working APK on the device first makes S2 measurable — you will know
exactly which of your files need it.

`PlaybackService` and `PlayerViewModel` both set `setEnableDecoderFallback(true)` and
`EXTENSION_RENDERER_MODE_PREFER`, so a stream the hardware decoder refuses falls back to software
rather than failing the file. That is the "no refusal" guarantee (`docs/11` §G2) in its Android form.

When a file does fail, the error card shows mpv-style detail — the Media3 error code and message,
not a friendly substitute. Which codec or container failed is the entire diagnostic value.

## Layout

```
android/
  settings.gradle.kts          repositories, single module
  gradle/libs.versions.toml    version catalogue — one place per dependency
  gradlew, gradle/wrapper/     pinned to Gradle 8.11.1
  app/
    build.gradle.kts           SDK 35, minSdk 24, arm64-v8a only
    proguard-rules.pro         keeps Media3 + window; R8 full mode would strip reflected extractors
    src/main/AndroidManifest.xml
    src/main/kotlin/dev/lumen/player/
      MainActivity.kt          permissions, theme, Compose entry
      LumenApp.kt
      fold/FoldState.kt        Posture from WindowInfoTracker — the foldable core
      library/MediaLibrary.kt  MediaStore query (scoped storage; a directory walk returns nothing)
      player/PlayerViewModel.kt player ownership, library state, error surfacing
      player/PlaybackService.kt MediaSessionService for background + lock screen
      ui/PlayerScreen.kt       the three layouts
```

`arm64-v8a` only: a Fold 5 is a Snapdragon 8 Gen 2, and including x86_64 would roughly double the
APK for an architecture that exists only in emulators. Add it back in `app/build.gradle.kts` if you
want to run this in Android Studio's emulator.

## Verification

What was actually checked, and how:

- **Every Kotlin file parses.** Compiled with kotlinc 2.0.21. Every remaining diagnostic is an
  unresolved `androidx`/`android` symbol — no syntax errors, no broken references between files in
  this project, no redeclarations or argument mismatches.
- **Every XML file parses**, including the manifest. The first draft had comments *between*
  attributes inside the `<activity>` tag, which is malformed XML; that was caught this way.
- **The Gradle wrapper runs.** `./gradlew --version` downloads and executes Gradle 8.11.1.
- **The CI workflow is valid YAML** and its job graph resolves.

What was **not** checked, because it could not be:

- Compilation against the real Android SDK and AndroidX. Unresolved-symbol errors hide type errors,
  so expect API mismatches — particularly in Compose and Media3, where signatures move between
  versions.
- Resource linking (`aapt2`), dexing, packaging, signing.
- Anything on a device: the fold transitions, the tabletop hinge maths, playback, permissions.

The CI workflow closes most of that gap on the first push. The device behaviour only you can check.
