package dev.lumen.player.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What each gesture means, checked without a finger.
 *
 * A gesture mapped the wrong way round is invisible in review and obvious within one second of use,
 * which is exactly the class of bug worth pinning down in a test rather than in a bug report.
 */
class PlayerGesturesTest {

    private val width = 1080f
    private val height = 2400f

    @Test
    fun `up means more, on both levels`() {
        // Screen coordinates grow downward and every human expects up to mean more. This is the
        // single most likely mistake in the file.
        assertTrue("dragging up must raise", PlayerGestures.levelDelta(-100f, height) > 0f)
        assertTrue("dragging down must lower", PlayerGestures.levelDelta(100f, height) < 0f)
        // The full height covers the full range, so a level can be taken from nothing to full in one
        // movement without running out of screen.
        assertEquals(1f, PlayerGestures.levelDelta(-height, height), 0.0001f)
    }

    @Test
    fun `right seeks forward and left seeks back`() {
        assertTrue(PlayerGestures.seekDeltaMs(200f, width) > 0)
        assertTrue(PlayerGestures.seekDeltaMs(-200f, width) < 0)
        assertEquals(PlayerGestures.FULL_WIDTH_SEEK_MS, PlayerGestures.seekDeltaMs(width, width))
    }

    @Test
    fun `the seek rate does not change with the length of the film`() {
        // A rate proportional to duration means the same finger movement does something different on
        // every file, and the gesture stops being learnable. Same pixels, same seconds, always.
        val short = PlayerGestures.scrubTarget(0, 100f, width, 5 * 60_000L)
        val long = PlayerGestures.scrubTarget(0, 100f, width, 4 * 60 * 60_000L)
        assertEquals(short, long)
    }

    @Test
    fun `a scrub cannot run off either end of the file`() {
        val duration = 90 * 60_000L
        assertEquals(0, PlayerGestures.scrubTarget(1_000, -width * 10, width, duration))
        assertEquals(duration, PlayerGestures.scrubTarget(duration - 1_000, width * 10, width, duration))
    }

    @Test
    fun `an unknown duration does not clamp the forward end to zero`() {
        // Duration is -1 or 0 until the file is prepared. Clamping to it would pin every forward
        // seek to the start — a scrub that silently rewinds is worse than one that does nothing.
        val target = PlayerGestures.scrubTarget(30_000, 200f, width, 0)
        assertTrue("expected to move forward, got $target", target > 30_000)
    }

    @Test
    fun `the picture divides into three zones with the middle a third wide`() {
        assertEquals(GestureZone.Left, PlayerGestures.zoneFor(10f, width))
        assertEquals(GestureZone.Middle, PlayerGestures.zoneFor(width / 2, width))
        assertEquals(GestureZone.Right, PlayerGestures.zoneFor(width - 10f, width))
        // Just inside each boundary, so the fractions are actually what they claim.
        assertEquals(GestureZone.Left, PlayerGestures.zoneFor(width * 0.32f, width))
        assertEquals(GestureZone.Middle, PlayerGestures.zoneFor(width * 0.34f, width))
        assertEquals(GestureZone.Middle, PlayerGestures.zoneFor(width * 0.66f, width))
        assertEquals(GestureZone.Right, PlayerGestures.zoneFor(width * 0.68f, width))
    }

    @Test
    fun `a zero-width surface does not divide by zero`() {
        assertEquals(GestureZone.Middle, PlayerGestures.zoneFor(0f, 0f))
        assertEquals(0L, PlayerGestures.seekDeltaMs(100f, 0f))
        assertEquals(0f, PlayerGestures.levelDelta(100f, 0f), 0.0001f)
    }

    @Test
    fun `the axis stays undecided until the movement is unambiguous`() {
        val slop = 24f
        // Below the slop, nothing has been decided and nothing should happen.
        assertEquals(DragAxis.Undecided, PlayerGestures.axisFor(5f, 5f, slop))
        assertEquals(DragAxis.Undecided, PlayerGestures.axisFor(-10f, 8f, slop))
        // Past it, the dominant direction wins and stays won — without this a slightly diagonal
        // scrub also changes the volume and the user cannot tell which thing they did.
        assertEquals(DragAxis.Horizontal, PlayerGestures.axisFor(50f, 10f, slop))
        assertEquals(DragAxis.Vertical, PlayerGestures.axisFor(10f, -50f, slop))
    }

    @Test
    fun `double-tap seeks at the sides and plays or pauses in the middle`() {
        val duration = 60 * 60_000L
        val from = 30 * 60_000L
        assertEquals(
            from - PlayerGestures.DOUBLE_TAP_SEEK_MS,
            PlayerGestures.doubleTapSeekMs(GestureZone.Left, from, duration),
        )
        assertEquals(
            from + PlayerGestures.DOUBLE_TAP_SEEK_MS,
            PlayerGestures.doubleTapSeekMs(GestureZone.Right, from, duration),
        )
        assertNull(
            "the middle is play/pause, not a seek",
            PlayerGestures.doubleTapSeekMs(GestureZone.Middle, from, duration),
        )
    }

    @Test
    fun `a double-tap near either end clamps instead of overshooting`() {
        val duration = 60_000L
        assertEquals(0L, PlayerGestures.doubleTapSeekMs(GestureZone.Left, 3_000, duration))
        assertEquals(duration, PlayerGestures.doubleTapSeekMs(GestureZone.Right, 58_000, duration))
    }
}
