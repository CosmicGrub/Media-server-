package dev.lumen.player

import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * On-device checks, run headless on an emulator in CI.
 *
 * Deliberately shallow. The value here is not assertions about the UI — it is proving the APK
 * installs, the Activity reaches RESUMED, and nothing throws during startup. A crash on launch is
 * the failure mode that makes every other test irrelevant, and it is invisible to a build that only
 * compiles.
 */
@RunWith(AndroidJUnit4::class)
class LaunchTest {

    /**
     * Media access, granted before anything launches.
     *
     * Without this the first run of these tests failed with "Activity never becomes requested state
     * RESUMED (last transition = PAUSED)" — and that was correct behaviour, not a flake. The app
     * requests the permission from a `LaunchedEffect` on first composition, so the system dialog
     * covers the Activity and holds it at PAUSED until someone answers. Nobody answers on a headless
     * emulator.
     *
     * Granting it up front tests the path a returning user actually takes. The denied path is a
     * different question and cannot be checked with `ActivityScenario`, which itself blocks until
     * RESUMED — the fix that came out of it was to make the denied state recoverable at all.
     */
    @get:Rule
    val permission: GrantPermissionRule =
        GrantPermissionRule.grant(android.Manifest.permission.READ_MEDIA_VIDEO)

    @Test
    fun theActivityReachesResumedWithoutCrashing() {
        // Compose composition, the permission request, `WindowInfoTracker` subscription and the
        // ExoPlayer construction in the ViewModel all happen on the way to RESUMED. Any of them
        // throwing takes the app down on first launch.
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            scenario.moveToState(Lifecycle.State.RESUMED)
            assertEquals(Lifecycle.State.RESUMED, scenario.state)
        }
    }

    @Test
    fun theActivitySurvivesRecreation() {
        // Standing in for the fold. A real fold sends a configuration change the manifest declares
        // it handles, but recreation is what happens when the system reclaims the Activity anyway —
        // and if state cannot survive that, it cannot survive being closed and reopened either.
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            scenario.moveToState(Lifecycle.State.RESUMED)
            scenario.recreate()
            assertEquals(Lifecycle.State.RESUMED, scenario.state)
        }
    }

    @Test
    fun theManifestDeclaresWhatAFoldableNeeds() {
        // Read back from the installed package rather than from the source manifest, so a merge or
        // a build-variant override that drops these is caught. Losing `resizeableActivity` puts the
        // app in a compatibility box on the inner display; losing a `configChanges` entry restarts
        // the video every time the device opens.
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        val pm = ctx.packageManager
        val activity = pm.getActivityInfo(
            android.content.ComponentName(ctx, MainActivity::class.java),
            0,
        )

        val changes = activity.configChanges
        // The constants are ActivityInfo's; each corresponds to a dimension that changes on fold.
        val required = mapOf(
            "screenSize" to android.content.pm.ActivityInfo.CONFIG_SCREEN_SIZE,
            "smallestScreenSize" to android.content.pm.ActivityInfo.CONFIG_SMALLEST_SCREEN_SIZE,
            "screenLayout" to android.content.pm.ActivityInfo.CONFIG_SCREEN_LAYOUT,
            "orientation" to android.content.pm.ActivityInfo.CONFIG_ORIENTATION,
            "density" to android.content.pm.ActivityInfo.CONFIG_DENSITY,
        )
        for ((name, bit) in required) {
            assertTrue(changes and bit != 0, "configChanges is missing $name")
        }

        // No pinned orientation. On a device whose natural orientation changes when it opens,
        // pinning one is how an app ends up sideways on the inner display.
        assertEquals(
            android.content.pm.ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED,
            activity.screenOrientation,
            "screenOrientation must not be pinned on a foldable",
        )
    }
}
