package dev.lumen.player.fold

import android.app.Activity
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.window.layout.FoldingFeature
import androidx.window.layout.WindowInfoTracker
import androidx.window.layout.WindowLayoutInfo
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * How the device is folded right now.
 *
 * This is the whole reason for a foldable-specific build. A Galaxy Z Fold 5 is three devices
 * depending on its hinge, and a layout that ignores that is either cramped on the cover screen or
 * stretched on the inner one:
 *
 *  - **Cover screen** — 6.2", 2316x904, roughly 23.1:9. Extremely tall and narrow. Video occupies a
 *    small band and everything else has to fit around it.
 *  - **Inner screen, flat** — 7.6", 2176x1812, roughly 6:5. Almost square, which no phone layout
 *    expects; a full-bleed 16:9 video leaves large empty regions that should hold the library.
 *  - **Tabletop** — half-open with a horizontal hinge, the device standing on a surface. Video
 *    belongs entirely above the fold and controls entirely below it. This is the posture that
 *    justifies the work: the phone becomes a laptop-shaped media player with no hands involved.
 */
sealed interface Posture {
    /** Flat: one continuous rectangle, whether that is the cover screen or the unfolded inner one. */
    data object Flat : Posture

    /**
     * Half-open with a horizontal hinge — the device is standing like a laptop.
     *
     * [hingeTopPx] and [hingeBottomPx] bound the physical hinge in window coordinates. Content must
     * be placed on one side or the other, never across: on a Fold 5 the hinge region is a visible
     * crease, and a control drawn on it is a control nobody can read or reliably press.
     */
    data class Tabletop(val hingeTopPx: Int, val hingeBottomPx: Int) : Posture

    /**
     * Half-open with a vertical hinge — held like a book. Content splits left and right.
     */
    data class Book(val hingeLeftPx: Int, val hingeRightPx: Int) : Posture
}

/**
 * Derive a [Posture] from the window layout.
 *
 * Kept as a pure function of [WindowLayoutInfo] so it can be tested without a device — the posture
 * logic is the part most likely to be wrong, and it is the part hardest to check by hand on real
 * hardware, where reproducing a half-open angle reliably is genuinely awkward.
 */
fun postureOf(layoutInfo: WindowLayoutInfo): Posture {
    val fold = layoutInfo.displayFeatures.filterIsInstance<FoldingFeature>().firstOrNull()
        ?: return Posture.Flat

    // A FLAT fold is a seamless inner display: there is a hinge, but content may cross it freely.
    // Only HALF_OPENED changes the layout.
    if (fold.state != FoldingFeature.State.HALF_OPENED) return Posture.Flat

    return when (fold.orientation) {
        FoldingFeature.Orientation.HORIZONTAL ->
            Posture.Tabletop(fold.bounds.top, fold.bounds.bottom)
        FoldingFeature.Orientation.VERTICAL ->
            Posture.Book(fold.bounds.left, fold.bounds.right)
        else -> Posture.Flat
    }
}

/**
 * A live [Posture] for the current activity.
 *
 * `WindowInfoTracker` only emits while the activity is started, so this stops costing anything the
 * moment the app is backgrounded.
 */
fun Activity.postureFlow(): Flow<Posture> =
    WindowInfoTracker.getOrCreate(this).windowLayoutInfo(this).map(::postureOf)

@Composable
fun rememberPosture(): State<Posture> {
    val context = LocalContext.current
    val activity = remember(context) { context.findActivity() }
    val flow = remember(activity) { activity?.postureFlow() }
    return flow?.collectAsState(initial = Posture.Flat)
        ?: androidx.compose.runtime.mutableStateOf(Posture.Flat)
}

/**
 * Walk up the context wrappers to the hosting Activity.
 *
 * `LocalContext` inside a Compose hierarchy is frequently a `ContextWrapper` rather than the
 * Activity itself, so a direct cast works until the day some theme or overlay wraps it and then
 * crashes. Returns null rather than throwing: no Activity means no fold information, which is a
 * degraded layout rather than a reason to take the app down.
 */
private fun android.content.Context.findActivity(): Activity? {
    var ctx: android.content.Context? = this
    while (ctx is android.content.ContextWrapper) {
        if (ctx is Activity) return ctx
        ctx = ctx.baseContext
    }
    return null
}
