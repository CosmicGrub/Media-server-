plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "dev.lumen.player"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.lumen.player"
        // 24 covers everything still receiving updates while keeping the modern MediaCodec surface.
        // A Fold 5 runs 34+, so nothing here is constrained by the floor.
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        // arm64 only. Every foldable and every phone worth targeting is 64-bit ARM; adding x86_64
        // would roughly double the APK for architectures that only exist in emulators.
        ndk { abiFilters += listOf("arm64-v8a") }
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

    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.3")
}
