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

/** Whether the hinge is flat or bent, independent of the androidx type. */
enum class HingeState { FLAT, HALF_OPENED }

/** Which way the hinge runs, independent of the androidx type. */
enum class HingeOrientation { HORIZONTAL, VERTICAL }

/**
 * The posture decision itself, over plain values.
 *
 * Deliberately free of Android and androidx types so it runs as an ordinary JVM unit test — no
 * emulator, no device. This is the part most likely to be wrong and the part hardest to check by
 * hand, because holding a real device at a reliable half-open angle while reading logs is genuinely
 * awkward. Everything Android-specific lives in the adapter below.
 */
fun posture(
    state: HingeState,
    orientation: HingeOrientation,
    hingeTop: Int,
    hingeBottom: Int,
    hingeLeft: Int,
    hingeRight: Int,
): Posture {
    // A FLAT fold is a seamless inner display: there is a hinge, but content may cross it freely.
    // Only HALF_OPENED changes the layout.
    if (state != HingeState.HALF_OPENED) return Posture.Flat
    return when (orientation) {
        HingeOrientation.HORIZONTAL -> Posture.Tabletop(hingeTop, hingeBottom)
        HingeOrientation.VERTICAL -> Posture.Book(hingeLeft, hingeRight)
    }
}

/**
 * Which [FoldingFeature] to read when a window reports more than one.
 *
 * Real hardware reports at most one hinge; the API shape allows more, and silently taking whichever
 * happened to be first is how a device that someday reports two would get an arbitrary, possibly-
 * flat one instead of the one that actually matters. Kept as a pure function over plain values, the
 * same reason [posture] is: this needs no device to test, either.
 *
 * Preference order: any feature that is actually [HingeState.HALF_OPENED] beats one that is
 * [HingeState.FLAT] outright, since a flat feature changes nothing about the layout regardless of
 * which one gets picked. Among several half-open candidates (a configuration no real device produces
 * today), the physically largest hinge is the one most likely to be the one actually visible.
 *
 * `halfOpen[i]`/`area[i]` describe the same feature at index `i`; returns that index, or `null` for
 * an empty list.
 */
fun selectFold(halfOpen: List<Boolean>, area: List<Long>): Int? {
    require(halfOpen.size == area.size) { "halfOpen and area must describe the same features" }
    if (halfOpen.isEmpty()) return null
    val openIndices = halfOpen.indices.filter { halfOpen[it] }
    val pool = openIndices.ifEmpty { halfOpen.indices.toList() }
    return pool.maxByOrNull { area[it] }
}

/**
 * Adapter from the window layout to [posture].
 *
 * Thin on purpose: it translates androidx types and decides nothing itself -- [selectFold] carries
 * the one real decision this function used to make inline, so the logic worth testing needs no
 * device either.
 */
fun postureOf(layoutInfo: WindowLayoutInfo): Posture {
    val folds = layoutInfo.displayFeatures.filterIsInstance<FoldingFeature>()
    val chosen = selectFold(
        folds.map { it.state == FoldingFeature.State.HALF_OPENED },
        folds.map { it.bounds.width().toLong() * it.bounds.height().toLong() },
    ) ?: return Posture.Flat
    val fold = folds[chosen]

    val state = if (fold.state == FoldingFeature.State.HALF_OPENED) {
        HingeState.HALF_OPENED
    } else {
        HingeState.FLAT
    }
    // An unrecognised orientation is treated as horizontal only when the bounds say so; a hinge
    // wider than it is tall runs horizontally. Guessing beats returning Flat, which would draw
    // controls straight across the crease.
    val orientation = when (fold.orientation) {
        FoldingFeature.Orientation.VERTICAL -> HingeOrientation.VERTICAL
        FoldingFeature.Orientation.HORIZONTAL -> HingeOrientation.HORIZONTAL
        else -> if (fold.bounds.width() >= fold.bounds.height()) {
            HingeOrientation.HORIZONTAL
        } else {
            HingeOrientation.VERTICAL
        }
    }
    return posture(
        state,
        orientation,
        fold.bounds.top,
        fold.bounds.bottom,
        fold.bounds.left,
        fold.bounds.right,
    )
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
