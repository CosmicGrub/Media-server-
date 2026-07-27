package dev.lumen.player.player

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService

/**
 * Keeps playback alive when the app is not in the foreground.
 *
 * A `MediaSessionService` is what makes the notification controls, Bluetooth buttons and the
 * lock-screen surface work. It matters more on a foldable than on a normal phone: closing a Fold 5
 * moves the app from the inner display to the cover screen, and without a session the transition
 * can tear down playback entirely.
 */
@UnstableApi
class PlaybackService : MediaSessionService() {

    private var mediaSession: MediaSession? = null

    override fun onCreate() {
        super.onCreate()
        val player = ExoPlayer.Builder(this)
            .setRenderersFactory(
                DefaultRenderersFactory(this)
                    // Fall back to a software decoder when the hardware one refuses a stream rather
                    // than failing the file. This is the "no refusal" guarantee from `docs/11` §G2 in
                    // its Android form: a phone's MediaCodec list is much narrower than a desktop's,
                    // and a codec the hardware will not take is exactly the case that must still play.
                    .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_PREFER)
                    .setEnableDecoderFallback(true)
            )
            .build()

        mediaSession = MediaSession.Builder(this, player).build()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = mediaSession

    override fun onTaskRemoved(rootIntent: android.content.Intent?) {
        // Swiping the app away should stop playback rather than leaving an orphaned notification —
        // unless something is actually playing, in which case the user is listening on purpose.
        val player = mediaSession?.player
        if (player == null || !player.playWhenReady || player.mediaItemCount == 0) {
            stopSelf()
        }
    }

    override fun onDestroy() {
        mediaSession?.run {
            player.release()
            release()
        }
        mediaSession = null
        super.onDestroy()
    }
}
