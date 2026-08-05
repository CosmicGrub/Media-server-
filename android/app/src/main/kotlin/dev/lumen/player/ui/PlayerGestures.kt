package dev.lumen.player.ui

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.positionChange
import kotlin.math.abs

/** Which third of the picture a touch landed in. */
enum class GestureZone { Left, Middle, Right }

/** Which way a drag turned out to be going. */
enum class DragAxis { Undecided, Horizontal, Vertical }

/**
 * What a touch on the video means.
 *
 * All pure functions over floats and longs, so the part that decides what a gesture *does* is
 * testable without a device — which matters here more than usual, because a gesture that maps the
 * wrong way round is invisible in code review and obvious within one second of use.
 *
 * The vocabulary is the one every mobile player already uses, deliberately. A player that invents
 * its own gestures is a player people have to learn instead of use.
 */
object PlayerGestures {

    /** Dragging the full width of the picture seeks this far. */
    const val FULL_WIDTH_SEEK_MS = 120_000L

    /** A double-tap at the side jumps this far. */
    const val DOUBLE_TAP_SEEK_MS = 10_000L

    /**
     * The middle band, as a fraction of the width, where a double-tap means play/pause rather than
     * seek. A third each way: narrower and the seek zones swallow the centre on a wide inner
     * display, wider and they are hard to hit one-handed on the cover screen.
     */
    const val MIDDLE_FRACTION = 1f / 3f

    fun zoneFor(x: Float, width: Float): GestureZone {
        if (width <= 0f) return GestureZone.Middle
        val f = (x / width).coerceIn(0f, 1f)
        val edge = (1f - MIDDLE_FRACTION) / 2f
        return when {
            f < edge -> GestureZone.Left
            f > 1f - edge -> GestureZone.Right
            else -> GestureZone.Middle
        }
    }

    /**
     * Decide, once, whether a drag is a seek or a level change.
     *
     * Locking the axis is what makes both usable. Without it a slightly diagonal scrub also changes
     * the volume, and the user cannot tell which of the two things they did was the one they meant.
     * `Undecided` until the movement is unambiguous — past the touch slop *and* clearly more one way
     * than the other — so a stray finger does nothing at all rather than something arbitrary.
     */
    fun axisFor(dx: Float, dy: Float, slop: Float): DragAxis {
        val ax = abs(dx)
        val ay = abs(dy)
        if (ax < slop && ay < slop) return DragAxis.Undecided
        return if (ax > ay) DragAxis.Horizontal else DragAxis.Vertical
    }

    /**
     * How far a horizontal drag seeks.
     *
     * Proportional to the fraction of the width crossed rather than to the duration: a rate that
     * changes with the length of the film means the same finger movement does something different on
     * every file, and the gesture stops being learnable.
     */
    fun seekDeltaMs(dxPx: Float, widthPx: Float): Long {
        if (widthPx <= 0f) return 0
        return ((dxPx / widthPx) * FULL_WIDTH_SEEK_MS).toLong()
    }

    /** Where a scrub of `dxPx` from `fromMs` lands, clamped inside the file. */
    fun scrubTarget(fromMs: Long, dxPx: Float, widthPx: Float, durationMs: Long): Long {
        val target = fromMs + seekDeltaMs(dxPx, widthPx)
        val end = if (durationMs > 0) durationMs else Long.MAX_VALUE
        return target.coerceIn(0, end)
    }

    /**
     * How much a vertical drag changes a 0..1 level.
     *
     * Negated because screen coordinates grow downward and every human expects up to mean more.
     * Getting this backwards is the single most likely mistake in the whole file, which is why it
     * has a test of its own.
     */
    fun levelDelta(dyPx: Float, heightPx: Float): Float {
        if (heightPx <= 0f) return 0f
        return -dyPx / heightPx
    }

    /** Where a double-tap jumps to, or null when it means play/pause instead. */
    fun doubleTapSeekMs(zone: GestureZone, fromMs: Long, durationMs: Long): Long? = when (zone) {
        GestureZone.Middle -> null
        GestureZone.Left -> (fromMs - DOUBLE_TAP_SEEK_MS).coerceAtLeast(0)
        GestureZone.Right -> {
            val end = if (durationMs > 0) durationMs else Long.MAX_VALUE
            (fromMs + DOUBLE_TAP_SEEK_MS).coerceAtMost(end)
        }
    }
}

/** What a drag is currently adjusting, for the overlay that says so. */
sealed interface GestureFeedback {
    data class Seek(val targetMs: Long, val deltaMs: Long) : GestureFeedback
    data class Brightness(val level: Float) : GestureFeedback
    data class Volume(val level: Float) : GestureFeedback
}

/**
 * One detector for everything the fingers do on the picture.
 *
 * Written as a single `awaitEachGesture` rather than several stacked detectors on purpose. Compose
 * routes a pointer event to every `pointerInput` in the chain and the first to consume it wins, so
 * two detectors that both care about dragging starve each other in ways that depend on timing and
 * are miserable to reason about. One detector that knows how many fingers are down can decide
 * without racing anything.
 *
 * The split is by pointer count: two fingers is always a transform (pinch to zoom, drag to pan when
 * zoomed), one finger is always a level or a scrub. Tap handling stays separate, because tap
 * detection gives up as soon as movement passes the touch slop and so cannot compete with a drag.
 */
suspend fun PointerInputScope.playerDragGestures(
    isZoomed: () -> Boolean,
    onZoom: (Float) -> Unit,
    onPan: (Float, Float) -> Unit,
    onDragStart: (GestureZone, DragAxis) -> Unit,
    onDrag: (GestureZone, DragAxis, dx: Float, dy: Float) -> Unit,
    onDragEnd: (DragAxis) -> Unit,
) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false)
        val zone = PlayerGestures.zoneFor(down.position.x, size.width.toFloat())
        var axis = DragAxis.Undecided
        var totalDx = 0f
        var totalDy = 0f
        var multiTouch = false

        while (true) {
            val event = awaitPointerEvent()
            if (event.changes.none { it.pressed }) break
            // Once a gesture has been two-fingered it stays a transform even if one finger lifts.
            // Otherwise letting go of one finger mid-pinch turns into a wild scrub.
            if (event.changes.count { it.pressed } > 1) multiTouch = true

            if (multiTouch) {
                val zoomChange = event.calculateZoom()
                val pan = event.calculatePan()
                if (zoomChange != 1f) onZoom(zoomChange)
                if (isZoomed() && pan != androidx.compose.ui.geometry.Offset.Zero) {
                    onPan(pan.x, pan.y)
                }
                event.changes.forEach { it.consume() }
                continue
            }

            val change = event.changes.firstOrNull { it.pressed } ?: break
            val d = change.positionChange()
            totalDx += d.x
            totalDy += d.y

            if (axis == DragAxis.Undecided) {
                axis = PlayerGestures.axisFor(totalDx, totalDy, viewConfiguration.touchSlop)
                if (axis != DragAxis.Undecided) onDragStart(zone, axis)
            }
            if (axis != DragAxis.Undecided) {
                onDrag(zone, axis, d.x, d.y)
                // Consumed only once the axis is settled, so a tap — which never gets that far —
                // still reaches the tap detector underneath.
                change.consume()
            }
        }

        if (axis != DragAxis.Undecided) onDragEnd(axis)
    }
}
