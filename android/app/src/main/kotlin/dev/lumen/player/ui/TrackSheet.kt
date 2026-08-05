package dev.lumen.player.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Subtitles
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.lumen.player.player.TrackChoices
import dev.lumen.player.player.TrackOption

/**
 * Reach for the picker.
 *
 * Its own button rather than folded into [ViewModeButton] or the display-options sheet: track choice
 * is a decision about *this file*, made and remade during playback, and burying it a level down
 * behind an unrelated icon is exactly how "the player picked the wrong audio" complaints happen —
 * the control exists, nobody finds it.
 */
@Composable
fun TrackPickerButton(onOpen: () -> Unit, modifier: Modifier = Modifier) {
    SmallFloatingActionButton(
        onClick = onOpen,
        containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.75f),
        modifier = modifier.semantics { contentDescription = "Audio and subtitle tracks" },
    ) {
        Icon(Icons.Filled.Subtitles, contentDescription = null)
    }
}

/**
 * Pick the audio and subtitle track for the file currently playing.
 *
 * Two lists rather than a combined one, because the two questions are independent — the reason a
 * forced-subtitle rule exists at all on the desktop side (`lumen-playback`'s track selector) is that
 * "audio in my language" and "subtitles for the foreign dialogue" are two separate preferences that
 * happen to often want different answers on the same file.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TrackSheet(
    choices: TrackChoices,
    subtitlesDisabled: Boolean,
    onSelect: (groupIndex: Int, trackIndex: Int) -> Unit,
    onSubtitlesOff: () -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().padding(bottom = 24.dp)) {
            if (choices.audio.isEmpty() && choices.subtitles.isEmpty()) {
                Text(
                    "No alternate tracks — this file only offers what is already playing.",
                    Modifier.padding(20.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
                return@ModalBottomSheet
            }

            if (choices.audio.isNotEmpty()) {
                SheetHeader("Audio")
                TrackList(choices.audio, onSelect)
            }

            if (choices.subtitles.isNotEmpty()) {
                SheetHeader("Subtitles")
                ListItem(
                    headlineContent = { Text("Off") },
                    leadingContent = {
                        if (subtitlesDisabled) Icon(Icons.Filled.Check, contentDescription = null)
                    },
                    modifier = Modifier.clickableItem(onSubtitlesOff),
                )
                TrackList(choices.subtitles, onSelect, selectedOverride = !subtitlesDisabled)
            }
        }
    }
}

@Composable
private fun SheetHeader(text: String) {
    Text(
        text,
        Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

/**
 * @param selectedOverride when false, no row is drawn as selected even if the platform still marks
 *   one — the "Off" row above already carries the checkmark, and showing two would be a picker
 *   contradicting itself.
 */
@Composable
private fun TrackList(
    options: List<TrackOption>,
    onSelect: (Int, Int) -> Unit,
    selectedOverride: Boolean = true,
) {
    // Not a LazyColumn: this sheet's whole content already scrolls with the ModalBottomSheet, and a
    // few-item list nested inside its own lazy scroller is how a two-track file ends up with a track
    // list two rows tall inside a mostly-empty scrollable box.
    options.forEach { opt ->
        ListItem(
            headlineContent = {
                Text(opt.label, color = if (opt.isSupported) Color.Unspecified else
                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.5f))
            },
            supportingContent = if (!opt.isSupported) {
                { Text("Not supported by this device") }
            } else null,
            leadingContent = {
                if (opt.isSelected && selectedOverride) {
                    Icon(Icons.Filled.Check, contentDescription = null)
                }
            },
            modifier = if (opt.isSupported) {
                Modifier.clickableItem { onSelect(opt.groupIndex, opt.trackIndex) }
            } else {
                Modifier
            },
        )
    }
}

private fun Modifier.clickableItem(onClick: () -> Unit): Modifier = this.clickable(onClick = onClick)
