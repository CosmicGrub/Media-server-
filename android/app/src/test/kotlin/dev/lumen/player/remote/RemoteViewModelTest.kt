package dev.lumen.player.remote

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The one decision `RemoteViewModel` makes about `library_version`, checked on a plain JVM.
 *
 * The ViewModel itself needs an `Application`, and this project deliberately carries no Robolectric
 * to fake one — so the decision is a free function over two numbers (see its own doc comment) and
 * this is the proof it says what the protocol's `library_version` was designed to let a client say
 * cheaply: "is my cached listing stale?"
 */
class RemoteViewModelTest {

    @Test
    fun `a library version that moved on means the cached listing is stale`() {
        assertTrue(libraryListingIsStale(previous = 3, current = 4))
        assertTrue(libraryListingIsStale(previous = 0, current = 1))
    }

    @Test
    fun `the same library version twice is not stale`() {
        // Every position-tick state push repeats the current version; refreshing on each one would
        // reload the whole library several times a second.
        assertFalse(libraryListingIsStale(previous = 7, current = 7))
        assertFalse(libraryListingIsStale(previous = 0, current = 0))
    }

    @Test
    fun `a library version that went down is stale because the server restarted`() {
        // The server's counter lives in memory and restarts at zero with the process. A reconnect
        // after the desktop rebooted sees 12 -> 0, and the disk may have changed meanwhile; only
        // refreshing on increases would leave that case showing the old listing.
        assertTrue(libraryListingIsStale(previous = 12, current = 0))
    }

    @Test
    fun `a version push before any listing was requested on this connection is not stale`() {
        // The server's first state push follows its auth reply immediately, and the connect path
        // is already fetching the listing off that same transition. Treating the push as a change
        // from "nothing yet" is exactly what loaded the library twice on every connect.
        assertFalse(libraryListingIsStale(previous = null, current = 1))
        assertFalse(libraryListingIsStale(previous = null, current = 0))
    }
}
