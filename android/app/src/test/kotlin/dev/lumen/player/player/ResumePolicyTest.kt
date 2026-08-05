package dev.lumen.player.player

import dev.lumen.player.library.ContentKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The resume rules, checked without a device.
 *
 * The policy is a companion-object function over two longs precisely so it can be tested here. Every
 * bug this catches is one a person would only find by watching a film to the end and reopening it.
 */
class ResumePolicyTest {

    private val twoHours = 2 * 60 * 60 * 1000L

    @Test
    fun `a film watched to the end starts from the top next time`() {
        // The single most irritating resume bug: reopening a finished film and landing on the
        // credits. `save` discards rather than stores in this case, and `shouldResume` agrees.
        assertTrue(ResumeStore.isEffectivelyFinished(twoHours - 10_000, twoHours))
        assertFalse(ResumeStore.shouldResume(twoHours - 10_000, twoHours))
    }

    @Test
    fun `barely started is not worth resuming`() {
        assertFalse(ResumeStore.shouldResume(3_000, twoHours))
        assertFalse(ResumeStore.shouldResume(ResumeStore.MIN_SAVE_MS - 1, twoHours))
        assertTrue(ResumeStore.shouldResume(ResumeStore.MIN_SAVE_MS, twoHours))
    }

    @Test
    fun `the middle of a film resumes`() {
        assertTrue(ResumeStore.shouldResume(twoHours / 2, twoHours))
        assertFalse(ResumeStore.isEffectivelyFinished(twoHours / 2, twoHours))
    }

    @Test
    fun `both end rules are needed because each is wrong at one end of the range`() {
        // A 90-second margin is right for a feature and would swallow a short clip whole.
        val clip = 3 * 60 * 1000L
        assertFalse("30s of a 3m clip is not finished", ResumeStore.isEffectivelyFinished(30_000, clip))
        assertTrue("2m55s of a 3m clip is", ResumeStore.isEffectivelyFinished(175_000, clip))

        // A 97% fraction is right for the clip and would leave four minutes of a four-hour film
        // counting as unwatched.
        val long = 4 * 60 * 60 * 1000L
        assertTrue(
            "60s from the end of a 4h film is finished",
            ResumeStore.isEffectivelyFinished(long - 60_000, long),
        )
    }

    @Test
    fun `an unknown duration resumes rather than losing the position`() {
        // Duration is not always known — a stream, or metadata that has not arrived. Treating that
        // as "finished" would throw the position away for exactly the files hardest to navigate.
        assertFalse(ResumeStore.isEffectivelyFinished(60_000, 0))
        assertFalse(ResumeStore.isEffectivelyFinished(60_000, -1))
        assertTrue(ResumeStore.shouldResume(60_000, 0))
    }

    @Test
    fun `saving and resuming agree, so a discarded position can never be offered`() {
        // If these two rules ever disagreed, a position would be stored that could not be used, or
        // offered that should have been dropped. Checked across the whole range rather than argued.
        for (pos in 0..twoHours step 60_000) {
            val savable = pos >= ResumeStore.MIN_SAVE_MS &&
                !ResumeStore.isEffectivelyFinished(pos, twoHours)
            assertEquals(
                "position $pos disagrees between save and resume",
                savable,
                ResumeStore.shouldResume(pos, twoHours),
            )
        }
    }

    @Test
    fun `durations format the way a person writes them`() {
        assertEquals("0:42", PlayerViewModel.formatDuration(42_000))
        assertEquals("42:10", PlayerViewModel.formatDuration(42 * 60_000 + 10_000))
        assertEquals("2:05:03", PlayerViewModel.formatDuration(2 * 3600_000 + 5 * 60_000 + 3_000))
        assertEquals("0:00", PlayerViewModel.formatDuration(-5))
    }

    @Test
    fun `the content key samples the same regions as the desktop`() {
        // Head, middle and tail, plus the length. Head catches the container header, middle the
        // payload, tail the index or `moov` that many muxers write last. Getting this wrong would
        // make two different files agree, which is a resume point on the wrong film.
        val big = 100 * ContentKey.CHUNK
        assertEquals(
            listOf(0L, big / 2 - ContentKey.CHUNK / 2, big - ContentKey.CHUNK),
            ContentKey.sampleOffsets(big),
        )
        assertEquals(3 * ContentKey.CHUNK, ContentKey.readCost(big))
    }

    @Test
    fun `a small file is read whole rather than sampled`() {
        // Below the threshold the three regions would overlap, so sampling would read most of the
        // file twice and still miss part of it.
        val small = ContentKey.CHUNK + 1
        val offsets = ContentKey.sampleOffsets(small)
        assertEquals(listOf(0L, ContentKey.CHUNK), offsets)
        assertEquals(small, ContentKey.readCost(small))
        assertEquals(emptyList<Long>(), ContentKey.sampleOffsets(0))
    }

    @Test
    fun `the length is mixed in little-endian so a truncated copy cannot collide`() {
        assertEquals(
            listOf<Byte>(1, 0, 0, 0, 0, 0, 0, 0),
            ContentKey.longToBytes(1).toList(),
        )
        assertEquals(
            listOf<Byte>(0, 0, 0, 0, 0, 0, 0, 1),
            ContentKey.longToBytes(1L shl 56).toList(),
        )
    }
}
