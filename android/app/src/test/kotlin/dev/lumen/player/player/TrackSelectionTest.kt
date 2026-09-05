package dev.lumen.player.player

import androidx.media3.common.C
import androidx.media3.common.Format
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Track labels, checked without a device.
 *
 * `Format` is a plain data holder in `media3-common` with no Android framework calls behind its
 * builder, so this runs on the JVM like everything else here. Worth having: a mislabelled track
 * still plays, so nothing else would ever catch the mistake.
 */
class TrackSelectionTest {

    private fun audio(
        language: String? = null,
        channels: Int = 0,
        mime: String? = null,
        id: String? = null,
    ) = Format.Builder()
        .setLanguage(language)
        .setChannelCount(channels)
        .setSampleMimeType(mime)
        .setId(id)
        .build()

    @Test
    fun `an audio label reads language, channels and codec in that order`() {
        val f = audio(language = "en", channels = 6, mime = "audio/true-hd")
        assertEquals("English · 5.1 · TrueHD", TrackSelection.audioLabel(f))
    }

    @Test
    fun `missing metadata falls back to an id rather than an empty label`() {
        // MediaStore's local demuxer very often supplies none of this. A blank row in a track list
        // reads as a bug in the list, not as "the file did not say".
        val f = audio(id = "2")
        assertEquals("Track 2", TrackSelection.audioLabel(f))
    }

    @Test
    fun `an unrecognised language tag still shows something`() {
        assertEquals("XX", TrackSelection.languageName("xx"))
        assertNull("und means unspecified, not a language", TrackSelection.languageName("und"))
        assertNull(TrackSelection.languageName(null))
        assertNull(TrackSelection.languageName(""))
    }

    @Test
    fun `common channel counts get names, others get a count`() {
        assertEquals("Mono", TrackSelection.channelLabel(1))
        assertEquals("Stereo", TrackSelection.channelLabel(2))
        assertEquals("5.1", TrackSelection.channelLabel(6))
        assertEquals("7.1", TrackSelection.channelLabel(8))
        assertEquals("4ch", TrackSelection.channelLabel(4))
    }

    @Test
    fun `lossless codecs are named distinctly from their lossy neighbours`() {
        // The whole reason to show a codec at all: TrueHD and AC-3 are not the same decision.
        assertEquals("TrueHD", TrackSelection.codecLabel("audio/true-hd"))
        assertEquals("DTS-HD", TrackSelection.codecLabel("audio/vnd.dts.hd"))
        assertEquals("DTS", TrackSelection.codecLabel("audio/vnd.dts"))
        assertEquals("AC-3", TrackSelection.codecLabel("audio/ac3"))
        assertEquals("E-AC-3", TrackSelection.codecLabel("audio/eac3"))
        assertNull(TrackSelection.codecLabel("audio/mystery"))
        assertNull(TrackSelection.codecLabel(null))
    }

    @Test
    fun `a subtitle label marks forced and caption tracks`() {
        val forced = Format.Builder()
            .setLanguage("fr")
            .setSelectionFlags(C.SELECTION_FLAG_FORCED)
            .build()
        assertEquals("French · Forced", TrackSelection.subtitleLabel(forced))

        val cc = Format.Builder()
            .setLanguage("en")
            .setRoleFlags(C.ROLE_FLAG_CAPTION)
            .build()
        assertEquals("English · CC", TrackSelection.subtitleLabel(cc))
    }

    @Test
    fun `a subtitle track with a real label uses it over the language alone`() {
        val f = Format.Builder().setLanguage("en").setLabel("Commentary").build()
        assertEquals("English · Commentary", TrackSelection.subtitleLabel(f))
    }
}
