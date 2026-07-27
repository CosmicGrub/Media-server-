package dev.lumen.player.library

import kotlin.test.assertEquals
import org.junit.Test

/**
 * The library list's formatting, tested on a plain JVM.
 *
 * These are the strings a user reads for every file, so an off-by-one in the minute rollover or a
 * unit boundary is visible on every row. Free functions over primitives keep them out of reach of
 * `MediaItem`, which holds an Android `Uri` and would otherwise drag the framework into a test that
 * only checks text.
 */
class FormattingTest {

    @Test
    fun `durations under an hour omit the hour field`() {
        assertEquals("0:05", formatDuration(5_000))
        assertEquals("1:05", formatDuration(65_000))
        assertEquals("59:59", formatDuration(3_599_000))
    }

    @Test
    fun `the hour boundary rolls over rather than showing sixty minutes`() {
        assertEquals("1:00:00", formatDuration(3_600_000))
        assertEquals("2:14:07", formatDuration((2 * 3600 + 14 * 60 + 7) * 1000L))
    }

    @Test
    fun `an unknown duration is a dash rather than zero`() {
        // MediaStore reports 0 for a file it could not probe. Showing "0:00" would claim the file is
        // empty; a dash says the length is unknown, which is the truth.
        assertEquals("—", formatDuration(0))
        assertEquals("—", formatDuration(-1))
    }

    @Test
    fun `sizes use the largest unit that keeps the number above one`() {
        assertEquals("512 B", formatSize(512))
        assertEquals("1.0 KB", formatSize(1024))
        assertEquals("1.5 MB", formatSize(1024L * 1024 * 3 / 2))
        assertEquals("2.0 GB", formatSize(1024L * 1024 * 1024 * 2))
    }

    @Test
    fun `a remux-sized file reads in gigabytes, not megabytes`() {
        // The case this product exists for: a 4K remux is tens of gigabytes, and a five-digit MB
        // figure would be unreadable in a list.
        assertEquals("62.0 GB", formatSize(62L * 1024 * 1024 * 1024))
    }

    @Test
    fun `zero bytes is reported exactly`() {
        assertEquals("0 B", formatSize(0))
    }
}
