package dev.lumen.player.library

import android.content.ContentUris
import android.content.Context
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * One playable item found on the device.
 *
 * [title] is what MediaStore reports, which is usually the filename. Parsing it into a real title,
 * year and episode is the job of the `lumen-match` crate on the desktop side; this build shows the
 * raw name rather than a half-implemented parse that would disagree with it.
 */
data class MediaItem(
    val id: Long,
    val uri: Uri,
    val title: String,
    val displayName: String,
    val durationMs: Long,
    val sizeBytes: Long,
    val width: Int,
    val height: Int,
    val mimeType: String?,
    val relativePath: String?,
) {
    val isHd: Boolean get() = height >= 720
    val is4k: Boolean get() = height >= 2160

    /** Resolution as text, or null when MediaStore did not record it. */
    fun resolution(): String? = if (width > 0 && height > 0) "${width}x$height" else null

    fun durationText(): String {
        if (durationMs <= 0) return "—"
        val total = durationMs / 1000
        val h = total / 3600
        val m = (total % 3600) / 60
        val s = total % 60
        return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
    }

    fun sizeText(): String {
        val units = listOf("B", "KB", "MB", "GB", "TB")
        var v = sizeBytes.toDouble()
        var i = 0
        while (v >= 1024 && i < units.lastIndex) {
            v /= 1024
            i++
        }
        return if (i == 0) "$sizeBytes B" else "%.1f %s".format(v, units[i])
    }
}

/**
 * Reads the device's video library through MediaStore.
 *
 * MediaStore rather than a filesystem walk: from Android 10 onward scoped storage means an app
 * cannot read arbitrary paths, and the directory walk that works on the desktop simply returns
 * nothing here. MediaStore is also already indexed, so a large library lists immediately instead of
 * stat-ing thousands of files.
 */
object MediaLibrary {

    private val PROJECTION = arrayOf(
        MediaStore.Video.Media._ID,
        MediaStore.Video.Media.TITLE,
        MediaStore.Video.Media.DISPLAY_NAME,
        MediaStore.Video.Media.DURATION,
        MediaStore.Video.Media.SIZE,
        MediaStore.Video.Media.WIDTH,
        MediaStore.Video.Media.HEIGHT,
        MediaStore.Video.Media.MIME_TYPE,
        MediaStore.Video.Media.RELATIVE_PATH,
    )

    suspend fun loadVideos(context: Context): List<MediaItem> = withContext(Dispatchers.IO) {
        // RELATIVE_PATH arrived in API 29. Querying it on an older device throws rather than
        // returning null, so the column list has to shrink to match.
        val projection =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) PROJECTION
            else PROJECTION.filterNot { it == MediaStore.Video.Media.RELATIVE_PATH }.toTypedArray()

        val collection =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL)
            } else {
                @Suppress("DEPRECATION")
                MediaStore.Video.Media.EXTERNAL_CONTENT_URI
            }

        val out = mutableListOf<MediaItem>()
        context.contentResolver.query(
            collection,
            projection,
            null,
            null,
            "${MediaStore.Video.Media.DATE_ADDED} DESC",
        )?.use { cursor ->
            val idCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media._ID)
            val titleCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.TITLE)
            val nameCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.DISPLAY_NAME)
            val durCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.DURATION)
            val sizeCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.SIZE)
            val wCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.WIDTH)
            val hCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.HEIGHT)
            val mimeCol = cursor.getColumnIndexOrThrow(MediaStore.Video.Media.MIME_TYPE)
            // Not `OrThrow`: this column is absent below API 29 and its absence is expected.
            val pathCol = cursor.getColumnIndex(MediaStore.Video.Media.RELATIVE_PATH)

            while (cursor.moveToNext()) {
                val id = cursor.getLong(idCol)
                out += MediaItem(
                    id = id,
                    uri = ContentUris.withAppendedId(collection, id),
                    title = cursor.getString(titleCol) ?: cursor.getString(nameCol) ?: "Untitled",
                    displayName = cursor.getString(nameCol) ?: "",
                    durationMs = cursor.getLong(durCol),
                    sizeBytes = cursor.getLong(sizeCol),
                    width = cursor.getInt(wCol),
                    height = cursor.getInt(hCol),
                    mimeType = cursor.getString(mimeCol),
                    relativePath = if (pathCol >= 0) cursor.getString(pathCol) else null,
                )
            }
        }
        out
    }
}
