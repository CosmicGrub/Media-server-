package dev.lumen.player.remote

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * One remembered server: enough to reconnect without pairing again.
 *
 * [fingerprint] is the SHA-256 of the TLS certificate this app saw the moment pairing succeeded --
 * see `RemoteTls.kt`. It is what every later reconnect is pinned against; without it, a saved token
 * would still work, but the connection carrying it would trust whatever certificate showed up that
 * time, which is exactly the gap pinning exists to close.
 */
data class SavedServer(val host: String, val port: Int, val token: String, val fingerprint: String)

/**
 * The last `lumen serve` this phone paired with.
 *
 * One server, not a list — this mirrors the desktop side's own current scope: `lumen serve` runs one
 * instance for one library, and a household with more than one desktop server is a real case but not
 * this version's problem to solve. Extending this to several saved servers later is additive (a list
 * instead of one slot); it does not need designing now on the strength of a use case nobody has hit.
 *
 * Backed by `EncryptedSharedPreferences` rather than a plain prefs file: the saved token is a bearer
 * credential that grants durable remote control of a desktop player, and `allowBackup="true"` in the
 * manifest means a plaintext copy would ride along in `adb backup`/full-data backup with no root
 * needed pre-Android 12. Encrypting it here means a backup or a copied prefs file carries only
 * ciphertext keyed to this app's entry in the Android Keystore, which does not itself get backed up.
 */
class RemoteConnectionStore(context: Context) {

    private val prefs: SharedPreferences = try {
        val masterKey = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    } catch (e: Exception) {
        // The Keystore-backed master key can fail on a handful of misbehaving OEM images. Falling
        // back to a plain prefs file keeps pairing working rather than crashing the app outright --
        // worse than the encrypted path, but strictly no worse than what shipped before this class
        // existed. Logged rather than silent, so a real-world occurrence is visible in bug reports.
        Log.w("RemoteConnectionStore", "encrypted prefs unavailable, falling back to plain storage", e)
        context.applicationContext.getSharedPreferences(FALLBACK_PREFS, Context.MODE_PRIVATE)
    }

    fun load(): SavedServer? {
        val host = prefs.getString(KEY_HOST, null) ?: return null
        val port = prefs.getInt(KEY_PORT, -1)
        val token = prefs.getString(KEY_TOKEN, null)
        val fingerprint = prefs.getString(KEY_FINGERPRINT, null)
        // A record from before pinning existed (or one written by a build that failed to capture a
        // fingerprint) has nothing to pin a reconnect against -- treated as absent rather than loaded
        // half-formed, so the caller re-pairs instead of silently reconnecting unpinned.
        if (port <= 0 || token.isNullOrEmpty() || fingerprint.isNullOrEmpty()) return null
        return SavedServer(host, port, token, fingerprint)
    }

    fun save(server: SavedServer) {
        prefs.edit()
            .putString(KEY_HOST, server.host)
            .putInt(KEY_PORT, server.port)
            .putString(KEY_TOKEN, server.token)
            .putString(KEY_FINGERPRINT, server.fingerprint)
            .apply()
    }

    /** Forget the saved token — used when a saved token is refused, so the app does not keep
     * retrying credentials the server itself has told it are no good. */
    fun clear() {
        prefs.edit()
            .remove(KEY_HOST)
            .remove(KEY_PORT)
            .remove(KEY_TOKEN)
            .remove(KEY_FINGERPRINT)
            .apply()
    }

    private companion object {
        const val PREFS = "lumen.remote.enc"
        const val FALLBACK_PREFS = "lumen.remote"
        const val KEY_HOST = "host"
        const val KEY_PORT = "port"
        const val KEY_TOKEN = "token"
        const val KEY_FINGERPRINT = "fingerprint"
    }
}
