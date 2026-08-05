package dev.lumen.player.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The display model, checked without a device.
 *
 * Everything here is a pure function over primitives for exactly this reason: the arrangement rule
 * is the decision most likely to be wrong on a window shape nobody happened to try by hand, and a
 * Fold 5 has more of those than any other device.
 */
@androidx.media3.common.util.UnstableApi
class DisplayOptionsTest {

    @Test
    fun `the one button cycles through every mode and returns`() {
        // The complaint this exists to answer was that the library could not be dismissed. Whatever
        // else the button does, repeated presses must always reach a mode with no library, and must
        // always get back to the one with it.
        var mode = ViewMode.Split
        val seen = mutableListOf(mode)
        repeat(ViewMode.entries.size) {
            mode = mode.next()
            seen += mode
        }
        assertEquals(ViewMode.Split, mode)
        assertTrue("a mode without the library must be reachable", seen.any { !it.showsLibrary })
        assertEquals(ViewMode.entries.toSet(), seen.toSet())
    }

    @Test
    fun `only immersive hides the system bars`() {
        assertFalse(ViewMode.Split.hidesSystemBars)
        assertFalse(ViewMode.Theater.hidesSystemBars)
        assertTrue(ViewMode.Immersive.hidesSystemBars)
        // Theater is the middle rung on purpose: fullscreen video with the clock and battery still
        // visible is what most people mean by "make the video bigger".
        assertFalse(ViewMode.Theater.showsLibrary)
    }

    @Test
    fun `a landscape cover screen does not get a side-by-side`() {
        // The bug this encodes: the first rule was `screenWidthDp >= 600`, and a Fold 5 cover screen
        // turned on its side is about 827x322dp — wide enough to pass, far too short for a list. The
        // result was a column of cards two rows high beside a video squeezed into 62% of an already
        // short window.
        assertEquals(
            Arrangement.Stacked,
            arrangementFor(isTabletop = false, widthDp = 827, heightDp = 322, mode = ViewMode.Split),
        )
        // The inner display flat is genuinely big enough for two panes.
        assertEquals(
            Arrangement.SideBySide,
            arrangementFor(isTabletop = false, widthDp = 768, heightDp = 640, mode = ViewMode.Split),
        )
        // Cover screen upright: too narrow either way.
        assertEquals(
            Arrangement.Stacked,
            arrangementFor(isTabletop = false, widthDp = 322, heightDp = 827, mode = ViewMode.Split),
        )
    }

    @Test
    fun `theater and immersive give the video the whole window`() {
        for (mode in listOf(ViewMode.Theater, ViewMode.Immersive)) {
            assertEquals(
                "mode $mode must not leave a library pane on screen",
                Arrangement.VideoOnly,
                arrangementFor(isTabletop = false, widthDp = 768, heightDp = 640, mode = mode),
            )
        }
    }

    @Test
    fun `tabletop keeps its shape in every mode`() {
        // The posture's whole reason for existing is that the top screen is the picture and the
        // bottom one is everything else. Collapsing that to a full-window video when the library is
        // hidden would put the film across the crease, which is the one thing this layout avoids.
        for (mode in ViewMode.entries) {
            assertEquals(
                Arrangement.Tabletop,
                arrangementFor(isTabletop = true, widthDp = 768, heightDp = 640, mode = mode),
            )
        }
    }

    @Test
    fun `every fit maps to a distinct media3 resize mode`() {
        val modes = VideoFit.entries.map { it.resizeMode() }
        assertEquals("two fits mapping to the same mode would be a silently dead control",
            modes.size, modes.toSet().size)
    }

    @Test
    fun `source aspect has no ratio and every override does`() {
        assertEquals(null, AspectOverride.Source.ratio)
        for (a in AspectOverride.entries - AspectOverride.Source) {
            val r = requireNotNull(a.ratio) { "$a must carry a ratio" }
            assertTrue("$a ratio must be positive", r > 0f)
        }
        assertEquals(16f / 9f, AspectOverride.W16H9.ratio!!, 0.0001f)
        assertEquals(2.39f, AspectOverride.W219H100.ratio!!, 0.0001f)
    }

    @Test
    fun `the split is clamped so neither pane can be dragged away`() {
        val s = DisplaySettings()
        assertEquals(DisplaySettings.MAX_SPLIT, s.withSplit(1.5f).splitFraction, 0.0001f)
        assertEquals(DisplaySettings.MIN_SPLIT, s.withSplit(-2f).splitFraction, 0.0001f)
        assertEquals(0.5f, s.withSplit(0.5f).splitFraction, 0.0001f)
        // A pane at zero is a pane the user cannot find again.
        assertTrue(DisplaySettings.MIN_SPLIT > 0f)
        assertTrue(DisplaySettings.MAX_SPLIT < 1f)
    }

    @Test
    fun `subtitle scale is clamped at both ends`() {
        val s = DisplaySettings()
        assertEquals(
            DisplaySettings.MAX_SUBTITLE_SCALE,
            s.withSubtitleScale(10f).subtitleScale,
            0.0001f,
        )
        assertEquals(
            DisplaySettings.MIN_SUBTITLE_SCALE,
            s.withSubtitleScale(0f).subtitleScale,
            0.0001f,
        )
        assertEquals(1.5f, s.withSubtitleScale(1.5f).subtitleScale, 0.0001f)
    }

    @Test
    fun `reset also returns subtitle styling to its defaults`() {
        val fiddled = DisplaySettings()
            .withSubtitleScale(2f)
            .copy(subtitleBackground = false)
        assertTrue(fiddled.isModified())
        val back = fiddled.reset()
        assertEquals(1f, back.subtitleScale, 0.0001f)
        assertTrue(back.subtitleBackground)
    }

    @Test
    fun `zoom snaps back to exactly one and drops the pan with it`() {
        // Pinching is imprecise. A residual 1.02 looks like a rendering bug rather than a setting,
        // and a pan left behind at 1.0 leaves the picture stuck off-centre with no way to recover it.
        val zoomed = DisplaySettings().withZoom(2f).withPan(40f, -25f)
        assertEquals(2f, zoomed.zoom, 0.0001f)
        assertEquals(40f, zoomed.panX, 0.0001f)

        val nearlyBack = zoomed.withZoom(0.51f)
        assertEquals(1f, nearlyBack.zoom, 0.0001f)
        assertEquals(0f, nearlyBack.panX, 0.0001f)
        assertEquals(0f, nearlyBack.panY, 0.0001f)
    }

    @Test
    fun `zoom is bounded at both ends`() {
        var s = DisplaySettings()
        repeat(20) { s = s.withZoom(2f) }
        assertEquals(DisplaySettings.MAX_ZOOM, s.zoom, 0.0001f)
        repeat(20) { s = s.withZoom(0.5f) }
        assertEquals(DisplaySettings.MIN_ZOOM, s.zoom, 0.0001f)
    }

    @Test
    fun `panning does nothing until there is overflow to pan into`() {
        val unzoomed = DisplaySettings().withPan(100f, 100f)
        assertEquals(0f, unzoomed.panX, 0.0001f)
        assertEquals(0f, unzoomed.panY, 0.0001f)
    }

    @Test
    fun `reset clears everything except the mode the user set separately`() {
        val fiddled = DisplaySettings(viewMode = ViewMode.Immersive)
            .copy(fit = VideoFit.Stretch, aspect = AspectOverride.W4H3)
            .withSplit(0.3f)
            .withZoom(2.5f)
        assertTrue(fiddled.isModified())

        val back = fiddled.reset()
        assertEquals(ViewMode.Immersive, back.viewMode)
        assertEquals(VideoFit.Fit, back.fit)
        assertEquals(AspectOverride.Source, back.aspect)
        assertEquals(DisplaySettings.DEFAULT_SPLIT, back.splitFraction, 0.0001f)
        assertEquals(1f, back.zoom, 0.0001f)
        assertFalse(back.isModified())
    }

    @Test
    fun `an unknown stored value falls back instead of crashing`() {
        // A preference file outlives the build that wrote it. Crashing on launch because it names a
        // constant a downgrade no longer has is a spectacular way to lose an install.
        assertEquals(VideoFit.Fit, "NoSuchMode".toEnum(VideoFit.Fit))
        assertEquals(VideoFit.Crop, "Crop".toEnum(VideoFit.Fit))
        assertEquals(ViewMode.Split, (null as String?).toEnum(ViewMode.Split))
    }

    @Test
    fun `rotation locks request a sensor orientation rather than a fixed rotation`() {
        // A device people turn either way, locked to one rotation, shows the picture upside down as
        // often as not.
        assertNotEquals(OrientationLock.Auto.request(), OrientationLock.Portrait.request())
        assertNotEquals(OrientationLock.Portrait.request(), OrientationLock.Landscape.request())
        assertEquals(
            android.content.pm.ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE,
            OrientationLock.Landscape.request(),
        )
        assertEquals(
            android.content.pm.ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT,
            OrientationLock.Portrait.request(),
        )
    }
}
