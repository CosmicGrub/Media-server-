package dev.lumen.player.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.common.util.UnstableApi
import androidx.media3.ui.PlayerView
import dev.lumen.player.fold.Posture
import dev.lumen.player.fold.rememberPosture
import dev.lumen.player.library.MediaItem
import dev.lumen.player.player.PlayerViewModel
import dev.lumen.player.player.UiState

/**
 * The whole UI, laid out by fold posture.
 *
 * Three layouts, because a Fold 5 is three devices:
 *
 *  - **Tabletop** (half-open, horizontal hinge): video above the crease, library below it. The
 *    layout that makes a foldable worth targeting — the phone stands on a table and plays hands-free.
 *  - **Wide** (inner display, flat): video and library side by side. The inner screen is nearly
 *    square, so a full-width 16:9 video would leave a third of the display empty.
 *  - **Tall** (cover screen): video on top, library filling the rest. At 23.1:9 there is no room
 *    for anything beside the video.
 */
@UnstableApi
@Composable
fun PlayerScreen(
    vm: PlayerViewModel,
    contentPadding: androidx.compose.foundation.layout.PaddingValues =
        androidx.compose.foundation.layout.PaddingValues(0.dp),
    /// Re-requests media access, or opens settings when the permission is permanently denied.
    /// Supplied by the host so this screen holds no permission mechanics.
    onRequestAccess: () -> Unit = {},
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val posture by rememberPosture()
    val config = LocalConfiguration.current

    // The inner display is about 6:5; the cover screen about 23:9. 600dp is the conventional
    // breakpoint for "this is a tablet-shaped surface", and on a Fold 5 it separates the two
    // displays cleanly.
    val isWide = config.screenWidthDp >= 600

    when (val p = posture) {
        is Posture.Tabletop -> TabletopLayout(vm, state, p, contentPadding, onRequestAccess)
        is Posture.Book -> BookLayout(vm, state, contentPadding, onRequestAccess)
        Posture.Flat ->
            if (isWide) WideLayout(vm, state, contentPadding, onRequestAccess)
            else TallLayout(vm, state, contentPadding, onRequestAccess)
    }
}

/**
 * Half-open, standing on a surface.
 *
 * The hinge bounds arrive in pixels from `WindowInfoTracker` and have to become dp for Compose.
 * Nothing is drawn across the crease: on a Fold 5 it is a visible, slightly recessed line, and a
 * control placed on it is one that is hard to read and unreliable to press.
 */
@UnstableApi
@Composable
private fun TabletopLayout(
    vm: PlayerViewModel,
    state: UiState,
    p: Posture.Tabletop,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
) {
    val density = LocalDensity.current
    val topHeightDp = with(density) { p.hingeTopPx.toDp() }
    val hingeHeightDp = with(density) { (p.hingeBottomPx - p.hingeTopPx).toDp() }

    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .fillMaxWidth()
                .height(topHeightDp)
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) {
            VideoSurface(vm, Modifier.fillMaxSize())
        }
        // The crease itself. Left empty on purpose — see above.
        Spacer(Modifier.fillMaxWidth().height(hingeHeightDp))
        Column(
            Modifier
                .fillMaxSize()
                .padding(bottom = contentPadding.calculateBottomPadding())
                .padding(horizontal = 12.dp)
        ) {
            NowPlayingBar(state, vm)
            LibraryList(state, vm, onRequestAccess, Modifier.fillMaxSize())
        }
    }
}

/** Half-open held like a book: video one side, library the other. */
@UnstableApi
@Composable
private fun BookLayout(
    vm: PlayerViewModel,
    state: UiState,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
) = WideLayout(vm, state, contentPadding, onRequestAccess)

/** Inner display, flat. Nearly square, so the two panes sit side by side. */
@UnstableApi
@Composable
private fun WideLayout(
    vm: PlayerViewModel,
    state: UiState,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
) {
    Row(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .weight(0.62f)
                .fillMaxHeight()
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) {
            VideoSurface(vm, Modifier.fillMaxSize())
        }
        Column(
            Modifier
                .weight(0.38f)
                .fillMaxHeight()
                .padding(top = contentPadding.calculateTopPadding())
                .padding(bottom = contentPadding.calculateBottomPadding())
                .padding(12.dp)
        ) {
            NowPlayingBar(state, vm)
            LibraryList(state, vm, onRequestAccess, Modifier.fillMaxSize())
        }
    }
}

/** Cover screen. Too narrow for anything but a stack. */
@UnstableApi
@Composable
private fun TallLayout(
    vm: PlayerViewModel,
    state: UiState,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
) {
    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .fillMaxWidth()
                // 16:9 rather than fillMaxHeight: on a 23:9 screen a proportional split would leave
                // the video a sliver and the list unusable.
                .aspectRatio(16f / 9f)
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) {
            VideoSurface(vm, Modifier.fillMaxSize())
        }
        Column(
            Modifier
                .fillMaxSize()
                .padding(bottom = contentPadding.calculateBottomPadding())
                .padding(horizontal = 12.dp)
        ) {
            NowPlayingBar(state, vm)
            LibraryList(state, vm, onRequestAccess, Modifier.fillMaxSize())
        }
    }
}

/**
 * Media3's `PlayerView`, embedded in Compose.
 *
 * There is no Compose-native video surface, so this is an `AndroidView`. The `update` block is what
 * keeps it correct across a fold: the composition is re-run with the same player instance, so the
 * surface is re-attached rather than recreated and playback continues through the transition.
 */
@UnstableApi
@Composable
private fun VideoSurface(vm: PlayerViewModel, modifier: Modifier = Modifier) {
    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            PlayerView(ctx).apply {
                useController = true
                controllerAutoShow = true
                setShowNextButton(false)
                setShowPreviousButton(false)
            }
        },
        update = { view -> view.player = vm.player },
        onRelease = { view -> view.player = null },
    )
}

@Composable
private fun NowPlayingBar(state: UiState, vm: PlayerViewModel) {
    val now = state.nowPlaying
    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
        Text(
            text = now?.title ?: "Nothing playing",
            style = MaterialTheme.typography.titleMedium,
            maxLines = 2,
        )
        if (now != null) {
            Text(
                text = listOfNotNull(now.resolution(), now.durationText(), now.sizeText(), now.mimeType)
                    .joinToString("  ·  "),
                style = MaterialTheme.typography.bodySmall,
            )
        }
        state.error?.let { message ->
            Card(Modifier.fillMaxWidth().padding(top = 8.dp).clickable { vm.dismissError() }) {
                Column(Modifier.padding(12.dp)) {
                    Text("Playback failed", style = MaterialTheme.typography.titleSmall)
                    // The real message, not a friendly substitute: which codec or container failed
                    // is the entire diagnostic value of a test run.
                    Text(message, style = MaterialTheme.typography.bodySmall)
                    Text("tap to dismiss", style = MaterialTheme.typography.labelSmall)
                }
            }
        }
        HorizontalDivider(Modifier.padding(top = 8.dp))
    }
}

@Composable
private fun LibraryList(
    state: UiState,
    vm: PlayerViewModel,
    onRequestAccess: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when {
        // `onRequestAccess`, not `vm.refresh()`. Refresh only re-queries MediaStore, which without
        // the permission returns nothing and leaves this screen exactly as it was — a button that
        // appears to do nothing, and no route back into the app for anyone who denied once.
        !state.permissionGranted -> Box(modifier, contentAlignment = Alignment.Center) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text("Media access is needed to list your videos.")
                Spacer(Modifier.height(12.dp))
                Button(onClick = onRequestAccess) { Text("Grant access") }
            }
        }

        state.loading -> Box(modifier, contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }

        state.items.isEmpty() -> Box(modifier, contentAlignment = Alignment.Center) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text("No video found on this device.")
                Spacer(Modifier.height(8.dp))
                Text(
                    "Copy a file into Movies/ or Download/ and pull to refresh.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Spacer(Modifier.height(12.dp))
                Button(onClick = { vm.refresh() }) { Text("Rescan") }
            }
        }

        else -> LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            items(state.items, key = { it.id }) { item ->
                LibraryRow(item, isPlaying = item.id == state.nowPlaying?.id) { vm.play(item) }
            }
        }
    }
}

@Composable
private fun LibraryRow(item: MediaItem, isPlaying: Boolean, onClick: () -> Unit) {
    Card(Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Column(Modifier.padding(12.dp)) {
            Text(
                text = item.displayName.ifEmpty { item.title },
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 2,
            )
            Text(
                text = buildString {
                    append(item.durationText())
                    item.resolution()?.let { append("  ·  ").append(it) }
                    if (item.is4k) append("  ·  4K") else if (item.isHd) append("  ·  HD")
                    append("  ·  ").append(item.sizeText())
                    if (isPlaying) append("  ·  playing")
                },
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
}
