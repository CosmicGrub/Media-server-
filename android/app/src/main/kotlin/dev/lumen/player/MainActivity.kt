package dev.lumen.player

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.darkColorScheme
import androidx.compose.ui.Modifier
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.media3.common.util.UnstableApi
import dev.lumen.player.player.PlayerViewModel
import dev.lumen.player.ui.PlayerScreen

@UnstableApi
class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Edge to edge, because letterboxing on the inner display is exactly what a foldable build
        // exists to avoid.
        enableEdgeToEdge()

        setContent {
            // Dark by default: this is a video player, and a light chrome around a film is a choice
            // nobody wants in a dark room.
            MaterialTheme(colorScheme = darkColorScheme()) {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    val vm: PlayerViewModel = viewModel()

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

                    Scaffold(Modifier.fillMaxSize()) { insets ->
                        // The video itself goes edge to edge — letterboxing a film to avoid a
                        // status bar defeats the point — so only the surrounding chrome is inset.
                        PlayerScreen(
                            vm = vm,
                            contentPadding = insets,
                            onRequestAccess = onRequestAccess,
                        )
                    }
                }
            }
        }
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
