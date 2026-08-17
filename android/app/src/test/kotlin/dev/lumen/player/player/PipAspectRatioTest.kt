package dev.lumen.player.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The clamp math, checked without a device.
 *
 * `enterPictureInPictureMode` throws for a ratio outside the accepted band rather than adjusting
 * it, so a bug here is not a cosmetic PiP window — it is a crash the moment someone backgrounds an
 * anamorphic film, which is exactly the kind of file this product exists to get right.
 */
class PipAspectRatioTest {

    private fun ratio(pair: Pair<Int, Int>) = pair.first.toDouble() / pair.second.toDouble()

    @Test
    fun `an ordinary 16 by 9 file passes through unclamped`() {
        val (n, d) = PipAspectRatio.clamp(1920, 1080)
        assertEquals(16.0 / 9.0, n.toDouble() / d, 0.001)
    }

    @Test
    fun `an anamorphic scope film is clamped to the accepted band, not rejected`() {
        // 2.76:1 (Ben-Hur, Ultra Panavision) is a real theatrical ratio and comfortably outside what
        // PiP accepts. The whole point of this type existing is that this must not crash.
        val clamped = PipAspectRatio.clamp(2760, 1000)
        val r = ratio(clamped)
        assertTrue("expected <= MAX, got $r", r <= PipAspectRatio.MAX + 0.001)
    }

    @Test
    fun `a tall portrait capture is clamped at the other end`() {
        val clamped = PipAspectRatio.clamp(1080, 3000)
        val r = ratio(clamped)
        assertTrue("expected >= MIN, got $r", r >= PipAspectRatio.MIN - 0.001)
    }

    @Test
    fun `every result lands inside the accepted band, swept across a wide range of shapes`() {
        // Rather than argue the formula, check the property it exists to guarantee, across enough
        // shapes that a boundary mistake could not hide between the cases picked by hand.
        for (w in 100..3000 step 137) {
            for (h in 100..3000 step 211) {
                val r = ratio(PipAspectRatio.clamp(w, h))
                assertTrue(
                    "($w, $h) -> $r is outside [${PipAspectRatio.MIN}, ${PipAspectRatio.MAX}]",
                    r >= PipAspectRatio.MIN - 0.001 && r <= PipAspectRatio.MAX + 0.001,
                )
            }
        }
    }

    @Test
    fun `unknown or invalid dimensions fall back rather than producing garbage`() {
        assertEquals(PipAspectRatio.FALLBACK, PipAspectRatio.clamp(0, 0))
        assertEquals(PipAspectRatio.FALLBACK, PipAspectRatio.clamp(-1, 1080))
        assertEquals(PipAspectRatio.FALLBACK, PipAspectRatio.clamp(1920, 0))
    }

    @Test
    fun `neither part of the result can overflow a 32-bit Rational`() {
        val (n, d) = PipAspectRatio.clamp(Int.MAX_VALUE, 1)
        assertTrue(n < 100_000 && d < 100_000)
    }
}
