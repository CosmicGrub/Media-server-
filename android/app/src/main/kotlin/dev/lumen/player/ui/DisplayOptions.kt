package dev.lumen.player.ui

import android.content.Context
import android.content.SharedPreferences
import android.content.pm.ActivityInfo
import androidx.media3.ui.AspectRatioFrameLayout
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * How the window is divided between video and everything else.
 *
 * The first version of this screen had no such concept: every layout hardcoded the video and the
 * library side by side, so the library was permanently on screen and permanently taking room the
 * video wanted. On a device that rotates and folds into four different window shapes, "you get 62%"
 * is not a layout — it is a guess that is wrong most of the time.
 */
enum class ViewMode {
    /** Video and library together. */
    Split,

    /** Library hidden; the video has the whole app window. System bars still visible. */
    Theater,

    /** Theater, plus the status and navigation bars hidden. Nothing on screen but the film. */
    Immersive;

    /** What the one-button toggle does. */
    fun next(): ViewMode = when (this) {
        Split -> Theater
        Theater -> Immersive
        Immersive -> Split
    }

    /** Whether the library list is on screen in this mode. */
    val showsLibrary: Boolean get() = this == Split

    /** Whether the system bars are hidden. */
    val hidesSystemBars: Boolean get() = this == Immersive
}

/**
 * How the picture is fitted into the space it has been given.
 *
 * These map onto Media3's resize modes rather than reimplementing the arithmetic, because
 * `PlayerView` applies them to the video surface itself — doing it in Compose would scale the
 * subtitle and control overlays along with the picture.
 */
enum class VideoFit(val label: String, val detail: String) {
    /** Whole picture visible, black bars where the shapes disagree. The safe default. */
    Fit("Fit", "Whole picture, bars where needed"),

    /** Fills the space, cropping the overflow. Aspect preserved — this is not a stretch. */
    Crop("Crop", "Fills the screen, cuts the edges"),

    /** Fills the space by distorting. Ugly, occasionally the only way to use a bad encode. */
    Stretch("Stretch", "Fills the screen, distorts the picture"),

    /** Match the available width, take whatever height the aspect asks for. */
    FitWidth("Fit width", "Match the width, overflow the height"),

    /** Match the available height, take whatever width the aspect asks for. */
    FitHeight("Fit height", "Match the height, overflow the width");

    /** The Media3 constant. Kept in one place so no call site guesses. */
    @androidx.media3.common.util.UnstableApi
    fun resizeMode(): Int = when (this) {
        Fit -> AspectRatioFrameLayout.RESIZE_MODE_FIT
        Crop -> AspectRatioFrameLayout.RESIZE_MODE_ZOOM
        Stretch -> AspectRatioFrameLayout.RESIZE_MODE_FILL
        FitWidth -> AspectRatioFrameLayout.RESIZE_MODE_FIXED_WIDTH
        FitHeight -> AspectRatioFrameLayout.RESIZE_MODE_FIXED_HEIGHT
    }

    fun next(): VideoFit = entries[(ordinal + 1) % entries.size]
}

/**
 * An aspect ratio forced onto the picture, overriding what the file declares.
 *
 * Needed more often than it should be. Anamorphic DVD rips, mislabelled `.avi` files and captures
 * with a wrong pixel-aspect flag all decode to the correct pixels with the wrong shape, and no
 * amount of fitting fixes a ratio the container is lying about.
 */
enum class AspectOverride(val label: String, val ratio: Float?) {
    /** Whatever the file says. */
    Source("Source", null),
    W16H9("16:9", 16f / 9f),
    W4H3("4:3", 4f / 3f),
    W219H100("2.39:1", 2.39f),
    W185H100("1.85:1", 1.85f),
    W21H9("21:9", 21f / 9f),
    W1H1("1:1", 1f);

    fun next(): AspectOverride = entries[(ordinal + 1) % entries.size]
}

/**
 * Whether the window follows the device or stays put.
 *
 * On a foldable this is not the usual convenience setting. Rotating a Fold 5 while it is half open
 * changes the posture, the window size and the layout all at once, and a film that reflows three
 * times because the device moved on a table is worse than one that simply stays where it was put.
 */
enum class OrientationLock(val label: String) {
    Auto("Auto"),
    Portrait("Portrait"),
    Landscape("Landscape");

    /** The `ActivityInfo` constant to request. */
    fun request(): Int = when (this) {
        Auto -> ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        // `SENSOR_` rather than the plain constants: locking to one rotation of an orientation means
        // the picture ends up upside down as often as not on a device people turn either way.
        Portrait -> ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT
        Landscape -> ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
    }

    fun next(): OrientationLock = entries[(ordinal + 1) % entries.size]
}

/**
 * Everything the user can say about how the picture is presented.
 *
 * A plain data class over primitives so the whole thing round-trips through preferences and unit
 * tests on a JVM with no device in sight.
 */
data class DisplaySettings(
    val viewMode: ViewMode = ViewMode.Split,
    val fit: VideoFit = VideoFit.Fit,
    val aspect: AspectOverride = AspectOverride.Source,
    val orientation: OrientationLock = OrientationLock.Auto,
    /** Share of the window given to the video in Split mode. */
    val splitFraction: Float = DEFAULT_SPLIT,
    /** Free-form pinch zoom applied on top of [fit]. 1.0 is untouched. */
    val zoom: Float = 1f,
    /** Pan offset in pixels, meaningful only while zoomed in. */
    val panX: Float = 0f,
    val panY: Float = 0f,
) {
    /**
     * The split is clamped rather than free. A pane dragged to nothing is a pane the user cannot
     * find again, and on a 23:9 cover screen a video given 95% of the height leaves a list two rows
     * tall — technically obeyed, practically broken.
     */
    fun withSplit(fraction: Float) = copy(splitFraction = fraction.coerceIn(MIN_SPLIT, MAX_SPLIT))

    /**
     * Zoom is clamped for the same reason, and snaps back to exactly 1.0 near it: pinching is
     * imprecise, and 1.02 looks like a rendering bug rather than a setting.
     */
    fun withZoom(factor: Float): DisplaySettings {
        val z = (zoom * factor).coerceIn(MIN_ZOOM, MAX_ZOOM)
        val snapped = if (kotlin.math.abs(z - 1f) < 0.03f) 1f else z
        // Panning is only meaningful while there is overflow to pan into. Zooming back out has to
        // drop the offset or the picture stays stuck off-centre with no way to recover it.
        return if (snapped == 1f) copy(zoom = 1f, panX = 0f, panY = 0f) else copy(zoom = snapped)
    }

    fun withPan(dx: Float, dy: Float): DisplaySettings =
        if (zoom <= 1f) this else copy(panX = panX + dx, panY = panY + dy)

    /** True when anything has been changed from the defaults — what the reset button keys off. */
    fun isModified(): Boolean = this != DisplaySettings(viewMode = viewMode)

    /** Back to defaults, keeping the view mode, which the user set with a different control. */
    fun reset(): DisplaySettings = DisplaySettings(viewMode = viewMode)

    companion object {
        const val DEFAULT_SPLIT = 0.62f
        const val MIN_SPLIT = 0.25f
        const val MAX_SPLIT = 0.85f
        const val MIN_ZOOM = 1f
        const val MAX_ZOOM = 4f
    }
}

/**
 * Which arrangement a window shape wants.
 *
 * A pure function of posture and size so it is testable without a device — the same discipline
 * `FoldState.posture()` follows, and for the same reason: this is the decision most likely to be
 * wrong on a shape nobody happened to try.
 */
enum class Arrangement {
    /** Half open on a surface: video above the crease, everything else below. */
    Tabletop,

    /** Two panes across. Only where the window is big enough for a list to be worth reading. */
    SideBySide,

    /** Two panes stacked. */
    Stacked,

    /** Video only. */
    VideoOnly,
}

/**
 * Choose the arrangement.
 *
 * The rule that matters is the last one. The first version keyed only on `screenWidthDp >= 600`,
 * which put the cover screen into a side-by-side the moment it was turned on its side: a 23:9 window
 * is over 600dp wide in landscape and barely 320dp tall, so the "library pane" was a column of cards
 * two rows high beside a video squeezed to 62% of an already short window. Width alone does not
 * describe a window; a list needs height before it is worth the space it costs.
 */
fun arrangementFor(
    isTabletop: Boolean,
    widthDp: Int,
    heightDp: Int,
    mode: ViewMode,
): Arrangement = when {
    // Tabletop keeps its shape in every mode: the top screen is the video whether or not a library
    // is shown below it. That is the posture's entire reason for existing.
    isTabletop -> Arrangement.Tabletop
    !mode.showsLibrary -> Arrangement.VideoOnly
    widthDp >= 600 && heightDp >= 480 -> Arrangement.SideBySide
    else -> Arrangement.Stacked
}

/**
 * Persisted display settings.
 *
 * `SharedPreferences` rather than DataStore: this is a handful of enums read once and written on a
 * tap, and it saves a dependency plus a coroutine scope for something that is genuinely synchronous.
 *
 * Persistence is the point. Settings that reset on every fold would be worse than no settings — the
 * device changes window shape several times an hour, and each change recreates the composition.
 */
class DisplayOptionsStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private val _settings = MutableStateFlow(load())
    val settings: StateFlow<DisplaySettings> = _settings.asStateFlow()

    fun update(transform: (DisplaySettings) -> DisplaySettings) {
        val next = transform(_settings.value)
        _settings.value = next
        save(next)
    }

    /**
     * Zoom and pan are deliberately not persisted. They are a gesture on one shot of one film, not a
     * preference — restoring a 3x zoom on the next thing the user opens would read as a bug.
     */
    private fun save(s: DisplaySettings) {
        prefs.edit()
            .putString(KEY_MODE, s.viewMode.name)
            .putString(KEY_FIT, s.fit.name)
            .putString(KEY_ASPECT, s.aspect.name)
            .putString(KEY_ORIENTATION, s.orientation.name)
            .putFloat(KEY_SPLIT, s.splitFraction)
            .apply()
    }

    private fun load(): DisplaySettings = DisplaySettings(
        viewMode = prefs.getString(KEY_MODE, null).toEnum(ViewMode.Split),
        fit = prefs.getString(KEY_FIT, null).toEnum(VideoFit.Fit),
        aspect = prefs.getString(KEY_ASPECT, null).toEnum(AspectOverride.Source),
        orientation = prefs.getString(KEY_ORIENTATION, null).toEnum(OrientationLock.Auto),
        splitFraction = prefs.getFloat(KEY_SPLIT, DisplaySettings.DEFAULT_SPLIT)
            .coerceIn(DisplaySettings.MIN_SPLIT, DisplaySettings.MAX_SPLIT),
    )

    private companion object {
        const val PREFS = "lumen.display"
        const val KEY_MODE = "view_mode"
        const val KEY_FIT = "fit"
        const val KEY_ASPECT = "aspect"
        const val KEY_ORIENTATION = "orientation"
        const val KEY_SPLIT = "split"
    }
}

/**
 * Read a stored enum name, falling back rather than throwing.
 *
 * A stored value can outlive the constant that produced it — a downgrade, or a rename between
 * builds. Crashing on launch because a preference file mentions an enum that no longer exists is a
 * spectacularly bad way to lose a user's install.
 */
internal inline fun <reified T : Enum<T>> String?.toEnum(fallback: T): T =
    this?.let { name -> enumValues<T>().firstOrNull { it.name == name } } ?: fallback
