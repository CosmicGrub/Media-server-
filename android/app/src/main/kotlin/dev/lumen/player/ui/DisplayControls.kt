package dev.lumen.player.ui

import androidx.compose.foundation.layout.Arrangement as LayoutArrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.FullscreenExit
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.ViewAgenda
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The one button.
 *
 * Deliberately a single control that cycles Split → Theater → Immersive rather than three separate
 * ones. The complaint that produced this file was that the library pane could not be got rid of; the
 * fix for that has to be reachable without first finding a settings sheet, and it has to be in the
 * same place in every window shape so it can be hit by muscle memory on a device that keeps changing
 * shape underneath it.
 *
 * Long-press opens everything else.
 */
@Composable
fun ViewModeButton(
    mode: ViewMode,
    onCycle: () -> Unit,
    onOpenOptions: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val (icon, label) = when (mode) {
        ViewMode.Split -> Icons.Filled.Fullscreen to "Hide the library and fill the window"
        ViewMode.Theater -> Icons.Filled.Fullscreen to "Go fullscreen, hiding the system bars"
        ViewMode.Immersive -> Icons.Filled.FullscreenExit to "Show the library again"
    }
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        SmallFloatingActionButton(
            onClick = onCycle,
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.75f),
            modifier = Modifier.semantics { contentDescription = label },
        ) {
            Icon(icon, contentDescription = null)
        }
        Spacer(Modifier.size(8.dp))
        SmallFloatingActionButton(
            onClick = onOpenOptions,
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.75f),
            modifier = Modifier.semantics { contentDescription = "Display options" },
        ) {
            Icon(Icons.Filled.Tune, contentDescription = null)
        }
    }
}

/**
 * Every remaining option, in one sheet.
 *
 * A sheet rather than a settings screen because these are adjustments you make *while watching* —
 * the answer to "why is this stretched" has to be one tap from the picture, with the picture still
 * on screen to judge the change against.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun DisplayOptionsSheet(
    settings: DisplaySettings,
    arrangement: Arrangement,
    onChange: ((DisplaySettings) -> DisplaySettings) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp)
                .padding(bottom = 32.dp),
            verticalArrangement = LayoutArrangement.spacedBy(4.dp),
        ) {
            SectionHeader("Layout")
            ChipRow(
                options = ViewMode.entries,
                selected = settings.viewMode,
                label = { it.name },
                onSelect = { m -> onChange { it.copy(viewMode = m) } },
            )

            // Only meaningful when there are in fact two panes to divide. Shown greyed rather than
            // hidden, because a control that vanishes as the device folds is one the user stops
            // believing exists.
            val splitApplies = arrangement != Arrangement.VideoOnly
            SectionHeader(
                if (splitApplies) "Video share of the window" else "Video share (Split mode only)"
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Slider(
                    value = settings.splitFraction,
                    onValueChange = { v -> onChange { it.withSplit(v) } },
                    valueRange = DisplaySettings.MIN_SPLIT..DisplaySettings.MAX_SPLIT,
                    enabled = splitApplies,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.size(12.dp))
                Text(
                    "${(settings.splitFraction * 100).toInt()}%",
                    style = MaterialTheme.typography.labelLarge,
                )
            }

            SectionHeader("Scaling")
            ChipRow(
                options = VideoFit.entries,
                selected = settings.fit,
                label = { it.label },
                onSelect = { f -> onChange { it.copy(fit = f) } },
            )
            Text(settings.fit.detail, style = MaterialTheme.typography.bodySmall)

            SectionHeader("Aspect ratio")
            ChipRow(
                options = AspectOverride.entries,
                selected = settings.aspect,
                label = { it.label },
                onSelect = { a -> onChange { it.copy(aspect = a) } },
            )
            if (settings.aspect != AspectOverride.Source) {
                Text(
                    "Overriding what the file declares. Use this when a rip has the wrong shape, " +
                        "not to fill the screen — Crop does that without distorting.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            SectionHeader("Rotation")
            ChipRow(
                options = OrientationLock.entries,
                selected = settings.orientation,
                label = { it.label },
                onSelect = { o -> onChange { it.copy(orientation = o) } },
            )

            SectionHeader("Zoom")
            Row(verticalAlignment = Alignment.CenterVertically) {
                Slider(
                    value = settings.zoom,
                    onValueChange = { v ->
                        // Set, not multiply: the slider reports an absolute position, while
                        // `withZoom` takes a gesture's relative factor.
                        onChange { s -> s.copy(zoom = 1f, panX = 0f, panY = 0f).withZoom(v) }
                    },
                    valueRange = DisplaySettings.MIN_ZOOM..DisplaySettings.MAX_ZOOM,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.size(12.dp))
                Text(
                    String.format(java.util.Locale.US, "%.1f×", settings.zoom),
                    style = MaterialTheme.typography.labelLarge,
                )
            }
            Text(
                "Pinch on the video to zoom, drag to pan, double-tap to step through the scaling " +
                    "modes.",
                style = MaterialTheme.typography.bodySmall,
            )

            Spacer(Modifier.height(12.dp))
            Row(horizontalArrangement = LayoutArrangement.spacedBy(8.dp)) {
                TextButton(onClick = { onChange { it.reset() } }, enabled = settings.isModified()) {
                    Text("Reset to defaults")
                }
                TextButton(onClick = onDismiss) { Text("Done") }
            }
        }
    }
}

@Composable
private fun SectionHeader(text: String) {
    Spacer(Modifier.height(12.dp))
    Text(text, style = MaterialTheme.typography.titleSmall)
}

/**
 * A wrapping row of single-choice chips.
 *
 * `FlowRow` rather than a `Row`: the aspect-ratio list is seven chips wide and the cover screen is
 * not, and a control that runs off the side of the narrowest screen the device has is no control.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun <T> ChipRow(
    options: List<T>,
    selected: T,
    label: (T) -> String,
    onSelect: (T) -> Unit,
) {
    FlowRow(horizontalArrangement = LayoutArrangement.spacedBy(8.dp)) {
        options.forEach { option ->
            FilterChip(
                selected = option == selected,
                onClick = { onSelect(option) },
                label = { Text(label(option)) },
            )
        }
    }
}

/**
 * The transient message shown when a gesture changes something.
 *
 * A double-tap that silently switches scaling mode is indistinguishable from a rendering glitch.
 * Naming the mode is what turns the gesture into a control the user can learn.
 */
@Composable
fun GestureHint(text: String, modifier: Modifier = Modifier) {
    Box(
        modifier
            .padding(12.dp)
            .semantics { contentDescription = text }
    ) {
        AssistChip(
            onClick = {},
            label = { Text(text) },
            colors = AssistChipDefaults.assistChipColors(
                containerColor = Color.Black.copy(alpha = 0.6f),
                labelColor = Color.White,
            ),
            leadingIcon = { Icon(Icons.Filled.ViewAgenda, contentDescription = null) },
        )
    }
}
