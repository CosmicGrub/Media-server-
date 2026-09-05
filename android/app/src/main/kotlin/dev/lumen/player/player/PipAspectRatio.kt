package dev.lumen.player.player

/**
 * Fit a video's pixel dimensions into the aspect ratio Android's Picture-in-Picture will accept.
 *
 * The platform does not clamp on your behalf: `enterPictureInPictureMode` **throws** when the
 * requested ratio falls outside 1:2.39–2.39:1, rather than adjusting it. An anamorphic scope film
 * comfortably exceeds that on either end, and this product exists specifically to play films most
 * players choke on — so the one file class PiP would crash on is exactly the one class most likely
 * to be playing when someone reaches for it.
 *
 * Pure integer arithmetic, deliberately not `android.util.Rational`: that type's real logic is
 * unavailable to a plain JUnit run — the Android stub jar throws for anything beyond a default
 * return — so testing the actual clamp math means keeping it in a type the JVM can execute. The
 * platform `Rational` is constructed once, at the call site in the Activity, from what this returns.
 */
object PipAspectRatio {

    /** Android's accepted band, `1/2.39` to `2.39`. */
    const val MIN = 100.0 / 239.0
    const val MAX = 239.0 / 100.0

    /** A safe default for a file whose dimensions are not known yet. */
    val FALLBACK = 16 to 9

    /**
     * A numerator/denominator pair inside the accepted band, closest to the source shape.
     *
     * Scaled to a denominator around a thousand and reduced, which keeps the ratio accurate to
     * three decimal places — far finer than the platform's own comparison needs — while staying
     * small enough that neither the numerator nor the denominator can overflow a 32-bit `Rational`.
     */
    fun clamp(width: Int, height: Int): Pair<Int, Int> {
        if (width <= 0 || height <= 0) return FALLBACK
        val ratio = (width.toDouble() / height.toDouble()).coerceIn(MIN, MAX)
        val den = 1000
        val num = (ratio * den).toInt().coerceAtLeast(1)
        val g = gcd(num, den)
        return (num / g) to (den / g)
    }

    private tailrec fun gcd(a: Int, b: Int): Int = if (b == 0) a else gcd(b, a % b)
}
