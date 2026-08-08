package dev.lumen.player.remote

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** What the remote-control screen needs to know. */
data class RemoteUiState(
    val connection: ConnectionState = ConnectionState.Disconnected,
    val library: List<RemoteProtocol.LibraryEntry> = emptyList(),
    val loadingLibrary: Boolean = false,
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

    init {
        viewModelScope.launch {
            client.connection.collect { c -> _state.value = _state.value.copy(connection = c) }
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
            client.connect(saved.host, saved.port)
            if (client.connection.value !is ConnectionState.AwaitingPairing) return@launch
            client.authenticate(saved.token).onFailure {
                // The server no longer recognises this token — it was revoked, or belongs to a
                // server that has since been reinstalled. Forgetting it is what turns a silent
                // stuck state into a clean re-pairing prompt.
                store.clear()
                _state.value = _state.value.copy(error = "saved connection was rejected: ${it.message}")
            }
            refreshLibraryIfConnected()
        }
    }

    fun connect(host: String, port: Int) {
        _state.value = _state.value.copy(
            error = null,
            lastHost = host,
            lastPort = port.toString(),
        )
        viewModelScope.launch { client.connect(host, port) }
    }

    fun submitPairingCode(code: String) {
        viewModelScope.launch {
            client.pair(code)
                .onSuccess { token ->
                    val host = _state.value.lastHost
                    val port = _state.value.lastPort.toIntOrNull()
                    if (port != null) store.save(SavedServer(host, port, token))
                    refreshLibraryIfConnected()
                }
                .onFailure { _state.value = _state.value.copy(error = it.message) }
        }
    }

    private suspend fun refreshLibraryIfConnected() {
        if (client.connection.value != ConnectionState.Connected) return
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

    fun refreshLibrary() {
        viewModelScope.launch { refreshLibraryIfConnected() }
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
