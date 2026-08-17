package dev.lumen.player

import android.Manifest
import android.app.PictureInPictureParams
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.util.Rational
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Cast
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Surface
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.unit.dp
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.common.util.UnstableApi
import dev.lumen.player.player.PipAspectRatio
import dev.lumen.player.player.PlayerViewModel
import dev.lumen.player.remote.RemoteScreen
import dev.lumen.player.remote.RemoteViewModel
import dev.lumen.player.ui.PlayerScreen

@UnstableApi
class MainActivity : ComponentActivity() {

    /**
     * Display settings outlive the composition on purpose.
     *
     * Folding, unfolding and rotating all recreate the composition, and on a Fold 5 that happens
     * several times an hour. A preference stored in composition state would reset every time the
     * device changed shape — which is precisely when the user most wants it to have been remembered.
     */
    private lateinit var displayOptions: dev.lumen.player.ui.DisplayOptionsStore

    /**
     * Obtained directly rather than via Compose's `viewModel()`, so that `onUserLeaveHint` and
     * `onPictureInPictureModeChanged` — plain `Activity` callbacks, not composables — can reach the
     * same instance the screen is showing. `ViewModelProvider` caches by scope regardless of which
     * API asks for it, so this is not a second ViewModel; it is the same one, reached from two places.
     */
    private val vm: PlayerViewModel by lazy {
        ViewModelProvider(
            this,
            ViewModelProvider.AndroidViewModelFactory.getInstance(application),
        )[PlayerViewModel::class.java]
    }

    /** Same pattern as [vm]: obtained via `ViewModelProvider` so it survives configuration changes
     * (fold, rotation) rather than reconnecting to the desktop on every one of them. */
    private val remoteVm: RemoteViewModel by lazy {
        ViewModelProvider(
            this,
            ViewModelProvider.AndroidViewModelFactory.getInstance(application),
        )[RemoteViewModel::class.java]
    }

    /**
     * Whether Picture-in-Picture is currently active, as a Compose state the screen reads to hide
     * everything but the picture — a tiny floating window has no room for a library list, and the
     * system already draws its own play/pause affordance from the media session.
     */
    private val isInPip = mutableStateOf(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        displayOptions = dev.lumen.player.ui.DisplayOptionsStore(this)
        // Edge to edge, because letterboxing on the inner display is exactly what a foldable build
        // exists to avoid.
        enableEdgeToEdge()

        setContent {
            // Dark by default: this is a video player, and a light chrome around a film is a choice
            // nobody wants in a dark room.
            MaterialTheme(colorScheme = darkColorScheme()) {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    val activity = this@MainActivity
                    val permission = requiredMediaPermission()
                    var askedOnce by androidx.compose.runtime.saveable.rememberSaveable {
                        androidx.compose.runtime.mutableStateOf(false)
                    }
                    val launcher = androidx.activity.compose.rememberLauncherForActivityResult(
                        ActivityResultContracts.RequestPermission()
                    ) { granted ->
                        askedOnce = true
                        vm.onPermissionResult(granted)
                    }

                    androidx.compose.runtime.LaunchedEffect(Unit) {
                        val already = ContextCompat.checkSelfPermission(
                            activity, permission
                        ) == android.content.pm.PackageManager.PERMISSION_GRANTED
                        if (already) vm.onPermissionResult(true) else launcher.launch(permission)
                    }

                    // What the "grant access" button does. Branching lives here rather than in the
                    // UI so the screen stays free of permission mechanics.
                    //
                    // The branch that matters is the last one. Once a permission is permanently
                    // denied, `launch()` shows nothing and returns a denial immediately — so a
                    // button wired straight to it looks broken and the user has no way back into
                    // the app. Settings is the only remaining route, and offering it is the
                    // difference between a recoverable state and a dead end.
                    val onRequestAccess: () -> Unit = {
                        val granted = ContextCompat.checkSelfPermission(activity, permission) ==
                            android.content.pm.PackageManager.PERMISSION_GRANTED
                        when {
                            granted -> vm.onPermissionResult(true)
                            !askedOnce || ActivityCompat.shouldShowRequestPermissionRationale(
                                activity,
                                permission,
                            ) -> launcher.launch(permission)
                            else -> activity.startActivity(
                                android.content.Intent(
                                    android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                                    android.net.Uri.fromParts("package", activity.packageName, null),
                                ),
                            )
                        }
                    }

                    val display by displayOptions.settings.collectAsStateWithLifecycle()

                    // Both of these are properties of the window rather than of the composition, so
                    // they are applied here and re-applied whenever the setting changes.
                    androidx.compose.runtime.LaunchedEffect(display.orientation) {
                        activity.requestedOrientation = display.orientation.request()
                    }
                    androidx.compose.runtime.LaunchedEffect(display.viewMode) {
                        setSystemBarsHidden(display.viewMode.hidesSystemBars)
                    }

                    // Which player this screen shows. A plain boolean rather than pulling in
                    // Navigation-Compose for one additional destination — this app has stayed with
                    // ViewModel-held state and a `when` for its whole life, and a second screen is
                    // not the reason to change that.
                    var showRemote by rememberSaveable { mutableStateOf(false) }

                    Scaffold(Modifier.fillMaxSize()) { insets ->
                        // The video itself goes edge to edge — letterboxing a film to avoid a
                        // status bar defeats the point — so only the surrounding chrome is inset.
                        val inPip by isInPip
                        if (showRemote) {
                            RemoteScreen(
                                vm = remoteVm,
                                contentPadding = insets,
                                onClose = { showRemote = false },
                            )
                        } else {
                            Box(Modifier.fillMaxSize()) {
                                PlayerScreen(
                                    vm = vm,
                                    settings = display,
                                    onSettingsChange = displayOptions::update,
                                    contentPadding = insets,
                                    onRequestAccess = onRequestAccess,
                                    isInPip = inPip,
                                )
                                // Opposite corner from PlayerScreen's own view-mode button, and
                                // hidden in PiP for the same reason that button is: a floating
                                // window the size of a playing card has no room for a second
                                // control nobody can read at that size. Not shown while immersive
                                // either — the whole point of that mode is nothing but the picture.
                                if (!inPip && !display.viewMode.hidesSystemBars) {
                                    SmallFloatingActionButton(
                                        onClick = { showRemote = true },
                                        containerColor = MaterialTheme.colorScheme.surfaceVariant
                                            .copy(alpha = 0.75f),
                                        modifier = Modifier
                                            .align(Alignment.BottomEnd)
                                            .padding(bottom = insets.calculateBottomPadding())
                                            .padding(16.dp),
                                    ) {
                                        Icon(Icons.Filled.Cast, contentDescription = "Control a desktop player")
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /**
     * Hide or restore the status and navigation bars.
     *
     * `BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE` is the part that matters: without it, hiding the
     * navigation bar on a gesture-navigation device leaves no way back except the hardware buttons
     * the Fold 5 does not have. With it, a swipe from the edge brings the bars back temporarily and
     * they retreat on their own.
     */
    /**
     * The system is about to leave this Activity — the user pressed Home, or switched apps.
     *
     * This is the documented hook for entering PiP automatically: unlike a button the user has to
     * find, it means a film keeps playing the instant someone does the thing they would naturally do
     * to glance at something else, which is the entire value Picture-in-Picture offers. Only offered
     * while something is actually playing — entering a floating window to look at an idle library
     * list would be a worse experience than simply leaving the app.
     *
     * On API 31+ `setAutoEnterEnabled` on the params below makes this redundant for the swipe-to-
     * home gesture specifically, but `onUserLeaveHint` still fires for other paths (a notification
     * pull-down, an incoming call) and is required at all on API 24–30, where auto-enter does not
     * exist. Kept unconditionally rather than branched by version, since calling it is harmless where
     * the system already handled the transition itself.
     */
    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        if (vm.state.value.isPlaying) enterPip()
    }

    private fun enterPip() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        // Clamped here, not trusted as already-clamped from the ViewModel: `videoSize` is the file's
        // raw pixel dimensions, and an anamorphic scope frame is exactly the shape that would make
        // `Rational` below throw if it reached the platform unclamped.
        val (rawW, rawH) = vm.videoSize.value
        val (w, h) = PipAspectRatio.clamp(rawW, rawH)
        val params = PictureInPictureParams.Builder()
            .setAspectRatio(Rational(w, h))
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) setAutoEnterEnabled(true)
            }
            .build()
        // A device that declares `android:supportsPictureInPicture` but denies it at runtime — a
        // manufacturer policy, a low-RAM mode — must not crash the app over a convenience feature.
        runCatching { enterPictureInPictureMode(params) }
    }

    override fun onPictureInPictureModeChanged(
        isInPictureInPictureMode: Boolean,
        newConfig: Configuration,
    ) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig)
        isInPip.value = isInPictureInPictureMode
    }

    private fun setSystemBarsHidden(hidden: Boolean) {
        val controller = androidx.core.view.WindowCompat.getInsetsController(window, window.decorView)
        controller.systemBarsBehavior =
            androidx.core.view.WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        val bars = androidx.core.view.WindowInsetsCompat.Type.systemBars()
        if (hidden) controller.hide(bars) else controller.show(bars)
    }

    /**
     * The permission that actually governs video access on this device.
     *
     * READ_MEDIA_VIDEO replaced READ_EXTERNAL_STORAGE at API 33. Requesting the wrong one is silently
     * useless: the request dialog never appears, the result is a denial, and the library reads empty
     * with no error to explain it.
     */
    private fun requiredMediaPermission(): String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_VIDEO
        } else {
            @Suppress("DEPRECATION")
            Manifest.permission.READ_EXTERNAL_STORAGE
        }
}
