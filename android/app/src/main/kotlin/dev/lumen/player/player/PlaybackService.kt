package dev.lumen.player.player

import android.app.PendingIntent
import android.content.Intent
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService

/**
 * The one player.
 *
 * This service owns the only `ExoPlayer` in the app, and everything else reaches it through a
 * `MediaController` bound to the session. That is not architecture for its own sake — it is the fix
 * for a real defect. The first version built an `ExoPlayer` here *and* another in the ViewModel, and
 * nothing ever started the service, so the `MediaSession` was driving a player that was not the one
 * on screen. Every consequence was silent: notification controls did nothing, the lock-screen
 * surface did nothing, Bluetooth and headset buttons did nothing, and the app looked fine because
 * the ViewModel's player was the one being watched.
 *
 * Owning it here matters more on a foldable than on an ordinary phone: closing a Fold 5 moves the
 * app from the inner display to the cover screen, and a player tied to an Activity can be torn down
 * in that transition. A player behind a session outlives it.
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
                    // than failing the file — the "no refusal" guarantee (`docs/11` §G2) in its
                    // Android form. The honest limit is worth stating: with no FFmpeg extension on
                    // the classpath there are no extension renderers for this to prefer, so today it
                    // buys only MediaCodec-to-MediaCodec fallback. TrueHD, DTS-HD MA and PGS stay
                    // undecodable here until that extension is built and shipped.
                    .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_PREFER)
                    .setEnableDecoderFallback(true)
            )
            // Take audio focus properly: pause for a call, duck for a notification, stop when
            // something else takes over for good. Without this the app plays over the top of
            // everything, which is behaviour people uninstall over.
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                    .setUsage(C.USAGE_MEDIA)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            // Audio keeps going with the screen off; the wake lock is dropped as soon as it stops.
            .setWakeMode(C.WAKE_MODE_LOCAL)
            // Pause when the headphones are pulled out, rather than blasting the film out of the
            // phone speaker on a train.
            .setHandleAudioBecomingNoisy(true)
            .build()

        mediaSession = MediaSession.Builder(this, player)
            .setSessionActivity(openAppIntent())
            .build()
    }

    /** Tapping the notification returns to the app rather than launching a second copy of it. */
    private fun openAppIntent(): PendingIntent {
        val intent = packageManager.getLaunchIntentForPackage(packageName)
            ?.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        return PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? =
        mediaSession

    override fun onTaskRemoved(rootIntent: Intent?) {
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
