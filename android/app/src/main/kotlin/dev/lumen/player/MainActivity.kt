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

                    val permission = requiredMediaPermission()
                    val launcher = androidx.activity.compose.rememberLauncherForActivityResult(
                        ActivityResultContracts.RequestPermission()
                    ) { granted -> vm.onPermissionResult(granted) }

                    androidx.compose.runtime.LaunchedEffect(Unit) {
                        val already = ContextCompat.checkSelfPermission(
                            this@MainActivity, permission
                        ) == android.content.pm.PackageManager.PERMISSION_GRANTED
                        if (already) vm.onPermissionResult(true) else launcher.launch(permission)
                    }

                    Scaffold(Modifier.fillMaxSize()) { insets ->
                        // The video itself goes edge to edge — letterboxing a film to avoid a
                        // status bar defeats the point — so only the surrounding chrome is inset.
                        PlayerScreen(vm = vm, contentPadding = insets)
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
