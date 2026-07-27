package dev.lumen.player.player

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.media3.common.MediaItem as ExoMediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import dev.lumen.player.library.MediaItem
import dev.lumen.player.library.MediaLibrary
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** What the UI needs to know about the library and what is playing. */
data class UiState(
    val loading: Boolean = true,
    val items: List<MediaItem> = emptyList(),
    val nowPlaying: MediaItem? = null,
    val isPlaying: Boolean = false,
    /** Set when a file fails. Shown rather than swallowed: which files fail is the useful output. */
    val error: String? = null,
    val permissionGranted: Boolean = false,
)

@UnstableApi
class PlayerViewModel(app: Application) : AndroidViewModel(app) {

    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    /**
     * The player is owned here rather than by the Composable.
     *
     * A Fold 5 recreates nothing on fold when `configChanges` covers the transition, but process
     * death and multi-window moves still happen. Keeping the player in the ViewModel means playback
     * position survives anything short of the process going away.
     */
    val player: ExoPlayer = ExoPlayer.Builder(app)
        .setRenderersFactory(
            DefaultRenderersFactory(app)
                .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_PREFER)
                .setEnableDecoderFallback(true)
        )
        .build()
        .apply {
            addListener(object : Player.Listener {
                override fun onIsPlayingChanged(isPlaying: Boolean) {
                    _state.value = _state.value.copy(isPlaying = isPlaying)
                }

                override fun onPlayerError(error: PlaybackException) {
                    // The message names the codec or container that failed, which is the whole point
                    // of running this against a real library.
                    _state.value = _state.value.copy(
                        error = "${error.errorCodeName}: ${error.message ?: "playback failed"}"
                    )
                }
            })
        }

    fun onPermissionResult(granted: Boolean) {
        _state.value = _state.value.copy(permissionGranted = granted)
        if (granted) refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(loading = true, error = null)
            val items = runCatching { MediaLibrary.loadVideos(getApplication()) }
                .getOrElse {
                    _state.value = _state.value.copy(error = "could not read the library: ${it.message}")
                    emptyList()
                }
            _state.value = _state.value.copy(loading = false, items = items)
        }
    }

    fun play(item: MediaItem) {
        _state.value = _state.value.copy(nowPlaying = item, error = null)
        player.setMediaItem(ExoMediaItem.fromUri(item.uri))
        player.prepare()
        player.playWhenReady = true
    }

    fun togglePlayPause() {
        if (player.isPlaying) player.pause() else player.play()
    }

    fun dismissError() {
        _state.value = _state.value.copy(error = null)
    }

    override fun onCleared() {
        player.release()
        super.onCleared()
    }
}
