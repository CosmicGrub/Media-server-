plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "dev.lumen.player"
    compileSdk = 35

    defaultConfig {
        // Distinct from the shared-development build (`dev.lumen.player`) on purpose: this is the
        // `device/galaxy-tab-s9-fe` fork, and a divergent applicationId is what lets it install
        // side-by-side with the mainline build and the other device forks on the same tablet,
        // rather than colliding over one package slot.
        applicationId = "dev.lumen.player.tabs9fe"
        // 24 covers everything still receiving updates while keeping the modern MediaCodec surface.
        // A Fold 5 runs 34+, so nothing here is constrained by the floor.
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        // Required for the on-device tests in src/androidTest.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        // arm64 only for anything shipped. Every foldable and every phone worth targeting is 64-bit
        // ARM, and adding x86_64 would roughly double the APK for an architecture that exists only
        // in emulators.
        //
        // The on-device CI job passes `-PemulatorAbi=x86_64` because CI runners have no arm64
        // emulator image. That widens this build only; the released APK stays arm64-only. Harmless
        // today because the app carries no native libraries and an APK without them installs on any
        // ABI — but it stops being harmless the moment the Rust core lands, and a silent
        // INSTALL_FAILED_NO_MATCHING_ABIS months from now is a bad way to discover that.
        ndk {
            val emulatorAbi = (project.findProperty("emulatorAbi") as String?)?.takeIf(String::isNotBlank)
            abiFilters += listOfNotNull("arm64-v8a", emulatorAbi)
        }
    }

    signingConfigs {
        // A debug key is deliberate for a sideload build: it needs no secret to produce, and the
        // APK is for testing rather than distribution. A release key belongs in CI secrets, not here.
        getByName("debug") {
            storeFile = file("debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Signed with the debug key so `assembleRelease` produces something installable without
            // a keystore. Swap in a real signing config before this is ever distributed.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
        // Desugaring so java.time and the newer stream APIs work on the API 24 floor.
        isCoreLibraryDesugaringEnabled = true
    }

    kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }

    buildFeatures { compose = true }

    packaging {
        resources.excludes += setOf("/META-INF/{AL2.0,LGPL2.1}", "/META-INF/DEPENDENCIES")
    }

    sourceSets["main"].kotlin.srcDirs("src/main/kotlin")
    sourceSets["test"].kotlin.srcDirs("src/test/kotlin")
    sourceSets["androidTest"].kotlin.srcDirs("src/androidTest/kotlin")

    testOptions {
        unitTests {
            // The unit tests here deliberately touch no Android APIs — the posture decision and the
            // formatting helpers are free functions over primitives. This is a safety net for any
            // that slip through, so a stray framework call returns a default instead of throwing
            // "not mocked" and sending someone hunting for a bug that is not there.
            isReturnDefaultValues = true
        }
    }
}

dependencies {
    implementation(libs.core.ktx)
    implementation(libs.lifecycle.runtime.ktx)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.activity.compose)
    implementation(libs.coroutines.android)
    implementation(libs.documentfile)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.media3.exoplayer)
    implementation(libs.media3.ui)
    implementation(libs.media3.session)
    implementation(libs.media3.common)

    // The foldable APIs. Without this there is no way to know the device is half-open, and tabletop
    // mode — the one layout that justifies a foldable build — cannot exist.
    implementation(libs.window)
    implementation(libs.adaptive)

    // Encrypted-at-rest storage for the saved remote-control pairing token.
    implementation(libs.security.crypto)

    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.3")

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)

    androidTestImplementation(libs.junit)
    androidTestImplementation(libs.kotlin.test.junit)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.junit)
    androidTestImplementation(libs.androidx.test.rules)
}
