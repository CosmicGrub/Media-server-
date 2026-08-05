package dev.lumen.player.ui

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.media.AudioManager
import android.provider.Settings
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext

/**
 * Screen brightness and media volume, as two 0..1 levels a drag can nudge.
 *
 * Brightness is set on the **window**, not the system. That distinction is the whole reason this is
 * a per-app control rather than a settings shortcut: dimming a film in a dark room should not leave
 * the phone dim afterwards, and `WindowManager.LayoutParams.screenBrightness` reverts the moment the
 * app loses focus. It also needs no permission, where changing the system brightness does.
 */
class ScreenLevels(
    private val activity: Activity?,
    private val audio: AudioManager?,
    initialBrightness: Float,
) {
    private var brightness: Float = initialBrightness

    /** Raise or lower brightness by `delta`, returning the level actually reached. */
    fun nudgeBrightness(delta: Float): Float {
        val next = (brightness + delta).coerceIn(MIN_BRIGHTNESS, 1f)
        brightness = next
        activity?.window?.let { w ->
            w.attributes = w.attributes.apply { screenBrightness = next }
        }
        return next
    }

    /** Raise or lower media volume by `delta`, returning the level actually reached. */
    fun nudgeVolume(delta: Float): Float {
        val am = audio ?: return 0f
        val max = am.getStreamMaxVolume(AudioManager.STREAM_MUSIC)
        if (max <= 0) return 0f
        val current = am.getStreamVolume(AudioManager.STREAM_MUSIC).toFloat() / max
        val next = (current + delta).coerceIn(0f, 1f)
        // No UI flag: the app draws its own indicator, and the system's would fight it.
        am.setStreamVolume(AudioManager.STREAM_MUSIC, (next * max).toInt(), 0)
        return next
    }

    companion object {
        /**
         * Never quite off. A gesture that can take the screen to black leaves no way to see the
         * control that would bring it back.
         */
        const val MIN_BRIGHTNESS = 0.02f
    }
}

@Composable
fun rememberScreenLevels(): ScreenLevels {
    val context = LocalContext.current
    return remember(context) {
        val activity = context.findActivity()
        ScreenLevels(
            activity = activity,
            audio = context.getSystemService(Context.AUDIO_SERVICE) as? AudioManager,
            initialBrightness = activity.currentBrightness(context),
        )
    }
}

/**
 * The window's brightness if it has been set, otherwise the system's.
 *
 * Starting from the system value matters: a window that has never been touched reports
 * `BRIGHTNESS_OVERRIDE_NONE` (-1), and treating that as a level would make the first upward drag
 * jump the screen to nearly black before climbing back.
 */
private fun Activity?.currentBrightness(context: Context): Float {
    val fromWindow = this?.window?.attributes?.screenBrightness ?: -1f
    if (fromWindow >= 0f) return fromWindow
    val system = runCatching {
        Settings.System.getInt(context.contentResolver, Settings.System.SCREEN_BRIGHTNESS) / 255f
    }.getOrDefault(0.5f)
    return system.coerceIn(ScreenLevels.MIN_BRIGHTNESS, 1f)
}

/**
 * Walk out of whatever `ContextWrapper` chain Compose handed us to the Activity underneath.
 *
 * `LocalContext` is not guaranteed to be the Activity — in a themed subtree or inside a dialog it is
 * a wrapper around it — so casting directly is a crash waiting for the first person who opens this
 * from somewhere unusual.
 */
private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
