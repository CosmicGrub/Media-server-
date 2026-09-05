package dev.lumen.player.remote

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The "Server" card's wording, tested without a device.
 *
 * These are the strings that turn a `HealthReport` into something a person can act on, and the
 * cases that matter most — a lapsed certificate, a never-indexed library, an unknown disk — are
 * exactly the ones a quick manual look at a healthy server never exercises. Free functions over
 * primitives, same as the library list's formatting helpers, so Compose never enters the test.
 */
class RemoteScreenFormattingTest {

    @Test
    fun `a certificate with days left reads as expiring in that many days`() {
        assertEquals("expires in 3 days", formatCertExpiry(3 * 86_400L))
        assertEquals("expires in 1 day", formatCertExpiry(86_400L))
        // Partial days round down: 3 days and 23 hours is still "3 days", not four.
        assertEquals("expires in 3 days", formatCertExpiry(3 * 86_400L + 82_800L))
    }

    @Test
    fun `a certificate expiring within the day is not reported as zero days`() {
        assertEquals("expires within a day", formatCertExpiry(3_600L))
        assertEquals("expires within a day", formatCertExpiry(0L))
    }

    @Test
    fun `a negative expiry reads as already expired that many days ago`() {
        // The value the Rust side's own test sends for a lapsed certificate: -259200 is three days.
        assertEquals("expired 3 days ago", formatCertExpiry(-259_200L))
        assertEquals("expired 1 day ago", formatCertExpiry(-86_400L))
        assertEquals("expired less than a day ago", formatCertExpiry(-60L))
    }

    @Test
    fun `an unknown certificate expiry says so rather than inventing a date`() {
        assertEquals("expiry unknown", formatCertExpiry(null))
    }

    @Test
    fun `a library that has never been reindexed says so`() {
        assertEquals("never reindexed", formatLastIndexed(null, nowUnixSecs = 1_700_000_000L))
    }

    @Test
    fun `a reindex timestamp reads as an age in days`() {
        val now = 1_700_000_000L
        assertEquals("reindexed today", formatLastIndexed(now - 3_600L, now))
        assertEquals("reindexed 1 day ago", formatLastIndexed(now - 86_400L, now))
        assertEquals("reindexed 10 days ago", formatLastIndexed(now - 10 * 86_400L, now))
    }

    @Test
    fun `a reindex timestamp from the future reads as today rather than a negative age`() {
        // Clock skew between the phone and the desktop is ordinary; a negative day count is not
        // something anyone can read.
        val now = 1_700_000_000L
        assertEquals("reindexed today", formatLastIndexed(now + 5 * 86_400L, now))
    }

    @Test
    fun `free disk space is gibibytes with one decimal`() {
        assertEquals("1.0 GiB", formatFreeDisk(1024L * 1024 * 1024))
        assertEquals("1.5 GiB", formatFreeDisk(1024L * 1024 * 1024 * 3 / 2))
        assertEquals("0.0 GiB", formatFreeDisk(0L))
        // The number the Rust health test sends: just under a gigabyte, so the unit does not flip.
        assertEquals("0.9 GiB", formatFreeDisk(999_999_999L))
    }

    @Test
    fun `unknown free disk space says so rather than showing zero`() {
        // Zero would read as a full disk, the one thing this field exists to warn about.
        assertEquals("unknown", formatFreeDisk(null))
    }

    @Test
    fun `the roundtrip is milliseconds`() {
        assertEquals("12 ms", formatRoundtrip(12))
    }

    @Test
    fun `the client count is pluralised`() {
        assertEquals("0 clients connected", formatClientCount(0))
        assertEquals("1 client connected", formatClientCount(1))
        assertEquals("2 clients connected", formatClientCount(2))
    }
}
