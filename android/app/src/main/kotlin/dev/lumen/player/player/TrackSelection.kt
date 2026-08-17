package dev.lumen.player.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi

/**
 * One selectable audio or subtitle track, described the way a person reads it rather than the way
 * the demuxer names it.
 *
 * This is the phone's equivalent of `lumen-playback`'s stream description — a translation of what
 * the platform reported into something a decision (here, a person's tap) can be made against. Kept
 * as a plain data class over primitives for the same reason that one is: the labelling logic is
 * testable without a device, and mislabelling a track is a defect users would find immediately and
 * developers would not, since the wrong label still plays.
 */
data class TrackOption(
    val groupIndex: Int,
    val trackIndex: Int,
    val label: String,
    val isSelected: Boolean,
    /** True when the platform has already ruled this track out — wrong profile, wrong channel
     *  count — and selecting it would silently fail. Shown disabled rather than hidden, because a
     *  track that vanishes from the list looks like a bug in the list, not a fact about the file. */
    val isSupported: Boolean,
)

/** Audio and subtitle choices for the file currently playing, plus "off" for subtitles. */
data class TrackChoices(
    val audio: List<TrackOption>,
    val subtitles: List<TrackOption>,
    val subtitlesOff: Boolean,
)

object TrackSelection {

    /** Build the two lists from what the player currently reports. */
    @UnstableApi
    fun choicesFor(tracks: Tracks): TrackChoices {
        val audio = mutableListOf<TrackOption>()
        val subs = mutableListOf<TrackOption>()
        var anySubtitleSelected = false

        tracks.groups.forEachIndexed { groupIndex, group ->
            for (trackIndex in 0 until group.length) {
                val format = group.getTrackFormat(trackIndex)
                val selected = group.isTrackSelected(trackIndex)
                val supported = group.isTrackSupported(trackIndex)
                when (group.type) {
                    C.TRACK_TYPE_AUDIO -> audio += TrackOption(
                        groupIndex, trackIndex, audioLabel(format), selected, supported
                    )
                    C.TRACK_TYPE_TEXT -> {
                        subs += TrackOption(
                            groupIndex, trackIndex, subtitleLabel(format), selected, supported
                        )
                        if (selected) anySubtitleSelected = true
                    }
                    else -> {}
                }
            }
        }
        return TrackChoices(audio, subs, subtitlesOff = subs.isNotEmpty() && !anySubtitleSelected)
    }

    /**
     * A human label for an audio track.
     *
     * Ordered language, then channel layout, then codec, then a disambiguating index — the order a
     * person actually scans a list of tracks in when picking a dub. A track with no metadata at all
     * still gets a usable label rather than an empty string, because MediaStore's demuxer frequently
     * supplies none of this for local files.
     */
    fun audioLabel(format: Format): String {
        val parts = mutableListOf<String>()
        languageName(format.language)?.let(parts::add)
        format.channelCount.takeIf { it > 0 }?.let { parts += channelLabel(it) }
        codecLabel(format.sampleMimeType)?.let(parts::add)
        if (parts.isEmpty()) {
            parts += "Track ${format.id ?: "?"}"
        }
        return parts.joinToString(" · ")
    }

    fun subtitleLabel(format: Format): String {
        val parts = mutableListOf<String>()
        languageName(format.language)?.let(parts::add)
        format.label?.let { if (it.isNotBlank()) parts += it }
        if (format.roleFlags and C.ROLE_FLAG_CAPTION != 0) parts += "CC"
        if (format.selectionFlags and C.SELECTION_FLAG_FORCED != 0) parts += "Forced"
        if (parts.isEmpty()) {
            parts += "Subtitle ${format.id ?: "?"}"
        }
        return parts.joinToString(" · ")
    }

    /** BCP-47/639 tag to a name worth reading, falling back to the tag itself rather than nothing. */
    fun languageName(tag: String?): String? {
        if (tag.isNullOrBlank() || tag.equals("und", ignoreCase = true)) return null
        return runCatching { java.util.Locale.forLanguageTag(tag) }
            .getOrNull()
            ?.displayLanguage
            ?.takeIf { it.isNotBlank() && !it.equals(tag, ignoreCase = true) }
            ?: tag.uppercase()
    }

    fun channelLabel(channels: Int): String = when (channels) {
        1 -> "Mono"
        2 -> "Stereo"
        6 -> "5.1"
        8 -> "7.1"
        else -> "${channels}ch"
    }

    /** FFmpeg/MediaCodec MIME types to the short name a person recognises. */
    fun codecLabel(mime: String?): String? = when (mime) {
        "audio/ac3" -> "AC-3"
        "audio/eac3" -> "E-AC-3"
        "audio/eac3-joc" -> "E-AC-3 JOC"
        "audio/ac4" -> "AC-4"
        "audio/true-hd" -> "TrueHD"
        "audio/vnd.dts" -> "DTS"
        "audio/vnd.dts.hd" -> "DTS-HD"
        "audio/mp4a-latm" -> "AAC"
        "audio/opus" -> "Opus"
        "audio/vorbis" -> "Vorbis"
        "audio/mpeg" -> "MP3"
        "audio/flac" -> "FLAC"
        "audio/raw" -> "PCM"
        else -> null
    }

    /**
     * The override that selects exactly one track and defers everything else to the platform.
     *
     * `setTrackTypeDisabled` is left alone rather than toggled here on purpose: disabling a whole
     * track type is a different, coarser action ("no subtitles at all") from overriding within one
     * type ("these subtitles, not those"), and conflating them is how a user's explicit "off" gets
     * silently replaced the next time a track of that type becomes available.
     */
    @UnstableApi
    fun overrideFor(tracks: Tracks, groupIndex: Int, trackIndex: Int): TrackSelectionOverride? {
        val group = tracks.groups.getOrNull(groupIndex) ?: return null
        return TrackSelectionOverride(group.mediaTrackGroup, listOf(trackIndex))
    }
}
