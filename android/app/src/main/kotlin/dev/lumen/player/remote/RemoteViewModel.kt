package dev.lumen.player.remote

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** What the remote-control screen needs to know. */
data class RemoteUiState(
    val connection: ConnectionState = ConnectionState.Disconnected,
    val library: List<RemoteProtocol.LibraryEntry> = emptyList(),
    val loadingLibrary: Boolean = false,
    /** The server's last answer to a health request, or `null` before one has arrived (or after
     * the connection it described went away — a report is about one connection, not the server
     * in general). Fetched once on connect and on demand, not on a timer: `docs/15` §D is a card
     * to glance at, and polling every few seconds would be the battery problem the push-based
     * protocol exists to avoid. */
    val health: RemoteProtocol.HealthReport? = null,
    val loadingHealth: Boolean = false,
    /** Set on a failed pair/auth/command attempt. Shown and dismissible, same convention as the
     * local player's own error surface — the real message names what went wrong. */
    val error: String? = null,
    /** The host:port last tried, kept so the pairing form does not forget what the user typed if a
     * connection attempt fails and they want to retry rather than start over. */
    val lastHost: String = "",
    val lastPort: String = DEFAULT_PORT,
) {
    companion object {
        const val DEFAULT_PORT = "7890"
    }
}

class RemoteViewModel(app: Application) : AndroidViewModel(app) {

    private val client = RemoteClient()
    private val store = RemoteConnectionStore(app)

    private val _state = MutableStateFlow(RemoteUiState())
    val state: StateFlow<RemoteUiState> = _state.asStateFlow()

    /** The remote player's own reported state — a separate flow from [state] because it changes on
     * every position tick and a screen only interested in "am I connected" should not recompose on
     * every one of those. */
    val playback: StateFlow<RemoteProtocol.PlaybackState> = client.playback

    /**
     * The `library_version` the listing was last requested under on the current connection, or
     * `null` when nothing has been requested on it yet — before the first connect, and again after
     * every disconnect, since a version from a previous connection (or the client's own initial
     * zero, which no server ever reported) says nothing about the server on the other end now.
     *
     * Set when a fetch is *sent*, not when its reply arrives. The server reads its library for the
     * reply and snapshots the version for its next state push at two different moments, so a bump
     * that lands while a fetch is in flight may or may not be reflected in the listing that comes
     * back; the only safe reading of "different from what I asked under" is "ask again", which
     * recording the send-time version gives the watcher below for free. A failed fetch leaves the
     * value as sent: the failure is on screen, and the next bump or a tap on "Refresh list" retries.
     *
     * Only ever touched from `viewModelScope`, which is the main thread, so it needs no lock.
     */
    private var listingVersion: Long? = null

    init {
        // The connect path owns the first fetch of both the listing and the health report: every
        // transition to `Connected` — a fresh pairing, a saved token accepted, a reconnect — is the
        // moment the server becomes usable, and having one place say "on connect, load these"
        // beats each of pair/auth remembering to. Launched rather than awaited in place: a fetch
        // can take up to the reply timeout, and this collector must stay free to record the
        // connection dropping meanwhile. The two requests go out concurrently, which is safe because
        // `RemoteClient` writes one line at a time and matches replies by id.
        viewModelScope.launch {
            client.connection.collect { c ->
                _state.value = _state.value.copy(
                    connection = c,
                    // A health report describes the connection that fetched it; once that
                    // connection is gone the numbers are history, not status.
                    health = if (c == ConnectionState.Connected) _state.value.health else null,
                )
                if (c == ConnectionState.Connected) {
                    launch { refreshLibraryIfConnected() }
                    launch { refreshHealthIfConnected() }
                } else {
                    listingVersion = null
                }
            }
        }
        // The auto-refresh `library_version` exists for: the server bumps it on every rescan,
        // whether a client asked (`rescan()`) or its own filesystem watcher noticed a change on
        // disk, and every state push carries the current value. Watching it is how this listing
        // stays current without ever polling for the whole library. The comparison itself is
        // [libraryListingIsStale], against [listingVersion] rather than against the previous push:
        // the server writes its first state push — and so its version — right behind the auth
        // reply, before it can even have read the connect fetch's request, so that fetch's reply is
        // read from a library at least that new. Comparing the push against the fetch's own
        // baseline is what keeps that first push from loading the library a second time, provided
        // the push was read before the fetch recorded its baseline — which is the normal order, the
        // reader thread having both lines in hand before the main thread gets to either. In the
        // other order the baseline is the client's previous value, the push reads as a change, and
        // the listing is fetched once more than needed: an extra round trip on connect in a race,
        // not the double load on every connect that comparing against the previous push gives.
        viewModelScope.launch {
            client.playback.map { it.libraryVersion }.collect { version ->
                if (libraryListingIsStale(listingVersion, version)) refreshLibraryIfConnected()
            }
        }
        // A saved server reconnects on its own; the user should not have to re-enter a code every
        // time the app is reopened for something that already proved it was allowed to connect.
        store.load()?.let { saved ->
            _state.value = _state.value.copy(lastHost = saved.host, lastPort = saved.port.toString())
            reconnectWithToken(saved)
        }
    }

    private fun reconnectWithToken(saved: SavedServer) {
        viewModelScope.launch {
            client.connect(saved.host, saved.port, saved.fingerprint)
            if (client.connection.value !is ConnectionState.AwaitingPairing) return@launch
            client.authenticate(saved.token).onFailure {
                // The server no longer recognises this token — it was revoked, or belongs to a
                // server that has since been reinstalled. Forgetting it is what turns a silent
                // stuck state into a clean re-pairing prompt.
                store.clear()
                _state.value = _state.value.copy(error = "saved connection was rejected: ${it.message}")
            }
            // No fetch here: `authenticate` moving the connection to `Connected` is what loads the
            // listing and the health report, via the connection collector in `init`.
        }
    }

    fun connect(host: String, port: Int) {
        _state.value = _state.value.copy(
            error = null,
            lastHost = host,
            lastPort = port.toString(),
        )
        // No pinned fingerprint yet -- this is necessarily a first connection to this server from
        // this app's point of view (a saved one reconnects via reconnectWithToken instead), so
        // whatever certificate it presents is accepted and recorded, pending the pairing code itself
        // succeeding. See RemoteClient's class doc and FingerprintTrustManager.
        viewModelScope.launch { client.connect(host, port, pinnedFingerprint = null) }
    }

    fun submitPairingCode(code: String) {
        viewModelScope.launch {
            client.pair(code)
                .onSuccess { token ->
                    val host = _state.value.lastHost
                    val port = _state.value.lastPort.toIntOrNull()
                    val fingerprint = client.observedFingerprint
                    when {
                        port == null -> {}
                        // The handshake that got us here must have observed a fingerprint -- if it
                        // somehow did not, saving a server with nothing to pin would make every future
                        // reconnect trust on sight forever, which is worse than not saving at all.
                        fingerprint == null -> _state.value = _state.value.copy(
                            error = "paired, but no certificate fingerprint was recorded; " +
                                "reconnect will require pairing again",
                        )
                        else -> store.save(SavedServer(host, port, token, fingerprint))
                    }
                    // No fetch here either: `pair` moved the connection to `Connected`, and the
                    // connection collector in `init` loads the listing off that transition.
                }
                .onFailure { _state.value = _state.value.copy(error = it.message) }
        }
    }

    private suspend fun refreshLibraryIfConnected() {
        if (client.connection.value != ConnectionState.Connected) return
        listingVersion = client.playback.value.libraryVersion // See the field's doc for why now.
        _state.value = _state.value.copy(loadingLibrary = true)
        client.library()
            .onSuccess { entries ->
                _state.value = _state.value.copy(library = entries, loadingLibrary = false)
            }
            .onFailure {
                _state.value =
                    _state.value.copy(loadingLibrary = false, error = "could not load the library: ${it.message}")
            }
    }

    /** Re-fetch the listing the server already has. Contrast [rescan], which asks the server to
     * re-walk its disk first. */
    fun refreshLibrary() {
        viewModelScope.launch { refreshLibraryIfConnected() }
    }

    /** Ask the server to re-walk its library root. Deliberately does not refresh the listing
     * itself on success: the server bumps `library_version` as part of every rescan, the next
     * state push carries it, and the version watcher in `init` refreshes from there — exactly as it
     * does for a rescan the server's own filesystem watcher triggered. A second fetch here would
     * mean every tap loads the library twice.
     *
     * The wait is [RemoteClient.RESCAN_TIMEOUT_MS], minutes rather than seconds, because the server
     * only answers once the walk is done; see that constant. */
    fun rescan() {
        viewModelScope.launch {
            client.rescan().onFailure {
                _state.value = _state.value.copy(error = "could not rescan the server: ${it.message}")
            }
        }
    }

    private suspend fun refreshHealthIfConnected() {
        if (client.connection.value != ConnectionState.Connected) return
        _state.value = _state.value.copy(loadingHealth = true)
        client.health()
            .onSuccess { report ->
                _state.value = _state.value.copy(health = report, loadingHealth = false)
            }
            .onFailure {
                _state.value =
                    _state.value.copy(loadingHealth = false, error = "could not read server health: ${it.message}")
            }
    }

    fun refreshHealth() {
        viewModelScope.launch { refreshHealthIfConnected() }
    }

    fun play(entry: RemoteProtocol.LibraryEntry) {
        viewModelScope.launch {
            client.play(entry.path).onFailure {
                _state.value = _state.value.copy(error = "could not play ${entry.title}: ${it.message}")
            }
        }
    }

    fun togglePlayPause() {
        viewModelScope.launch { client.toggle() }
    }

    fun seek(positionMs: Long) {
        viewModelScope.launch { client.seek(positionMs) }
    }

    fun setVolume(level: Int) {
        viewModelScope.launch { client.setVolume(level) }
    }

    /** Disconnects and forgets the saved server — the deliberate "log out" action, distinct from a
     * connection merely dropping, which should reconnect rather than require pairing again. */
    fun forget() {
        store.clear()
        client.close()
        _state.value = RemoteUiState()
    }

    fun dismissError() {
        _state.value = _state.value.copy(error = null)
    }

    override fun onCleared() {
        client.release()
        super.onCleared()
    }
}

/**
 * Whether a state push carrying `library_version` [current] means the listing, last requested under
 * [previous], should be fetched again.
 *
 * Any change counts, not only an increase. The server's counter is in-memory and starts at zero
 * every time `lumen serve` starts, so a reconnect after the desktop rebooted can see the version
 * go *down* — and the library on disk may well have changed in between. Treating only increases as
 * stale would leave exactly that case showing an old listing until the user noticed. The one thing
 * that is not stale is the same number twice, which is what every position-tick state push carries.
 *
 * A `null` [previous] — no listing requested on this connection yet — is never stale: the connect
 * path fetches the listing off the connection becoming usable regardless of any version, and a
 * push arriving before that fetch has recorded its baseline would otherwise load the library a
 * second time. The one situation this leaves unfetched is a version change with no listing at all,
 * which cannot happen: the server pushes state only to an authenticated connection, and
 * authenticating is what triggers the fetch.
 *
 * A free function over two numbers, kept out of the ViewModel class, so the decision is checkable on
 * a plain JVM without an `Application` to construct one with.
 */
internal fun libraryListingIsStale(previous: Long?, current: Long): Boolean =
    previous != null && current != previous
