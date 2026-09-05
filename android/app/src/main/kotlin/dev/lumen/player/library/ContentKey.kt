package dev.lumen.player.library

import android.content.Context
import android.net.Uri
import java.io.FileInputStream
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * A content-derived identity for a media file.
 *
 * Mirrors the sampling of the desktop's `lumen-identity`: the exact byte length mixed in first, then
 * up to a megabyte each from the head, the middle and the tail. Head catches the container header,
 * middle catches payload, tail catches the index or `moov` that many muxers write last. Sampling all
 * three makes a same-length different-content collision implausible without reading whole files.
 *
 * **Not a cryptographic identity and must never be used as one.** It answers "is this the same file
 * I saw before, possibly at a different path?", a question with no adversary.
 *
 * The digest here is SHA-256 rather than the desktop's xxh3, because one is in the Java standard
 * library and the other would have to be reimplemented bit-exactly to agree. The consequence is
 * worth stating plainly: keys from this build and keys from the desktop do **not** match, so watch
 * state does not yet follow a file between the two. Making it do so is a decision about which side
 * adopts the other's digest, and it belongs with the sync work rather than ahead of it. Within one
 * device this is exactly as move-proof as the desktop's.
 */
object ContentKey {

    /** Bytes read from each of the three regions. */
    const val CHUNK = 1024 * 1024L

    /** At or below this, the regions would overlap, so the file is read whole instead. */
    const val FULL_READ_THRESHOLD = 3 * CHUNK

    /**
     * Compute the key, or null if the file cannot be opened.
     *
     * Null rather than an exception: a file that vanished between the library listing and the tap
     * that played it is an ordinary event on a phone, and it must not take the playback down with it.
     */
    suspend fun of(context: Context, uri: Uri, size: Long): String? = withContext(Dispatchers.IO) {
        runCatching { compute(context, uri, size) }.getOrNull()
    }

    private fun compute(context: Context, uri: Uri, size: Long): String? {
        val digest = MessageDigest.getInstance("SHA-256")
        // Length first, so two files sharing their sampled regions but differing in size can never
        // collide — the common case for truncated copies and padded container rewrites.
        digest.update(longToBytes(size))

        context.contentResolver.openFileDescriptor(uri, "r")?.use { pfd ->
            FileInputStream(pfd.fileDescriptor).use { stream ->
                val channel = stream.channel
                val buf = ByteArray(CHUNK.toInt())
                val offsets = if (size <= FULL_READ_THRESHOLD) {
                    // Read it whole, in chunk-sized steps.
                    generateSequence(0L) { it + CHUNK }.takeWhile { it < size }.toList()
                } else {
                    listOf(0L, size / 2 - CHUNK / 2, size - CHUNK)
                }
                for (off in offsets) {
                    channel.position(off)
                    val want = minOf(CHUNK, size - off).toInt()
                    val n = readFully(stream, buf, want)
                    if (n > 0) digest.update(buf, 0, n)
                }
            }
        } ?: return null

        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    /**
     * `read` may return fewer bytes than asked for reasons other than end of file, especially over a
     * content provider backed by something remote. Looping is what makes the key deterministic.
     */
    private fun readFully(stream: FileInputStream, buf: ByteArray, want: Int): Int {
        var filled = 0
        while (filled < want) {
            val n = stream.read(buf, filled, want - filled)
            if (n <= 0) break
            filled += n
        }
        return filled
    }

    internal fun longToBytes(v: Long): ByteArray =
        ByteArray(8) { i -> ((v shr (i * 8)) and 0xFF).toByte() }

    /**
     * The regions that would be sampled for a file of this size.
     *
     * Split out as a pure function so the sampling — the part that decides whether two files agree —
     * is testable without a device, a content provider or a file.
     */
    fun sampleOffsets(size: Long): List<Long> =
        if (size <= FULL_READ_THRESHOLD) {
            generateSequence(0L) { it + CHUNK }.takeWhile { it < size }.toList()
        } else {
            listOf(0L, size / 2 - CHUNK / 2, size - CHUNK)
        }

    /** Bytes actually read for a file of this size — what the cost of a key is, before paying it. */
    fun readCost(size: Long): Long = if (size <= FULL_READ_THRESHOLD) size else 3 * CHUNK
}
