package dev.lumen.player.remote

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.IconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * The remote-control screen: pair with a `lumen serve` on the LAN, then browse and drive its
 * playback. Entirely separate from [dev.lumen.player.ui.PlayerScreen] — this controls a player
 * running on another machine, not the one on this phone, and conflating the two states would make
 * it unclear at a glance which "now playing" a control on screen actually affects.
 */
@Composable
fun RemoteScreen(
    vm: RemoteViewModel,
    contentPadding: PaddingValues = PaddingValues(0.dp),
    onClose: () -> Unit = {},
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val playback by vm.playback.collectAsStateWithLifecycle()

    Column(
        Modifier
            .fillMaxSize()
            .padding(top = contentPadding.calculateTopPadding())
            .padding(bottom = contentPadding.calculateBottomPadding())
            .padding(16.dp)
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("Remote", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.weight(1f))
            IconButton(onClick = onClose) { Icon(Icons.Filled.Close, contentDescription = "Close") }
        }
        Spacer(Modifier.height(12.dp))

        state.error?.let { message ->
            Card(Modifier.fillMaxWidth().padding(bottom = 12.dp).clickable { vm.dismissError() }) {
                Column(Modifier.padding(12.dp)) {
                    Text(message, style = MaterialTheme.typography.bodySmall)
                    Text("tap to dismiss", style = MaterialTheme.typography.labelSmall)
                }
            }
        }

        when (val c = state.connection) {
            ConnectionState.Disconnected, is ConnectionState.Failed ->
                ConnectForm(state, onConnect = vm::connect, failure = (c as? ConnectionState.Failed)?.reason)
            ConnectionState.Connecting ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        CircularProgressIndicator()
                        Spacer(Modifier.height(8.dp))
                        Text("Connecting to ${state.lastHost}:${state.lastPort}...")
                    }
                }
            ConnectionState.AwaitingPairing -> PairingForm(onSubmit = vm::submitPairingCode)
            ConnectionState.Connected -> ConnectedContent(state, playback, vm)
        }
    }
}

@Composable
private fun ConnectForm(state: RemoteUiState, onConnect: (String, Int) -> Unit, failure: String?) {
    var host by remember { mutableStateOf(state.lastHost) }
    var port by remember { mutableStateOf(state.lastPort) }

    Column {
        Text(
            "Connect to a desktop running “lumen serve” on the same network.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(
            value = host,
            onValueChange = { host = it },
            label = { Text("Host or IP") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = port,
            onValueChange = { port = it.filter(Char::isDigit) },
            label = { Text("Port") },
            singleLine = true,
            keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = KeyboardType.Number
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        failure?.let {
            Spacer(Modifier.height(8.dp))
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
        Spacer(Modifier.height(16.dp))
        Button(
            onClick = { port.toIntOrNull()?.let { onConnect(host.trim(), it) } },
            enabled = host.isNotBlank() && port.toIntOrNull() != null,
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Connect") }
    }
}

@Composable
private fun PairingForm(onSubmit: (String) -> Unit) {
    var code by remember { mutableStateOf("") }
    Column {
        Text(
            "Enter the six-digit pairing code shown in the terminal that started “lumen serve”.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(
            value = code,
            onValueChange = { code = it.filter(Char::isDigit).take(6) },
            label = { Text("Pairing code") },
            singleLine = true,
            keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = KeyboardType.NumberPassword
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(16.dp))
        Button(
            onClick = { onSubmit(code) },
            enabled = code.length == 6,
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Pair") }
    }
}

@Composable
private fun ConnectedContent(state: RemoteUiState, playback: RemoteProtocol.PlaybackState, vm: RemoteViewModel) {
    Column(Modifier.fillMaxSize()) {
        NowPlayingCard(playback, vm)
        Spacer(Modifier.height(12.dp))
        ServerCard(state.health, state.loadingHealth, onRefresh = vm::refreshHealth)
        Spacer(Modifier.height(16.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "Library (${state.library.size})",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            // Two different things, labelled so the difference is visible: "Refresh list" re-fetches
            // what the server already knows; "Rescan server" asks it to re-walk its disk first
            // (the listing then refreshes on its own, off the version bump that produces).
            TextButton(onClick = vm::refreshLibrary) { Text("Refresh list") }
            TextButton(onClick = vm::rescan) { Text("Rescan server") }
        }
        when {
            state.loadingLibrary -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
            state.library.isEmpty() -> Text(
                "No files reported. Try “Rescan server”.",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 8.dp),
            )
            else -> LazyColumn(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                items(state.library, key = { it.path }) { entry ->
                    Card(
                        Modifier.fillMaxWidth().clickable { vm.play(entry) },
                    ) {
                        Text(
                            entry.title,
                            style = MaterialTheme.typography.bodyMedium,
                            maxLines = 2,
                            modifier = Modifier.padding(12.dp),
                        )
                    }
                }
            }
        }
        TextButton(onClick = vm::forget, modifier = Modifier.padding(top = 8.dp)) {
            Text("Disconnect and forget this server")
        }
    }
}

@Composable
private fun NowPlayingCard(playback: RemoteProtocol.PlaybackState, vm: RemoteViewModel) {
    val now = playback.nowPlaying
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            Text(
                now?.title ?: "Nothing playing on the desktop",
                style = MaterialTheme.typography.titleMedium,
                maxLines = 2,
            )
            if (now != null) {
                Spacer(Modifier.height(12.dp))
                // A slider the user drags calls `seek` only once, on release — sending one per pixel
                // of drag would flood the connection and make the desktop's own playback stutter
                // trying to keep up with seeks that are already stale by the time it acts on them.
                var dragPosition by remember(now.path) { mutableStateOf<Float?>(null) }
                val duration = now.durationMs.coerceAtLeast(1)
                Slider(
                    value = (dragPosition ?: now.positionMs.toFloat()) / duration,
                    onValueChange = { dragPosition = it * duration },
                    onValueChangeFinished = {
                        dragPosition?.let { vm.seek(it.toLong()) }
                        dragPosition = null
                    },
                )
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(formatMs(dragPosition?.toLong() ?: now.positionMs), style = MaterialTheme.typography.labelSmall)
                    Text(formatMs(now.durationMs), style = MaterialTheme.typography.labelSmall)
                }
                Spacer(Modifier.height(8.dp))
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = vm::togglePlayPause) {
                        Icon(
                            if (now.paused) Icons.Filled.PlayArrow else Icons.Filled.Pause,
                            contentDescription = if (now.paused) "Play" else "Pause",
                        )
                    }
                    Spacer(Modifier.width(12.dp))
                    Text("Volume", style = MaterialTheme.typography.labelSmall)
                    Slider(
                        value = now.volume / 100f,
                        onValueChange = { vm.setVolume((it * 100).toInt()) },
                        modifier = Modifier.weight(1f).padding(start = 8.dp),
                    )
                }
            }
        }
    }
}

/**
 * The "Server" card of `docs/15-next-generation-engines.md` §D: the five things a headless
 * `lumen serve` cannot otherwise tell this phone about itself, in units a person reads. A card
 * inside the connected layout, as that document specifies, not a screen of its own — it is glanced
 * at on the way to the library list, not visited.
 */
@Composable
private fun ServerCard(health: RemoteProtocol.HealthReport?, loading: Boolean, onRefresh: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Server", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TextButton(onClick = onRefresh, enabled = !loading) { Text("Refresh") }
            }
            when {
                health != null -> {
                    val nowUnixSecs = System.currentTimeMillis() / 1000
                    val lines = listOf(
                        "Player responds in ${formatRoundtrip(health.mpvRoundtripMs)}",
                        "Certificate ${formatCertExpiry(health.tlsCertExpiresInSecs)}",
                        "Library ${formatLastIndexed(health.libraryLastIndexedUnixSecs, nowUnixSecs)}",
                        "Free space ${formatFreeDisk(health.freeDiskBytes)}",
                        formatClientCount(health.pairedClientCount),
                    )
                    for (line in lines) Text(line, style = MaterialTheme.typography.bodySmall)
                }
                loading -> Text("Checking...", style = MaterialTheme.typography.bodySmall)
                else -> Text("No health report yet.", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

/** Seconds in one day, for the day-granularity wording below. Days, not hours or minutes, because
 * the two things this card counts in days — a certificate's remaining life and how long ago the
 * library was indexed — are only actionable at that scale: nobody re-pairs over a cert expiring
 * "in 3 hours 12 minutes" any differently than one expiring "today". */
private const val SECS_PER_DAY = 86_400L

internal fun formatRoundtrip(ms: Long): String = "$ms ms"

/** "expires in N days", "expired N days ago", or "expiry unknown" for the `null` the server sends
 * for a certificate persisted before expiry was tracked. The two directions are worded differently
 * on purpose: a lapsed certificate and one with days left are different situations, which is why
 * the server sends a signed number instead of clamping at zero. */
internal fun formatCertExpiry(secs: Long?): String {
    if (secs == null) return "expiry unknown"
    val days = kotlin.math.abs(secs) / SECS_PER_DAY
    return when {
        secs < 0 && days == 0L -> "expired less than a day ago"
        secs < 0 -> "expired ${plural(days, "day")} ago"
        days == 0L -> "expires within a day"
        else -> "expires in ${plural(days, "day")}"
    }
}

/** "never reindexed" for the `null` the server sends when no index has ever been written for this
 * library — `lumen serve` only ever scans in memory, so this is the honest state for most servers,
 * not an error. Otherwise the age in days, against [nowUnixSecs] passed in rather than read here so
 * the wording is checkable without depending on the clock. */
internal fun formatLastIndexed(unixSecs: Long?, nowUnixSecs: Long): String {
    if (unixSecs == null) return "never reindexed"
    // A timestamp from the future (clock skew between the two machines) is clamped to "today"
    // rather than shown as a negative age nobody could read.
    val days = ((nowUnixSecs - unixSecs).coerceAtLeast(0)) / SECS_PER_DAY
    return if (days == 0L) "reindexed today" else "reindexed ${plural(days, "day")} ago"
}

/** GiB with one decimal — not the library list's `formatSize`, which picks a unit to fit. A media
 * library volume is gigabytes or more by definition, and a fixed unit means the number reads
 * the same way on every visit instead of switching units as space fills up. `null` is the server
 * reporting that the platform call itself failed.
 *
 * Formatted under `Locale.US` explicitly, the same way `ui/DisplayControls.kt` formats its zoom
 * and subtitle scale: Kotlin's bare `String.format` uses the JVM's default locale, so on a device
 * — or a self-hosted CI runner — set to a comma-decimal locale the figure would come out as
 * "1,0 GiB", and the test pinning this wording would fail there and nowhere else. A fixed-format
 * figure a test asserts on has to be formatted the same way everywhere. */
internal fun formatFreeDisk(bytes: Long?): String {
    if (bytes == null) return "unknown"
    return String.format(java.util.Locale.US, "%.1f GiB", bytes / (1024.0 * 1024 * 1024))
}

internal fun formatClientCount(count: Long): String =
    "${plural(count, "client")} connected"

private fun plural(n: Long, unit: String): String = if (n == 1L) "1 $unit" else "$n ${unit}s"

/** `h:mm:ss`, or `m:ss` under an hour — the same convention the local player's duration text uses,
 * so a number on this screen reads the same way as one on the other. */
private fun formatMs(ms: Long): String {
    val total = (ms / 1000).coerceAtLeast(0)
    val h = total / 3600
    val m = (total % 3600) / 60
    val s = total % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
}
