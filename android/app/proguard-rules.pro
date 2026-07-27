# Media3 reflects over renderer and extractor classes when selecting a decoder, so R8 full mode will
# strip ones it cannot see referenced. A stripped extractor is a container that stops opening in
# release builds while working perfectly in debug — the worst kind of bug to chase.
-keep class androidx.media3.** { *; }
-dontwarn androidx.media3.**

# WindowManager's fold APIs are resolved through the platform extension at runtime.
-keep class androidx.window.** { *; }
-dontwarn androidx.window.**
