package dev.lumen.player.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement as LayoutArrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BrightnessMedium
import androidx.compose.material.icons.filled.VolumeUp
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
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
 * The whole UI, laid out by fold posture and by what the user asked for.
 *
 * A Fold 5 is several devices — cover screen, inner display, half open on a table, and each of those
 * turned on its side — so the arrangement is chosen rather than fixed. But the arrangement is only
 * half the answer: the first version of this screen put the library beside the video in every one of
 * those shapes with no way to dismiss it, so the video never got the panel it was on. [ViewMode] is
 * the user's half, and it wins.
 */
@UnstableApi
@Composable
fun PlayerScreen(
    vm: PlayerViewModel,
    settings: DisplaySettings,
    onSettingsChange: ((DisplaySettings) -> DisplaySettings) -> Unit,
    contentPadding: PaddingValues = PaddingValues(0.dp),
    /// Re-requests media access, or opens settings when the permission is permanently denied.
    /// Supplied by the host so this screen holds no permission mechanics.
    onRequestAccess: () -> Unit = {},
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val posture by rememberPosture()
    val config = LocalConfiguration.current

    val tabletop = posture as? Posture.Tabletop
    val arrangement = arrangementFor(
        isTabletop = tabletop != null,
        widthDp = config.screenWidthDp,
        heightDp = config.screenHeightDp,
        mode = settings.viewMode,
    )

    var sheetOpen by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    // The hint is a message, not a state: it says what just changed and then gets out of the way.
    LaunchedEffect(hint) {
        if (hint != null) {
            kotlinx.coroutines.delay(1400)
            hint = null
        }
    }

    val video: @Composable (Modifier) -> Unit = { modifier ->
        VideoSurface(
            vm = vm,
            settings = settings,
            onZoom = { factor -> onSettingsChange { it.withZoom(factor) } },
            onPan = { dx, dy -> onSettingsChange { it.withPan(dx, dy) } },
            onCycleFit = {
                onSettingsChange { s -> s.copy(fit = s.fit.next()) }
                hint = "Scaling: ${settings.fit.next().label}"
            },
            modifier = modifier,
        )
    }

    Box(Modifier.fillMaxSize()) {
        when (arrangement) {
            Arrangement.Tabletop -> TabletopLayout(
                vm, state, tabletop!!, settings, contentPadding, onRequestAccess, video
            )
            Arrangement.SideBySide ->
                SideBySideLayout(vm, state, settings, contentPadding, onRequestAccess, video)
            Arrangement.Stacked ->
                StackedLayout(vm, state, settings, contentPadding, onRequestAccess, video)
            Arrangement.VideoOnly -> Box(
                Modifier.fillMaxSize().background(Color.Black),
                contentAlignment = Alignment.Center,
            ) { video(Modifier.fillMaxSize()) }
        }

        // Always in the same corner, in every arrangement and every posture. A control that moves
        // when the device folds is one that has to be found again each time.
        ViewModeButton(
            mode = settings.viewMode,
            onCycle = {
                onSettingsChange { it.copy(viewMode = it.viewMode.next()) }
                hint = settings.viewMode.next().name
            },
            onOpenOptions = { sheetOpen = true },
            modifier = Modifier
                .align(Alignment.TopEnd)
                .padding(top = contentPadding.calculateTopPadding())
                .padding(12.dp),
        )

        hint?.let { GestureHint(it, Modifier.align(Alignment.TopCenter)) }
    }

    if (sheetOpen) {
        DisplayOptionsSheet(
            settings = settings,
            arrangement = arrangement,
            onChange = onSettingsChange,
            onDismiss = { sheetOpen = false },
        )
    }
}

/**
 * Half-open, standing on a surface.
 *
 * The hinge bounds arrive in pixels from `WindowInfoTracker` and have to become dp for Compose.
 * Nothing is drawn across the crease: on a Fold 5 it is a visible, slightly recessed line, and a
 * control placed on it is one that is hard to read and unreliable to press.
 *
 * This arrangement survives every view mode, because the geometry is the point — the top screen is
 * the picture whether or not a library sits below it. In Theater and Immersive the bottom half keeps
 * the now-playing bar and loses the list, which is what makes the posture usable one-handed.
 */
@UnstableApi
@Composable
private fun TabletopLayout(
    vm: PlayerViewModel,
    state: UiState,
    p: Posture.Tabletop,
    settings: DisplaySettings,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
    video: @Composable (Modifier) -> Unit,
) {
    val density = LocalDensity.current
    val topHeightDp = with(density) { p.hingeTopPx.toDp() }
    val hingeHeightDp = with(density) { (p.hingeBottomPx - p.hingeTopPx).toDp() }

    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier.fillMaxWidth().height(topHeightDp).background(Color.Black),
            contentAlignment = Alignment.Center,
        ) { video(Modifier.fillMaxSize()) }
        // The crease itself. Left empty on purpose — see above.
        Spacer(Modifier.fillMaxWidth().height(hingeHeightDp))
        Column(
            Modifier
                .fillMaxSize()
                .padding(bottom = contentPadding.calculateBottomPadding())
                .padding(horizontal = 12.dp)
        ) {
            NowPlayingBar(state, vm)
            if (settings.viewMode.showsLibrary) {
                LibraryList(state, vm, onRequestAccess, Modifier.fillMaxSize())
            }
        }
    }
}

/** Two panes across. The split is the user's, not a constant. */
@UnstableApi
@Composable
private fun SideBySideLayout(
    vm: PlayerViewModel,
    state: UiState,
    settings: DisplaySettings,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
    video: @Composable (Modifier) -> Unit,
) {
    Row(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .weight(settings.splitFraction)
                .fillMaxHeight()
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) { video(Modifier.fillMaxSize()) }
        Column(
            Modifier
                .weight(1f - settings.splitFraction)
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

/**
 * Two panes stacked. The cover screen, and the inner display turned to a shape too short for a list
 * beside the picture.
 *
 * The video box is a share of the height rather than a hardcoded 16:9. The 16:9 box was wrong in
 * both directions at once: a 2.39:1 film was letterboxed inside it *and* the box was letterboxed
 * inside a 23:9 screen, so a scope film played in a band across the middle of a tall black
 * rectangle. A share of the height plus the chosen scaling mode gives one letterbox at most, and the
 * share is adjustable because no single number is right for both a phone screen and a tablet one.
 */
@UnstableApi
@Composable
private fun StackedLayout(
    vm: PlayerViewModel,
    state: UiState,
    settings: DisplaySettings,
    contentPadding: PaddingValues,
    onRequestAccess: () -> Unit,
    video: @Composable (Modifier) -> Unit,
) {
    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .fillMaxWidth()
                .weight(settings.splitFraction)
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) { video(Modifier.fillMaxSize()) }
        Column(
            Modifier
                .fillMaxWidth()
                .weight(1f - settings.splitFraction)
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
 *
 * Two layers of sizing, and they are not interchangeable:
 *
 *  - **Fit and aspect** are applied by `PlayerView` to the video surface itself, so the subtitle and
 *    control overlays stay unscaled and legible. Scaling those with the picture is how subtitles end
 *    up cropped off the side of a zoomed frame.
 *  - **Pinch zoom and pan** are a Compose `graphicsLayer` on top, because Media3 has no such concept.
 *    `clipToBounds` keeps a zoomed picture inside its pane instead of drawing over the library.
 */
@UnstableApi
@Composable
private fun VideoSurface(
    vm: PlayerViewModel,
    settings: DisplaySettings,
    onZoom: (Float) -> Unit,
    onPan: (Float, Float) -> Unit,
    onCycleFit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // Null until the playback service has been reached. `PlayerView` accepts that and shows its
    // placeholder, which is the honest thing to draw while there is genuinely no player yet.
    val player by vm.player.collectAsStateWithLifecycle()
    val levels = rememberScreenLevels()
    var feedback by remember { mutableStateOf<GestureFeedback?>(null) }
    // Where the scrub started. Read once per gesture: reading the live position on every frame would
    // make the target chase the seek that the gesture itself is causing.
    var scrubFrom by remember { mutableLongStateOf(0L) }
    var scrubTo by remember { mutableLongStateOf(0L) }

    LaunchedEffect(feedback) {
        if (feedback != null) {
            kotlinx.coroutines.delay(900)
            feedback = null
        }
    }

    Box(
        modifier
            .clipToBounds()
            .pointerInput(player) {
                val width = size.width.toFloat()
                val height = size.height.toFloat()
                playerDragGestures(
                    isZoomed = { settings.zoom > 1f },
                    onZoom = onZoom,
                    onPan = onPan,
                    onDragStart = { _, axis ->
                        if (axis == DragAxis.Horizontal) {
                            scrubFrom = player?.currentPosition ?: 0L
                            scrubTo = scrubFrom
                        }
                    },
                    onDrag = { zone, axis, dx, dy ->
                        when (axis) {
                            DragAxis.Horizontal -> {
                                scrubTo = PlayerGestures.scrubTarget(
                                    scrubTo, dx, width, player?.duration ?: 0L
                                )
                                feedback = GestureFeedback.Seek(scrubTo, scrubTo - scrubFrom)
                            }
                            DragAxis.Vertical -> {
                                val delta = PlayerGestures.levelDelta(dy, height)
                                feedback = if (zone == GestureZone.Left) {
                                    GestureFeedback.Brightness(levels.nudgeBrightness(delta))
                                } else {
                                    GestureFeedback.Volume(levels.nudgeVolume(delta))
                                }
                            }
                            DragAxis.Undecided -> {}
                        }
                    },
                    // The seek happens once, at the end. Seeking on every frame of the drag makes a
                    // decoder re-key dozens of times a second and the picture stutters into place
                    // rather than following the finger.
                    onDragEnd = { axis ->
                        if (axis == DragAxis.Horizontal) player?.seekTo(scrubTo)
                    },
                )
            }
            // Taps stay in their own `pointerInput`. Tap detection gives up the moment movement
            // passes the touch slop, so it cannot compete with the drag detector above — which is
            // exactly why these two can coexist when two drag detectors could not.
            .pointerInput(player) {
                val width = size.width.toFloat()
                detectTapGestures(
                    onDoubleTap = { offset ->
                        val p = player ?: return@detectTapGestures
                        val zone = PlayerGestures.zoneFor(offset.x, width)
                        val target =
                            PlayerGestures.doubleTapSeekMs(zone, p.currentPosition, p.duration)
                        if (target == null) {
                            vm.togglePlayPause()
                        } else {
                            p.seekTo(target)
                            feedback = GestureFeedback.Seek(target, target - p.currentPosition)
                        }
                    },
                )
            }
    ) {
        AndroidView(
            modifier = Modifier
                .fillMaxSize()
                .graphicsLayer {
                    scaleX = settings.zoom
                    scaleY = settings.zoom
                    translationX = settings.panX
                    translationY = settings.panY
                },
            factory = { ctx ->
                PlayerView(ctx).apply {
                    useController = true
                    controllerAutoShow = true
                    setShowNextButton(false)
                    setShowPreviousButton(false)
                }
            },
            update = { view ->
                view.player = player
                view.resizeMode = settings.fit.resizeMode()
                // The forced ratio goes on `PlayerView`'s own content frame — Media3's supported
                // mechanism for this — rather than on a Compose wrapper, so the picture is reshaped
                // without reshaping the subtitle and control overlays drawn on top of it.
                //
                // A non-positive value is how `AspectRatioFrameLayout` is told to go back to the
                // video's own ratio; there is no separate clear call, which is what `Source` maps to.
                view.findViewById<androidx.media3.ui.AspectRatioFrameLayout>(
                    androidx.media3.ui.R.id.exo_content_frame
                )?.setAspectRatio(settings.aspect.ratio ?: 0f)
            },
            onRelease = { view -> view.player = null },
        )

        feedback?.let { GestureFeedbackOverlay(it, Modifier.align(Alignment.Center)) }
    }
}

/**
 * What a drag is doing, drawn over the picture while it happens.
 *
 * A gesture with no feedback is indistinguishable from one that missed — the whole reason a phone
 * player shows *something* the instant a thumb touches the screen, before the effect it is having is
 * otherwise visible at all.
 */
@Composable
private fun GestureFeedbackOverlay(feedback: GestureFeedback, modifier: Modifier = Modifier) {
    Card(
        modifier.padding(24.dp),
        colors = androidx.compose.material3.CardDefaults.cardColors(
            containerColor = Color.Black.copy(alpha = 0.65f)
        ),
    ) {
        Column(
            Modifier.padding(horizontal = 20.dp, vertical = 14.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            when (feedback) {
                is GestureFeedback.Seek -> {
                    val sign = if (feedback.deltaMs >= 0) "+" else "-"
                    Text(
                        PlayerViewModel.formatDuration(feedback.targetMs),
                        color = Color.White,
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    Text(
                        "$sign${PlayerViewModel.formatDuration(kotlin.math.abs(feedback.deltaMs))}",
                        color = Color.White.copy(alpha = 0.8f),
                        style = MaterialTheme.typography.labelLarge,
                    )
                }
                is GestureFeedback.Brightness ->
                    LevelRow(Icons.Filled.BrightnessMedium, feedback.level)
                is GestureFeedback.Volume ->
                    LevelRow(Icons.Filled.VolumeUp, feedback.level)
            }
        }
    }
}

@Composable
private fun LevelRow(icon: androidx.compose.ui.graphics.vector.ImageVector, level: Float) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null, tint = Color.White, modifier = Modifier.size(20.dp))
        Spacer(Modifier.size(10.dp))
        LinearProgressIndicator(
            progress = { level.coerceIn(0f, 1f) },
            modifier = Modifier.size(width = 120.dp, height = 6.dp),
            color = Color.White,
            trackColor = Color.White.copy(alpha = 0.25f),
        )
    }
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
        // "Resumed from 42:10". A player that silently starts a film forty minutes in is
        // indistinguishable from one that lost your place and picked a random spot; saying so is
        // what turns it from a surprise into a feature, and tapping starts from the beginning.
        state.notice?.let { message ->
            Card(Modifier.fillMaxWidth().padding(top = 8.dp).clickable {
                vm.clearResumePoint()
                vm.dismissNotice()
            }) {
                Column(Modifier.padding(12.dp)) {
                    Text(message, style = MaterialTheme.typography.bodySmall)
                    Text("tap to start from the beginning", style = MaterialTheme.typography.labelSmall)
                }
            }
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

        else -> LazyColumn(modifier, verticalArrangement = LayoutArrangement.spacedBy(4.dp)) {
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
