package dev.lumen.player.fold

import kotlin.test.assertEquals
import org.junit.Test

/**
 * The fold posture decision, tested without a device.
 *
 * This is the logic a foldable build exists for, and it is the hardest thing to check by hand:
 * holding a real Fold 5 at a stable half-open angle while reading logcat is genuinely awkward, and
 * an emulator's fold controls only reproduce a few of the cases. Keeping the decision free of
 * Android types means every case runs in milliseconds on any JVM.
 */
class PostureTest {

    @Test
    fun `a flat hinge is flat whatever its orientation`() {
        // The inner display unfolded: there is a hinge, but content may cross it freely. Treating
        // this as tabletop would split a full-screen video in half for no reason.
        assertEquals(
            Posture.Flat,
            posture(HingeState.FLAT, HingeOrientation.HORIZONTAL, 900, 1000, 0, 0),
        )
        assertEquals(
            Posture.Flat,
            posture(HingeState.FLAT, HingeOrientation.VERTICAL, 0, 0, 900, 1000),
        )
    }

    @Test
    fun `half open with a horizontal hinge is tabletop, carrying the crease bounds`() {
        // The posture that justifies the whole build. The bounds must survive: they are what keeps
        // controls off the crease.
        val p = posture(HingeState.HALF_OPENED, HingeOrientation.HORIZONTAL, 880, 920, 0, 0)
        assertEquals(Posture.Tabletop(hingeTopPx = 880, hingeBottomPx = 920), p)
    }

    @Test
    fun `half open with a vertical hinge is book mode`() {
        val p = posture(HingeState.HALF_OPENED, HingeOrientation.VERTICAL, 0, 0, 1070, 1110)
        assertEquals(Posture.Book(hingeLeftPx = 1070, hingeRightPx = 1110), p)
    }

    @Test
    fun `a zero-width hinge still reports its position`() {
        // Some devices report a crease with no thickness. The split point still matters, and
        // collapsing to Flat would put controls exactly on the fold.
        val p = posture(HingeState.HALF_OPENED, HingeOrientation.HORIZONTAL, 906, 906, 0, 0)
        assertEquals(Posture.Tabletop(906, 906), p)
    }

    @Test
    fun `tabletop bounds match a Fold 5 inner display`() {
        // 2176x1812 inner panel, crease near the middle. Sanity-checks that the numbers the layout
        // divides by are the ones handed in, rather than being swapped or offset.
        val p = posture(HingeState.HALF_OPENED, HingeOrientation.HORIZONTAL, 890, 922, 0, 0)
        val top = (p as Posture.Tabletop).hingeTopPx
        assertEquals(890, top)
        assertEquals(922, p.hingeBottomPx)
        // Video occupies everything above the crease; the rest is for controls.
        assertEquals(890, top, "video region height")
    }

    @Test
    fun `selectFold returns null for no features at all`() {
        assertEquals(null, selectFold(emptyList(), emptyList()))
    }

    @Test
    fun `selectFold picks the only feature there is`() {
        assertEquals(0, selectFold(listOf(true), listOf(500_000L)))
        assertEquals(0, selectFold(listOf(false), listOf(500_000L)))
    }

    @Test
    fun `selectFold prefers a half-open feature over a flat one regardless of size`() {
        // A real device reports at most one hinge, but the API shape allows more; if it ever
        // happens, the flat one changes nothing about the layout no matter how large it is, so the
        // half-open one is what has to win even when it is physically the smaller of the two.
        val halfOpen = listOf(false, true)
        val area = listOf(9_000_000L, 100L)
        assertEquals(1, selectFold(halfOpen, area))
    }

    @Test
    fun `selectFold breaks a tie between two half-open features by area`() {
        val halfOpen = listOf(true, true, true)
        val area = listOf(200_000L, 900_000L, 500_000L)
        assertEquals(1, selectFold(halfOpen, area))
    }

    @Test
    fun `selectFold picks something even when nothing is half-open`() {
        // Every feature flat: the result does not matter for the eventual posture (they all resolve
        // to Flat), but the function must still return a valid index rather than null, which would
        // be read as "no features at all" -- a different, wrong signal.
        val idx = selectFold(listOf(false, false), listOf(10L, 20L))
        assertEquals(true, idx == 0 || idx == 1)
    }

    @Test(expected = IllegalArgumentException::class)
    fun `selectFold refuses mismatched list lengths rather than reading past the end`() {
        selectFold(listOf(true, false), listOf(100L))
    }
}
