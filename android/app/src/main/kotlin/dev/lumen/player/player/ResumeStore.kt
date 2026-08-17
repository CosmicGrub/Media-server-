package dev.lumen.player.player

import android.content.Context
import android.content.SharedPreferences

/**
 * Where you were in each file, and whether it is worth going back there.
 *
 * The universal complaint about every product in this category is "I moved my files and lost my
 * watch state", and it happens because they key user data on path. The desktop side answers that
 * with `lumen-identity`: a content-derived sketch that survives rename, move and remount. This is
 * the same idea on the phone, keyed the same way, so a file that moves keeps its position.
 */
class ResumeStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Where playback got to for this content key, or null if it is unknown or not worth resuming. */
    fun positionFor(key: String, durationMs: Long): Long? {
        val saved = prefs.getLong(posKey(key), -1L)
        if (saved < 0) return null
        return if (shouldResume(saved, durationMs)) saved else null
    }

    fun save(key: String, positionMs: Long, durationMs: Long) {
        // A file watched to the end must not resume at the end — the next time you open it you want
        // it from the top, which is what clearing rather than storing achieves.
        if (isEffectivelyFinished(positionMs, durationMs)) {
            prefs.edit().remove(posKey(key)).apply()
            return
        }
        if (positionMs < MIN_SAVE_MS) return
        prefs.edit().putLong(posKey(key), positionMs).apply()
    }

    fun clear(key: String) {
        prefs.edit().remove(posKey(key)).apply()
    }

    /**
     * The content key for a MediaStore id, if it has already been computed.
     *
     * Computing a sketch reads three megabytes, which is far too slow to do on the tap that starts
     * playback. Caching it against the MediaStore id makes it a once-per-file cost: the first play of
     * a file has nothing to resume anyway, and every play after that is instant.
     */
    fun cachedKey(mediaStoreId: Long): String? = prefs.getString(keyCacheName(mediaStoreId), null)

    fun rememberKey(mediaStoreId: Long, contentKey: String) {
        prefs.edit().putString(keyCacheName(mediaStoreId), contentKey).apply()
    }

    private fun posKey(key: String) = "pos:$key"
    private fun keyCacheName(id: Long) = "key:$id"

    companion object {
        private const val PREFS = "lumen.resume"

        /** Below this, there is nothing to come back to. */
        const val MIN_SAVE_MS = 10_000L

        /** Within this of the end, the file counts as watched. */
        const val END_MARGIN_MS = 90_000L

        /** ...or past this fraction of it, for anything short enough that the margin would swallow it. */
        const val END_FRACTION = 0.97

        /**
         * Should a saved position be offered?
         *
         * Deliberately the same rule as `save` uses to discard, so a position that would be thrown
         * away can never also be resumed to. `durationMs <= 0` means the duration is not yet known —
         * resume anyway rather than losing the position to a metadata gap.
         */
        fun shouldResume(positionMs: Long, durationMs: Long): Boolean =
            positionMs >= MIN_SAVE_MS && !isEffectivelyFinished(positionMs, durationMs)

        /**
         * Has this been watched?
         *
         * Two rules, because one is wrong at each end of the range. A ninety-second margin is right
         * for a feature film and would swallow a three-minute clip whole; a 97% fraction is right for
         * the clip and leaves four minutes of a four-hour film counting as unwatched.
         */
        fun isEffectivelyFinished(positionMs: Long, durationMs: Long): Boolean {
            if (durationMs <= 0) return false
            return positionMs >= durationMs - END_MARGIN_MS ||
                positionMs >= durationMs * END_FRACTION
        }
    }
}
