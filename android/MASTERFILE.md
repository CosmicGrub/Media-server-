# Android Masterfile — sideloadable APK with full desktop parity

Everything needed to build, sign and sideload a modern Android build of Lumen that matches what the
PC version can do. Written to be self-contained: someone with a clean machine and this file should
need nothing else.

**Target device of record:** Samsung Galaxy Z Fold 5 (SM-F946B) — Snapdragon 8 Gen 2, arm64-v8a,
One UI on Android 14+. Everything here works on any modern arm64 Android; the Fold 5 specifics are
called out where they apply.

---

## 0. Status — what is proven and what is not

Honesty about provenance matters more than a tidy document, because the difference decides where you
spend your first day of debugging.

### Verified working

The toolchain in §1 and the dependency set in §3 **produced a signed, installable APK** in CI on
2026-07-27 (`.github/workflows/android.yml`, run 30272610131): debug and release both compiled,
linked resources, dexed, packaged, signed, and passed `aapt2 dump badging` and
`apksigner verify`. Artifact `lumen-android-apk`, 22.9 MB.

If a version bump breaks your build, **this exact combination is the known-good fallback.**

### Designed but NOT built

Everything in §5 (the Rust core over JNI) and §6 (libmpv playback). These are the parity path, and
the architecture is worked out, but no line of it has been compiled. Treat the commands as a
starting point, not as tested recipes.

### The current app's real limitation

Stage 1 (what exists today) plays through **Media3/ExoPlayer**, which decodes via the device's
`MediaCodec` plus Media3's software fallbacks. That is materially narrower than the desktop player.
Expect these to fail on Android and work on PC:

| Failing class | Why |
| --- | --- |
| TrueHD, DTS-HD MA, DTS:X audio | No Android decoder; passthrough needs an AVR over HDMI |
| Some 10-bit HEVC in Matroska | Device-dependent `MediaCodec` profile support |
| VC-1, MPEG-2 | Rarely present in phone decoders |
| PGS/VobSub subtitles | Media3 renders text subs; bitmap subs need libass |
| Ordered chapters, segment linking | Matroska features no Android demuxer implements |

§5–§6 exist to close exactly this gap.

---

## 1. Toolchain — exact versions

| Component | Version | Notes |
| --- | --- | --- |
| **JDK** | **17** (Temurin) | AGP 8.7 requires 17. Not 21 — Gradle's toolchain resolution gets confusing when the JDK on PATH differs from the target. |
| **Gradle** | **8.11.1** | Pinned in `gradle/wrapper/gradle-wrapper.properties`. Never rely on a system Gradle. |
| **Android Gradle Plugin** | **8.7.3** | The pairing constraint that matters: AGP 8.7 needs Gradle ≥ 8.9 and JDK 17. |
| **Kotlin** | **2.0.21** | From 2.0 the Compose compiler ships with Kotlin (`org.jetbrains.kotlin.plugin.compose`), so there is no separate Compose-compiler version to drift. |
| **compileSdk / targetSdk** | **35** | Android 15. Google Play requires targeting within one year of the latest release; sideloading does not, but targeting current avoids compatibility shims. |
| **minSdk** | **24** | Android 7.0. Covers everything still receiving updates. A Fold 5 runs 34+, so nothing here is constrained by the floor. |
| **NDK** (§5–6 only) | **r27c** (27.2.12479018) | Needed only for the Rust core and libmpv. Not required for the current app. |
| **CMake** (§5–6 only) | **3.22.1** | The version `sdkmanager` ships; matching it avoids a second toolchain download. |
| **Android Studio** | Ladybug (2024.2.1) or newer | Optional — the wrapper builds without it. Open the `android/` directory, not the repo root. |

### Installing without Android Studio

Android Studio is a convenience, not a requirement. The command-line tools are enough and are what
CI uses.

```bash
# 1. JDK 17
sudo apt install -y openjdk-17-jdk          # or: brew install temurin@17
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64

# 2. Android command-line tools
export ANDROID_HOME="$HOME/Android/Sdk"
mkdir -p "$ANDROID_HOME/cmdline-tools"
cd "$ANDROID_HOME/cmdline-tools"
curl -O https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip -q commandlinetools-linux-11076708_latest.zip
mv cmdline-tools latest          # the SDK expects this exact directory name
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"

# 3. SDK components
yes | sdkmanager --licenses
sdkmanager \
  "platform-tools" \
  "platforms;android-35" \
  "build-tools;35.0.0"

# 4. Only for §5-§6 (Rust core / libmpv)
sdkmanager "ndk;27.2.12479018" "cmake;3.22.1"
```

**`dl.google.com` must be reachable.** It is blocked on many corporate and sandboxed networks, and
there is no mirror — the SDK, NDK, build-tools and the Android Gradle Plugin all come from Google's
hosts. If you cannot reach it, build in CI (§8) instead; that is precisely why the workflow exists.

Persist the environment:

```bash
cat >> ~/.bashrc <<'EOF'
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64
export ANDROID_HOME="$HOME/Android/Sdk"
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"
EOF
```

`local.properties` in `android/` (git-ignored, machine-specific):

```properties
sdk.dir=/home/you/Android/Sdk
```

---

## 2. Project layout

```
android/
  settings.gradle.kts          repositories; FAIL_ON_PROJECT_REPOS so a module cannot add its own
  build.gradle.kts             plugin declarations only
  gradle.properties            JVM heap, AndroidX, R8 full mode, config cache
  gradle/libs.versions.toml    version catalogue — one place per dependency
  gradlew, gradlew.bat         wrapper, pinned to 8.11.1 — commit these
  gradle/wrapper/
    gradle-wrapper.jar         yes, commit it; the wrapper cannot bootstrap without it
    gradle-wrapper.properties
  app/
    build.gradle.kts
    proguard-rules.pro
    debug.keystore             NOT committed — generated (§7)
    src/main/
      AndroidManifest.xml
      kotlin/dev/lumen/player/
        MainActivity.kt        permissions, theme, Compose entry
        LumenApp.kt
        fold/FoldState.kt      Posture from WindowInfoTracker
        library/MediaLibrary.kt MediaStore query
        player/PlayerViewModel.kt
        player/PlaybackService.kt MediaSessionService
        ui/PlayerScreen.kt     the three fold layouts
      res/
        values/{strings,themes,colors}.xml
        drawable/ic_launcher_foreground.xml
        mipmap-anydpi-v26/ic_launcher.xml
    # §5-§6 additions
    src/main/jniLibs/arm64-v8a/    libmpv.so, libavcodec.so, ... (prebuilt or CI-built)
    src/main/cpp/                  JNI glue, CMakeLists.txt
```

---

## 3. Dependencies — the full set

`gradle/libs.versions.toml`. A version catalogue rather than inline strings so a typo is a build
configuration error rather than an unresolvable coordinate discovered five minutes into a build.

```toml
[versions]
agp = "8.7.3"
kotlin = "2.0.21"
composeBom = "2024.12.01"
coreKtx = "1.15.0"
lifecycle = "2.8.7"
activityCompose = "1.9.3"
media3 = "1.5.0"
window = "1.3.0"
adaptive = "1.0.0"
documentfile = "1.0.1"
coroutines = "1.9.0"
serialization = "1.7.3"
datastore = "1.1.1"
room = "2.6.1"
work = "2.10.0"

[libraries]
# --- core ---
core-ktx                   = { module = "androidx.core:core-ktx", version.ref = "coreKtx" }
lifecycle-runtime-ktx      = { module = "androidx.lifecycle:lifecycle-runtime-ktx", version.ref = "lifecycle" }
lifecycle-viewmodel-compose= { module = "androidx.lifecycle:lifecycle-viewmodel-compose", version.ref = "lifecycle" }
lifecycle-runtime-compose  = { module = "androidx.lifecycle:lifecycle-runtime-compose", version.ref = "lifecycle" }
activity-compose           = { module = "androidx.activity:activity-compose", version.ref = "activityCompose" }
coroutines-android         = { module = "org.jetbrains.kotlinx:kotlinx-coroutines-android", version.ref = "coroutines" }

# --- UI ---
compose-bom                = { module = "androidx.compose:compose-bom", version.ref = "composeBom" }
compose-ui                 = { module = "androidx.compose.ui:ui" }
compose-ui-graphics        = { module = "androidx.compose.ui:ui-graphics" }
compose-ui-tooling         = { module = "androidx.compose.ui:ui-tooling" }
compose-ui-tooling-preview = { module = "androidx.compose.ui:ui-tooling-preview" }
compose-material3          = { module = "androidx.compose.material3:material3" }
compose-material-icons     = { module = "androidx.compose.material:material-icons-extended" }

# --- foldable: the reason this is a Fold build and not a phone build ---
window                     = { module = "androidx.window:window", version.ref = "window" }
adaptive                   = { module = "androidx.compose.material3.adaptive:adaptive", version.ref = "adaptive" }

# --- playback ---
media3-exoplayer           = { module = "androidx.media3:media3-exoplayer", version.ref = "media3" }
media3-ui                  = { module = "androidx.media3:media3-ui", version.ref = "media3" }
media3-session             = { module = "androidx.media3:media3-session", version.ref = "media3" }
media3-common              = { module = "androidx.media3:media3-common", version.ref = "media3" }
# Widens container coverage considerably; see §6 for why it still is not parity.
media3-datasource          = { module = "androidx.media3:media3-datasource", version.ref = "media3" }
media3-extractor           = { module = "androidx.media3:media3-extractor", version.ref = "media3" }

# --- library + persistence ---
documentfile               = { module = "androidx.documentfile:documentfile", version.ref = "documentfile" }
datastore-preferences      = { module = "androidx.datastore:datastore-preferences", version.ref = "datastore" }
room-runtime               = { module = "androidx.room:room-runtime", version.ref = "room" }
room-ktx                   = { module = "androidx.room:room-ktx", version.ref = "room" }
room-compiler              = { module = "androidx.room:room-compiler", version.ref = "room" }
work-runtime               = { module = "androidx.work:work-runtime-ktx", version.ref = "work" }
serialization-json         = { module = "org.jetbrains.kotlinx:kotlinx-serialization-json", version.ref = "serialization" }

[plugins]
android-application = { id = "com.android.application", version.ref = "agp" }
kotlin-android      = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
kotlin-compose      = { id = "org.jetbrains.kotlin.plugin.compose", version.ref = "kotlin" }
kotlin-serialization= { id = "org.jetbrains.kotlin.plugin.serialization", version.ref = "kotlin" }
room                = { id = "androidx.room", version.ref = "room" }
```

### Why each non-obvious one is here

- **`androidx.window`** — the only way to know the device is half-open. Without it tabletop mode
  cannot exist, and tabletop mode is the entire argument for a foldable-specific build.
- **`media3-session`** — the media notification and lock-screen controls. On a foldable it matters
  more than on a normal phone: closing a Fold 5 moves the app between displays, and without a
  session the transition can tear playback down.
- **`room` + `work`** — the library index and the background rescan. A MediaStore query on every
  launch is fine for 200 files and unusable at 5,000; the desktop scanner's output belongs in a
  local database populated by a `WorkManager` job.
- **`datastore`** — resume positions and settings. Not `SharedPreferences`: it has no async API and
  blocks the main thread on first read, which on a large preferences file is a visible stall.
- **`kotlinx-serialization`** — emits the **same JSON report schema** as the desktop `lumen test`.
  Same schema is the point: one analysis script should read both.

---

## 4. Capability parity — PC to Android

The desktop player's behaviour, and what the equivalent is here. This table is the specification;
anything marked ⚠ is where an Android build silently differs unless deliberately handled.

| # | PC capability | Desktop implementation | Android equivalent | Status |
| --- | --- | --- | --- | --- |
| 1 | Content-based container detection | `lumen_probe::sniff` on head bytes | Same Rust over JNI (§5); read head via `ContentResolver.openInputStream` | ⚠ must not fall back to MIME type |
| 2 | Filename → title/year/episode | `lumen_match::parse` | Same Rust over JNI (§5) | ⚠ a Kotlin reimplementation *will* drift |
| 3 | Recursive library scan | `std::fs` walk | `MediaStore` + SAF tree URI | ⚠ scoped storage forbids path walks |
| 4 | Season/episode playlist order | `playlist_order` | Same Rust over JNI | — |
| 5 | Playback | mpv, `--vo=gpu-next` | libmpv via NDK (§6); Media3 today | ⚠ the real parity gap |
| 6 | Hardware decode | `--hwdec=auto-safe` | `MediaCodec`; enumerate `MediaCodecList` | — |
| 7 | HDR tone mapping | libplacebo | `Display.HdrCapabilities` + libplacebo in mpv | ⚠ Media3 does not tone-map |
| 8 | Bitstream audio passthrough | IEC 61937 | `AudioTrack` + `AudioManager.getDevices()` | ⚠ USB-C/HDMI only; no speaker passthrough |
| 9 | Bitmap subtitles (PGS/VobSub) | libass | libass inside the mpv build | ⚠ Media3 cannot render these |
| 10 | Sidecar subtitle discovery | `--sub-auto=fuzzy` | Query sibling files via SAF | ⚠ MediaStore does not index `.srt` |
| 11 | Per-file outcome + reason | mpv `end-file` reason | `Player.Listener.onPlayerError` / mpv events | — |
| 12 | Software-decode detection | `hwdec-current` | `MediaCodecInfo.isHardwareAccelerated` (API 29+) | — |
| 13 | Seekability check | `seekable` property | `Player.isCurrentMediaItemSeekable` | — |
| 14 | Extension/content mismatch | sniff vs extension | Same, via §5 | — |
| 15 | JSON report | hand-rolled writer | `kotlinx-serialization`, **same schema** | — |
| 16 | Exit codes | process exit | Report file + logcat tag `LumenTest` | ⚠ no exit code on Android |
| 17 | Fold-aware layout | n/a | `WindowInfoTracker` (§9) | Android-only |

### The two that decide whether this is parity or a lookalike

**#2 and #5.** Everything else is mechanical.

Reimplementing the filename parser in Kotlin guarantees divergence: the desktop parser is 41 tests
and a 83-row corpus of real release names, and six genuine bugs were found in it by that corpus
alone. A second implementation will not reproduce those fixes, and the two will disagree about your
library in ways nobody notices until the metadata is wrong on one device.

The answer is to **compile the existing Rust to Android and call it over JNI** — §5.

---

## 5. The Rust core on Android (JNI)

> **Designed, not built.** No part of this section has been compiled. It is the architecturally
> correct path to parity and the commands are the right shape, but expect to debug them.

The workspace crates that should run unchanged on Android:

- `lumen-model` — containers, codecs, colour/HDR types
- `lumen-probe` — content sniffing, EBML/ISOBMFF structure, recovery ladder
- `lumen-match` — filename parsing, title matching
- `lumen-playback` — the playback decision ladder
- `lumen-caps` — capability model

All are `no_std`-friendly pure logic with no OS dependencies beyond `std`, which is exactly what
makes them portable.

### Rust targets

```bash
rustup target add aarch64-linux-android    # every modern phone, incl. Fold 5
rustup target add armv7-linux-androideabi  # optional, only for pre-2017 devices
rustup target add x86_64-linux-android     # optional, for the emulator
cargo install cargo-ndk
```

### A JNI crate

`crates/lumen-jni/Cargo.toml`:

```toml
[package]
name = "lumen-jni"
version.workspace = true
edition.workspace = true

[lib]
# cdylib is what produces a .so the JVM can load. rlib as well so tests still run on the host.
crate-type = ["cdylib", "rlib"]

[dependencies]
lumen-model.workspace = true
lumen-probe.workspace = true
lumen-match.workspace = true
jni = "0.21"
```

The boundary should be **one function taking bytes and a name, returning JSON**. A chatty JNI
surface means dozens of signatures to keep in sync across two languages; one function means one.

```rust
// crates/lumen-jni/src/lib.rs
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::jstring;
use jni::JNIEnv;

/// Identify a file from its head bytes and its name. Returns JSON.
///
/// The name is mangled to match `dev.lumen.player.core.LumenCore.identify`. Getting it wrong is an
/// `UnsatisfiedLinkError` at call time rather than a build error, so it is worth checking with
/// `javap -s` against the compiled class rather than by eye.
#[no_mangle]
pub extern "system" fn Java_dev_lumen_player_core_LumenCore_identify<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    head: JByteArray<'l>,
    name: JString<'l>,
) -> jstring {
    let bytes = env.convert_byte_array(&head).unwrap_or_default();
    let name: String = env.get_string(&name).map(Into::into).unwrap_or_default();

    let candidates = lumen_probe::sniff(&bytes);
    let parsed = lumen_match::parse(&name);
    let json = serde_json_like_render(&candidates, &parsed);

    env.new_string(json).expect("json is valid UTF-8").into_raw()
}
```

### Building the `.so`

```bash
cargo ndk \
  -t arm64-v8a \
  -o android/app/src/main/jniLibs \
  build --release -p lumen-jni
```

That writes `android/app/src/main/jniLibs/arm64-v8a/liblumen_jni.so`, which AGP packages
automatically — no Gradle changes needed.

### Kotlin side

```kotlin
package dev.lumen.player.core

object LumenCore {
    init { System.loadLibrary("lumen_jni") }

    /** Head bytes plus filename in, JSON out. One function, one thing to keep in sync. */
    external fun identify(head: ByteArray, name: String): String
}
```

Read the head bytes through the resolver, because a path may not exist under scoped storage:

```kotlin
val head = context.contentResolver.openInputStream(uri)?.use { stream ->
    ByteArray(4096).let { buf ->
        val n = stream.read(buf)
        if (n <= 0) ByteArray(0) else buf.copyOf(n)
    }
} ?: ByteArray(0)
val json = LumenCore.identify(head, displayName)
```

**4096 bytes** is the same window the desktop scanner uses — enough for every signature in
`lumen_probe::magic` plus the `ftyp` brand list.

---

## 6. libmpv for real codec parity

> **Designed, not built.** This is spike **S2** in `docs/09-roadmap.md`.

Media3 will never match the desktop player because it decodes with the phone's `MediaCodec`. mpv
carries its own FFmpeg, so the same file plays the same way everywhere. That is the difference
between "an Android player" and "the same player, on Android".

### Options, honestly ranked

1. **`libmpv-android` prebuilt** — build scripts exist upstream
   (`github.com/mpv-android/mpv-android`) producing `libmpv.so` plus FFmpeg. Fastest route to a
   working build. **Check the licence:** an FFmpeg built with `--enable-gpl` makes the whole APK
   GPL, which conflicts with the App Store constraint in `ADR-0002`. Build LGPL-only —
   `--disable-gpl --disable-nonfree` — matching `native/ffmpeg.config`.
2. **Build FFmpeg + mpv from source with the NDK** — full control over the LGPL configuration and
   which decoders are present. Slow (hours), and the correct answer for a shipping product.
3. **Media3 + its FFmpeg extension** — narrows the audio gap only (adds DTS, AC-3, and more), still
   no libass, no ordered chapters, no libplacebo tone mapping. A reasonable intermediate step.

### Wiring mpv into the app

mpv on Android renders through its own `SurfaceView`. The Compose side becomes an `AndroidView`
wrapping that surface, and the fold layouts in §9 are unchanged — they position a surface either
way, so the posture work is not wasted when playback is swapped underneath.

```kotlin
// Sketch. mpv is driven by property strings, exactly as on the desktop.
MPVLib.create(context)
MPVLib.setOptionString("vo", "gpu-next")     // the same renderer as the PC build
MPVLib.setOptionString("hwdec", "mediacodec-copy")
MPVLib.setOptionString("gpu-context", "android")
MPVLib.setOptionString("sub-auto", "fuzzy")
MPVLib.init()
```

**`hwdec=mediacodec-copy` rather than `mediacodec`:** the direct path hands frames to the display
without letting the renderer touch them, which means no libplacebo tone mapping and no subtitle
compositing. The copy costs bandwidth and buys back everything that makes the desktop picture look
right.

### Audio passthrough

The Fold 5 has no HDMI, so passthrough only applies over USB-C to a DAC or dock. Query first —
never assume:

```kotlin
val am = context.getSystemService(AudioManager::class.java)
val devices = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
val supportsAc3 = devices.any { dev ->
    dev.encodings.any { it == AudioFormat.ENCODING_AC3 || it == AudioFormat.ENCODING_E_AC3 }
}
```

This is the Android form of the sink-level capability probing in `docs/11` — ask the sink, do not
consult a static profile table.

---

## 7. Signing and sideloading

### Debug key (testing)

Not a secret — it is the same well-known key every developer machine generates — but keep it out of
git anyway.

```bash
cd android
keytool -genkeypair -v \
  -keystore app/debug.keystore \
  -storepass android -keypass android \
  -alias androiddebugkey \
  -keyalg RSA -keysize 2048 -validity 10000 \
  -dname "CN=Android Debug,O=Android,C=US"
```

### Release key (anything you distribute)

```bash
keytool -genkeypair -v \
  -keystore ~/keys/lumen-release.jks \
  -storepass "$KS_PASS" -keypass "$KEY_PASS" \
  -alias lumen -keyalg RSA -keysize 4096 -validity 10000 \
  -dname "CN=Lumen,O=Lumen,C=GB"
```

Credentials go in `~/.gradle/gradle.properties` (outside the repo) or CI secrets — never in
`build.gradle.kts`:

```properties
LUMEN_STORE_FILE=/home/you/keys/lumen-release.jks
LUMEN_STORE_PASSWORD=...
LUMEN_KEY_ALIAS=lumen
LUMEN_KEY_PASSWORD=...
```

**Losing a release key means the app can never be updated in place** — a new key is a different app
as far as Android is concerned. Back it up somewhere you would not lose.

### Building

```bash
cd android
./gradlew assembleDebug      # app/build/outputs/apk/debug/app-debug.apk
./gradlew assembleRelease    # app/build/outputs/apk/release/app-release.apk
```

### Installing on the Fold 5

```bash
# Settings > About phone > Software information > tap "Build number" 7x
# then Settings > Developer options > USB debugging
adb devices                       # confirm the device, accept the prompt on the phone
adb install -r app-release.apk    # -r replaces an existing install
```

Wireless, which is easier with a folding device on a desk:

```bash
adb pair <ip>:<port>              # Developer options > Wireless debugging > Pair with code
adb connect <ip>:<port>
adb install -r app-release.apk
```

Without a computer: copy the APK to the phone, open it in Files, and allow installing from that
source when prompted.

### Getting a CI build onto the phone

Every push that touches `android/` publishes the APK to a rolling prerelease:

**<https://github.com/CosmicGrub/Media-server-/releases/tag/android-latest>**

Open that on the phone, tap `lumen.apk`, allow installation from the browser when Android asks, and
that is the whole procedure — no PC, no cable. The link never changes; the asset behind it is
replaced on each build, and the release title carries the commit it came from.

The reason this exists rather than pointing at the run's Artifacts section: **an Actions artifact is
a zip, and Android cannot install a zip.** Getting the app onto a device from an artifact means a
PC, a download, an unzip, and a cable. A release asset is the `.apk` itself.

The repository is private, so the phone's browser has to be signed in to GitHub for the download to
start. That is a one-time login, not a per-build step.

From a PC instead, if the cable is already there:

```bash
gh release download android-latest --repo CosmicGrub/Media-server- --pattern lumen.apk
adb install -r lumen.apk
```

### Verifying what you built

```bash
BT=$ANDROID_HOME/build-tools/35.0.0
$BT/aapt2 dump badging app-release.apk | grep -E "package:|sdkVersion|targetSdkVersion|native-code"
$BT/apksigner verify --print-certs --verbose app-release.apk
```

Check `native-code: 'arm64-v8a'` — a missing ABI means the `.so` files did not get packaged, which
manifests on the device as `UnsatisfiedLinkError` at first use rather than as a build failure.

---

## 8. Building in CI

`.github/workflows/android.yml` in this repo. Push and collect `lumen-android-apk` from the run's
Artifacts.

**This is the fallback when `dl.google.com` is unreachable** — a locked-down network cannot fetch
the SDK, and a GitHub runner already has it. It is how the current APK was produced.

The workflow generates the debug keystore rather than committing one, builds both variants, and
inspects the output with `aapt2` and `apksigner` before uploading. A build that produces a file
nobody checked is how a broken manifest reaches a device.

---

## 9. Fold 5 specifics

### Geometry

| Surface | Size | Resolution | Ratio |
| --- | --- | --- | --- |
| Cover | 6.2" | 2316 × 904 | ~23.1 : 9 |
| Inner | 7.6" | 2176 × 1812 | ~6 : 5 |

Both are unusual. A layout tuned for a 20:9 phone is cramped on one and stretched on the other.

### The manifest attributes that matter

```xml
android:resizeableActivity="true"
android:configChanges="orientation|screenSize|screenLayout|smallestScreenSize|keyboardHidden|keyboard|navigation|uiMode|density"
```

- Without `resizeableActivity`, the system runs the app in a compatibility box on the inner display.
- `configChanges` lists exactly what changes when the device opens. Handling them keeps **playback
  running across the fold** instead of tearing down the Activity and restarting the video — the most
  visible foldable bug there is.
- **Never set `screenOrientation`.** Pinning an orientation on a device whose natural orientation
  changes when it opens is how apps end up sideways on the inner display.

### Postures

```kotlin
WindowInfoTracker.getOrCreate(activity).windowLayoutInfo(activity)
    .map { it.displayFeatures.filterIsInstance<FoldingFeature>().firstOrNull() }
```

- `HALF_OPENED` + `HORIZONTAL` → **tabletop**: video above the crease, controls below. The posture
  that justifies a foldable build — the phone stands on a table and plays hands-free.
- `HALF_OPENED` + `VERTICAL` → **book**: split left/right.
- `FLAT` → one continuous rectangle; branch on width instead.

**Never draw across the hinge.** On a Fold 5 it is a visible, slightly recessed crease, and a
control placed on it is hard to read and unreliable to press. `FoldingFeature.bounds` gives the
region to leave empty.

### Other One UI behaviours worth knowing

- **Multi-window** is used heavily on this device. `resizeableActivity` covers it; test at a third
  of the screen, where a two-pane layout must collapse.
- **Flex mode panel** — One UI may offer its own controls in the lower half in tabletop mode. An app
  that handles the posture itself should take the whole window and draw its own.
- **App Continuity** — closing the device moves the app to the cover screen. With `configChanges`
  handled this is seamless; without it, playback restarts.

---

## 10. Verification checklist

### Build
- [ ] `./gradlew assembleDebug` succeeds from clean
- [ ] `./gradlew assembleRelease` succeeds (R8 enabled — this is where missing ProGuard rules surface)
- [ ] `aapt2 dump badging` shows `targetSdkVersion:'35'` and `native-code:'arm64-v8a'`
- [ ] `apksigner verify` passes with v2/v3 schemes present

### Device — fold behaviour
- [ ] Launch on the cover screen; video is visible and the library is usable
- [ ] Open the device mid-playback — **video must not restart**
- [ ] Close it mid-playback — same
- [ ] Half-open with a horizontal hinge — video sits above the crease, controls below, nothing on it
- [ ] Multi-window at one third width — layout collapses rather than clipping
- [ ] Rotate on the inner display — no letterboxing

### Device — playback parity
- [ ] Compare against the PC report: `lumen test <library> --json pc.json`
- [ ] Run the same library on the phone; diff the outcomes
- [ ] Every file that fails on Android but plays on PC is a §5–§6 item — record which and why
- [ ] Confirm hardware decoding engaged (`MediaCodecInfo.isHardwareAccelerated`)
- [ ] 4K HEVC plays without dropping frames
- [ ] HDR content is tone-mapped rather than washed out

### Regression
- [ ] Report JSON parses with the same script as the desktop report
- [ ] The parser agrees with the desktop on the same filenames (§5 makes this automatic; a Kotlin
      reimplementation makes it a permanent test burden)

---

## 11. Pitfalls, each of which has cost someone a day

| Symptom | Cause |
| --- | --- |
| `UnsatisfiedLinkError` at first native call | The `.so` was not packaged. Check `native-code:` in badging output; the build succeeded regardless. |
| Library empty, no error | Requested `READ_EXTERNAL_STORAGE` on API 33+. It is `READ_MEDIA_VIDEO` now; the wrong one denies silently. |
| Works in debug, container fails in release | R8 stripped a Media3 extractor it could not see referenced. Keep rules in `proguard-rules.pro`. |
| Video restarts when the device is opened | `configChanges` is missing a dimension. All of `screenSize\|smallestScreenSize\|screenLayout\|density` are needed. |
| App is letterboxed on the inner display | `resizeableActivity` is false, or a `screenOrientation` is pinned. |
| Controls unreadable in tabletop mode | Content drawn across the hinge. Use `FoldingFeature.bounds`. |
| `Could not resolve com.android.application` | `dl.google.com` unreachable. There is no mirror — use CI (§8). |
| OOM part-way through a release build | Gradle daemon heap. `org.gradle.jvmargs=-Xmx3g` in `gradle.properties`. |
| Emulator will not install the APK | `abiFilters` is arm64-only. Add `x86_64` for emulator work. |
| Playback dies when the app is backgrounded | No `MediaSessionService`, or the foreground-service type is missing from the manifest. |

---

## 12. Roadmap

| Stage | Deliverable | Status |
| --- | --- | --- |
| **1** | Media3 player, fold-aware layouts, MediaStore library | **Built — APK produced in CI 2026-07-27** |
| **2** | Rust core over JNI (§5): identical sniffing and parsing | Designed |
| **3** | Room index + WorkManager rescan; scales past a few thousand files | Not started |
| **4** | libmpv playback (§6): real codec parity | Spike S2 |
| **5** | Audio passthrough over USB-C; sink capability probing | Blocked on 4 |
| **6** | Server pairing, streaming, and the transcode ladder from `docs/13` | Blocked on the server |

Stage 2 is the highest value per hour: it removes an entire class of divergence permanently, and it
is pure build plumbing rather than new logic.

---

## References

- `docs/09-roadmap.md` — spike S2 (Android NDK)
- `docs/11` — the compatibility charter; G0/G1/G2 and the tier model
- `docs/13` — remux and transcode matrices
- `ADR-0001` — desktop shell choice
- `ADR-0002` — LGPL-only build discipline; why an `--enable-gpl` FFmpeg is not an option
- `crates/lumen-play/README.md` — the desktop player this must match
- `android/README.md` — the current app
