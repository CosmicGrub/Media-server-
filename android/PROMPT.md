# Standalone prompt — build the sideloadable Android APK

Copy everything between the rules into a fresh session. It is self-contained: it assumes no memory
of previous conversations and names the files it needs.

Companion document: `android/MASTERFILE.md` — versions, dependencies, the parity matrix, and the
pitfall list. The prompt tells an agent *what to do*; the masterfile tells it *what is true*.

---

## The prompt

---

You are building a **sideloadable Android APK** for a media player called Lumen, targeting a
**Samsung Galaxy Z Fold 5** (SM-F946B, Snapdragon 8 Gen 2, arm64-v8a, Android 14+). The APK must
install by hand — no Play Store — and must reach feature parity with the existing desktop player.

### Read first

Before writing anything, read these in the repository. They are the specification; do not
reconstruct their contents from assumption.

1. `android/MASTERFILE.md` — **the authority**. Toolchain versions, the full dependency catalogue,
   the PC→Android capability parity matrix (§4), the JNI design (§5), the libmpv path (§6), signing
   and sideloading (§7), Fold 5 specifics (§9), the verification checklist (§10), and a pitfall
   table (§11) where each row has cost someone a day.
2. `crates/lumen-play/README.md` — the desktop player you must match.
3. `android/README.md` — the current Android app and what is already done.
4. `docs/11` — the compatibility charter: guarantees G0 (universal play), G1 (no silent
   degradation), G2 (no refusal), and the six-tier fidelity model.
5. `docs/09-roadmap.md` — spike S2 is the Android NDK work.

### What already exists

A working Stage 1 app: Kotlin, Compose, Media3, three fold-aware layouts, MediaStore library, a
`MediaSessionService` for background playback. It **builds and produces a signed APK** — CI run
30272610131 on 2026-07-27 emitted debug and release variants that passed `aapt2 dump badging` and
`apksigner verify`.

Do not rebuild it from scratch. Extend it.

### The toolchain that is known to work

Do not substitute versions unless something forces you to, and say so if you do:

- JDK **17** (Temurin) — not 21
- Gradle **8.11.1** via the committed wrapper
- Android Gradle Plugin **8.7.3**
- Kotlin **2.0.21** (the Compose compiler ships with Kotlin from 2.0)
- compileSdk / targetSdk **35**, minSdk **24**
- NDK **r27c** and CMake **3.22.1** — only for the Rust core and libmpv
- Android Studio Ladybug or newer, optional; the wrapper builds without it

`dl.google.com` must be reachable for a local build. There is no mirror. If it is blocked, build
via `.github/workflows/android.yml` and collect the `lumen-android-apk` artifact — that is exactly
why the workflow exists.

### Your objective, in priority order

**1. Confirm the current state before changing it.**
Build the existing app and install it. `./gradlew assembleDebug`, then `adb install -r`. If it does
not build, fixing that comes before anything else. Report what you actually observed, not what the
documents claim.

**2. Close the parity gap — the Rust core over JNI (MASTERFILE §5).**
This is the highest-value work and it is build plumbing, not new logic.

The desktop crates `lumen-probe` (content-based container sniffing) and `lumen-match` (filename →
title/year/episode) are pure logic and cross-compile to `aarch64-linux-android` unchanged. Create a
`lumen-jni` crate exposing **one** function — head bytes plus filename in, JSON out — build it with
`cargo ndk`, and call it from Kotlin.

**Do not reimplement the parser in Kotlin.** The desktop parser carries 41 tests and an 83-row
corpus of real release names, and six genuine bugs were found by that corpus alone. A second
implementation cannot inherit those fixes, and the two will disagree about the user's library in
ways nobody notices until the metadata is already wrong.

**3. Real codec parity — libmpv (MASTERFILE §6, spike S2).**
Media3 decodes with the phone's `MediaCodec`; mpv carries its own FFmpeg. That difference is why
TrueHD, DTS-HD MA, PGS subtitles and ordered chapters fail on Android today and work on the PC.

Build FFmpeg and mpv **LGPL-only** — `--disable-gpl --disable-nonfree`, matching
`native/ffmpeg.config`. A GPL FFmpeg makes the whole APK GPL and breaks `ADR-0002`. This constraint
is not negotiable; if you cannot satisfy it, stop and say so rather than shipping a GPL build.

Use `hwdec=mediacodec-copy`, not `mediacodec`: the direct path bypasses the renderer, losing
libplacebo tone mapping and subtitle compositing.

**4. Scale the library (MASTERFILE §12, stage 3).**
A MediaStore query per launch is fine at 200 files and unusable at 5,000. Add a Room index
populated by a `WorkManager` job.

### Constraints — these are not suggestions

- **One broken file must never stop a scan or a playlist.** That is guarantee G2. Fifty corrupt
  files at the front of a thousand-file library must not prevent the other 950 from being reported.
- **Never silently degrade.** G1. If a file plays only because the CPU decoded it, or a track was
  dropped, the report says so. A quiet fallback that looks like success is worse than a failure.
- **Content decides what a file is, not its extension or MIME type.** A `.mkv` whose bytes are not
  Matroska is usually a truncated download, and finding that at scan time beats a mystery failure
  during playback.
- **Emit the same JSON report schema as the desktop `lumen test`.** One analysis script must read
  both. Diverging schemas make cross-platform comparison manual, which means it stops happening.
- **arm64-v8a only** unless you need the emulator. Adding x86_64 roughly doubles the APK for an
  architecture no phone has.
- **Never commit a keystore**, not even the debug one.
- **Never set `android:screenOrientation`.** On a device whose natural orientation changes when it
  opens, pinning one is how apps end up sideways on the inner display.

### Fold 5 requirements

Three postures, three layouts, driven by `WindowInfoTracker`:

| Posture | Geometry | Layout |
| --- | --- | --- |
| Cover screen | 6.2", 2316×904, ~23.1:9 | Video pinned 16:9 at top, library fills the rest |
| Inner, flat | 7.6", 2176×1812, ~6:5 | Video and library side by side |
| Tabletop (half-open, horizontal hinge) | — | Video entirely above the crease, controls entirely below |

Tabletop is the posture that justifies a foldable build. **Never draw across the hinge** — it is a
visible, slightly recessed crease, and a control on it is hard to read and unreliable to press. Use
`FoldingFeature.bounds` to find the region to leave empty.

`configChanges` must cover
`orientation|screenSize|screenLayout|smallestScreenSize|keyboardHidden|keyboard|navigation|uiMode|density`.
Anything missing means the Activity is destroyed on fold and **the video restarts** — the most
visible foldable bug there is.

### How to verify — actually run these

Do not report success from a clean compile. A build that produces a file nobody checked is how a
broken manifest reaches a device.

```bash
# Build
cd android && ./gradlew assembleDebug assembleRelease

# Inspect — a passing build tells you nothing about ABI packaging
BT=$ANDROID_HOME/build-tools/35.0.0
$BT/aapt2 dump badging app/build/outputs/apk/release/app-release.apk \
  | grep -E "package:|targetSdkVersion|native-code"
$BT/apksigner verify --print-certs app/build/outputs/apk/release/app-release.apk

# Install
adb install -r app/build/outputs/apk/release/app-release.apk
adb logcat -s LumenTest:V AndroidRuntime:E
```

`native-code: 'arm64-v8a'` must be present once §5 lands. Missing ABIs are an
`UnsatisfiedLinkError` on the device, not a build failure — the build passes either way.

Then work MASTERFILE §10 in full. The cross-platform check is the one that matters:

```bash
lumen test /path/to/library --json pc.json      # desktop
# run the same library on the phone, export its report
# diff the outcomes; every file that fails on Android but plays on PC is a §5-§6 item
```

### Reporting

State plainly what you built, what you verified, and **what you did not**. If the Rust core is
designed but not compiled, say that. If libmpv is not wired up, say that. An unverified claim in a
build document costs more than an admitted gap, because the gap gets planned around and the false
claim does not.

When you finish, update `android/MASTERFILE.md` §0 and §12 so the next person inherits an accurate
status rather than this one.

---

## Notes on using this prompt

**It is deliberately opinionated.** The constraints section exists because each item is a decision
already made and paid for elsewhere in the project — the LGPL discipline, content-over-extension,
the shared report schema. An agent that relitigates them wastes the work that produced them.

**The read-first list is load-bearing.** Everything after it assumes those documents were read. An
agent that skips them will reimplement the filename parser in Kotlin, which is the single most
expensive wrong turn available here.

**Trim it for a smaller job.** For a UI-only change, keep the read-first list, the Fold 5 section
and the constraints; drop §5, §6 and the parity objectives.
