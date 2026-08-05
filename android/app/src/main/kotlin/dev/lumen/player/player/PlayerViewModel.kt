package dev.lumen.player.player

import android.app.Application
import android.content.ComponentName
import androidx.core.content.ContextCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.media3.common.MediaItem as ExoMediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.Tracks
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import dev.lumen.player.library.ContentKey
import dev.lumen.player.library.MediaItem
import dev.lumen.player.library.MediaLibrary
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
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
    /** A transient note — "Resumed from 42:10" — that says what just happened and then goes away. */
    val notice: String? = null,
)

@UnstableApi
class PlayerViewModel(app: Application) : AndroidViewModel(app) {

    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    private val resume = ResumeStore(app)

    /**
     * The player, once the session has been reached.
     *
     * Null until then, and the UI has to cope with that rather than assume otherwise. Connecting to
     * a `MediaSessionService` is asynchronous by construction — it starts and binds a service — so a
     * non-null player here would be a lie for the first few hundred milliseconds of every launch.
     */
    private val _player = MutableStateFlow<Player?>(null)
    val player: StateFlow<Player?> = _player.asStateFlow()

    private var controller: MediaController? = null

    /**
     * What the current file offers to choose between.
     *
     * Owned here rather than read straight off the player at the moment a sheet opens, because
     * `Player.getCurrentTracks()` only has an answer once the player has actually parsed the file —
     * empty for a beat after every `play()` call — and the picker needs to react to that arrival
     * rather than see a permanently empty list on a slow-opening file.
     */
    private val _tracks = MutableStateFlow(Tracks.EMPTY)
    val tracks: StateFlow<Tracks> = _tracks.asStateFlow()

    /**
     * Whether the user has explicitly turned subtitles off, as opposed to the file simply not
     * offering a track the platform would auto-select. Tracked from the player's own selection
     * parameters rather than inferred from [tracks], because "off" is a standing instruction that
     * must survive a track list arriving, changing, or briefly going empty between files.
     */
    private val _subtitlesDisabled = MutableStateFlow(false)
    val subtitlesDisabled: StateFlow<Boolean> = _subtitlesDisabled.asStateFlow()

    /**
     * The playing video's pixel dimensions, for Picture-in-Picture's aspect ratio.
     *
     * Read synchronously from a plain Activity callback (`onUserLeaveHint`, not a composable), which
     * is why this is a `StateFlow` with a `.value` rather than something that only makes sense inside
     * composition.
     */
    private val _videoSize = MutableStateFlow(PipAspectRatio.FALLBACK)
    val videoSize: StateFlow<Pair<Int, Int>> = _videoSize.asStateFlow()

    /**
     * Set once this ViewModel is done with.
     *
     * The connection callback can arrive after that — a service bind is not cancellable — and a
     * controller handed to a dead ViewModel would leak the binding for the life of the process.
     */
    private var cleared = false

    /** The content key of what is playing, once known. Null while it is still being computed. */
    private var currentKey: String? = null

    private val listener = object : Player.Listener {
        override fun onIsPlayingChanged(isPlaying: Boolean) {
            _state.value = _state.value.copy(isPlaying = isPlaying)
            // Pausing is the most common moment to leave, so it is the most important one to record.
            if (!isPlaying) savePosition()
        }

        override fun onPlayerError(error: PlaybackException) {
            // The message names the codec or container that failed, which is the whole point of
            // running this against a real library.
            _state.value = _state.value.copy(
                error = "${error.errorCodeName}: ${error.message ?: "playback failed"}"
            )
        }

        override fun onMediaItemTransition(item: ExoMediaItem?, reason: Int) {
            savePosition()
            // A stale track list from the previous file must not be shown, even for one frame, as
            // if it described the new one — that is how a subtitle picker ends up offering a
            // language the file playing does not have.
            _tracks.value = Tracks.EMPTY
            // Likewise the aspect ratio: entering PiP between the transition and the new file's
            // first decoded frame must use a safe fallback, not the previous film's shape.
            _videoSize.value = PipAspectRatio.FALLBACK
        }

        override fun onTracksChanged(tracks: Tracks) {
            _tracks.value = tracks
        }

        override fun onVideoSizeChanged(videoSize: VideoSize) {
            if (videoSize.width > 0 && videoSize.height > 0) {
                _videoSize.value = videoSize.width to videoSize.height
            }
        }

        override fun onTrackSelectionParametersChanged(
            parameters: androidx.media3.common.TrackSelectionParameters
        ) {
            _subtitlesDisabled.value =
                parameters.disabledTrackTypes.contains(androidx.media3.common.C.TRACK_TYPE_TEXT)
        }
    }

    init {
        connect()
        // Playback can run for hours without a state change, and the process can be killed at any
        // moment while it does. A periodic write is what makes the position survive that.
        viewModelScope.launch {
            while (isActive) {
                delay(POSITION_SAVE_INTERVAL_MS)
                if (_state.value.isPlaying) savePosition()
            }
        }
    }

    private fun connect() {
        val app = getApplication<Application>()
        val token = SessionToken(app, ComponentName(app, PlaybackService::class.java))
        val future = MediaController.Builder(app, token).buildAsync()
        future.addListener(
            {
                val connected = runCatching { future.get() }.getOrNull()
                when {
                    connected == null -> _state.value = _state.value.copy(
                        error = "could not reach the playback service; playback is unavailable"
                    )
                    // Arrived too late to be of use to anyone. Released rather than kept, or the
                    // binding outlives the screen that asked for it.
                    cleared -> connected.release()
                    else -> {
                        connected.addListener(listener)
                        controller = connected
                        _player.value = connected
                    }
                }
            },
            ContextCompat.getMainExecutor(app),
        )
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
                    _state.value =
                        _state.value.copy(error = "could not read the library: ${it.message}")
                    emptyList()
                }
            _state.value = _state.value.copy(loading = false, items = items)
        }
    }

    /**
     * Start a file, from where it was left if that is known.
     *
     * The resume lookup takes the fast path when it can and the slow one only when it must. A
     * content key costs a three-megabyte read, which is far too slow to sit between the tap and the
     * picture, so it is cached against the MediaStore id and computed in the background on a miss.
     * The first play of a file therefore starts at zero — which is correct, there is nothing to
     * resume — and every play after that is instant.
     */
    fun play(item: MediaItem) {
        // Whatever was playing keeps its position; switching away is not the same as finishing.
        savePosition()

        val controller = _player.value
        if (controller == null) {
            _state.value = _state.value.copy(
                error = "the playback service is not connected yet; try again in a moment"
            )
            return
        }

        val known = resume.cachedKey(item.id)
        currentKey = known
        val startAt = known?.let { resume.positionFor(it, item.durationMs) }

        _state.value = _state.value.copy(
            nowPlaying = item,
            error = null,
            notice = startAt?.let { "Resumed from ${formatDuration(it)}" },
        )

        val exoItem = ExoMediaItem.fromUri(item.uri)
        if (startAt != null) {
            controller.setMediaItem(exoItem, startAt)
        } else {
            controller.setMediaItem(exoItem)
        }
        controller.prepare()
        controller.playWhenReady = true

        if (known == null) resolveKeyInBackground(item)
    }

    /**
     * Compute the content key for a file being played for the first time.
     *
     * If it turns out to name a file we have seen before — the same bytes under a new path, which is
     * what happens when a file is renamed or moved — a saved position may exist after all. Seeking to
     * it is only reasonable while playback has barely started; a jump ten minutes in would be a
     * player doing something the user did not ask for.
     */
    private fun resolveKeyInBackground(item: MediaItem) {
        viewModelScope.launch {
            val key = ContentKey.of(getApplication(), item.uri, item.sizeBytes) ?: return@launch
            resume.rememberKey(item.id, key)
            // The user may have moved on while three megabytes were being read.
            if (_state.value.nowPlaying?.id != item.id) return@launch
            currentKey = key

            val saved = resume.positionFor(key, item.durationMs) ?: return@launch
            val controller = _player.value ?: return@launch
            if (controller.currentPosition <= LATE_RESUME_WINDOW_MS) {
                controller.seekTo(saved)
                _state.value =
                    _state.value.copy(notice = "Resumed from ${formatDuration(saved)} (moved file)")
            }
        }
    }

    private fun savePosition() {
        val key = currentKey ?: return
        val controller = _player.value ?: return
        val duration = controller.duration.takeIf { it > 0 } ?: return
        resume.save(key, controller.currentPosition, duration)
    }

    /** Forget where we were in what is playing, so it starts from the top next time. */
    fun clearResumePoint() {
        currentKey?.let(resume::clear)
        _state.value = _state.value.copy(notice = "Resume point cleared")
    }

    fun togglePlayPause() {
        val controller = _player.value ?: return
        if (controller.isPlaying) controller.pause() else controller.play()
    }

    /**
     * Switch to a specific audio or subtitle track.
     *
     * Also clears the "subtitles off" instruction when `groupIndex` names a text track — picking a
     * subtitle is unambiguous consent to show one, and leaving the disabled flag set would have the
     * platform hide the very track the user just chose.
     */
    fun selectTrack(groupIndex: Int, trackIndex: Int) {
        val controller = _player.value ?: return
        val group = _tracks.value.groups.getOrNull(groupIndex)
        val override = TrackSelection.overrideFor(_tracks.value, groupIndex, trackIndex) ?: return
        var params = controller.trackSelectionParameters.buildUpon().addOverride(override)
        if (group?.type == androidx.media3.common.C.TRACK_TYPE_TEXT) {
            params = params.setTrackTypeDisabled(androidx.media3.common.C.TRACK_TYPE_TEXT, false)
        }
        controller.trackSelectionParameters = params.build()
    }

    /**
     * Turn subtitles off.
     *
     * `setTrackTypeDisabled`, not an override that selects nothing — an override still leaves the
     * type enabled, so the platform would be free to fall back to a default track the moment one
     * became available (a language switch, a stream change) and silently undo the choice.
     */
    fun disableSubtitles() {
        val controller = _player.value ?: return
        controller.trackSelectionParameters = controller.trackSelectionParameters
            .buildUpon()
            .setTrackTypeDisabled(androidx.media3.common.C.TRACK_TYPE_TEXT, true)
            .build()
    }

    fun dismissError() {
        _state.value = _state.value.copy(error = null)
    }

    fun dismissNotice() {
        _state.value = _state.value.copy(notice = null)
    }

    override fun onCleared() {
        // The position is written before the controller goes, not after.
        savePosition()
        cleared = true
        // Release the controller, not the player: the player belongs to the service and outlives
        // this ViewModel, which is the entire point of the service owning it.
        controller?.removeListener(listener)
        controller?.release()
        controller = null
        _player.value = null
        super.onCleared()
    }

    companion object {
        /** How often the position is written while playing. */
        const val POSITION_SAVE_INTERVAL_MS = 10_000L

        /**
         * How far in a late-arriving resume point may still be honoured. Beyond this, seeking would
         * be the player moving the film without being asked.
         */
        const val LATE_RESUME_WINDOW_MS = 15_000L

        /** `h:mm:ss`, or `m:ss` under an hour. */
        fun formatDuration(ms: Long): String {
            val total = (ms / 1000).coerceAtLeast(0)
            val h = total / 3600
            val m = (total % 3600) / 60
            val s = total % 60
            return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
        }
    }
}
