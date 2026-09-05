package dev.lumen.player.remote

/**
 * The wire protocol spoken by `lumen serve` — see `crates/lumen-play/src/remote/protocol.rs` on the
 * desktop side, which this must match exactly. Newline-delimited JSON over a plain TCP socket, not
 * WebSocket: the desktop side already explains why, and the same reasoning holds here in reverse —
 * reading one JSON object per line costs nothing to depend on, where a WebSocket client would mean
 * either a new library or hand-rolling the HTTP upgrade handshake for a protocol that gains nothing
 * from it on this side either.
 *
 * Every request carries an `id`; every reply echoes it. `RemoteClient` is what matches replies back
 * to the call that is waiting on one — this file only knows how to read and write the shapes, using
 * [RemoteJson] rather than `org.json` for the reason documented there.
 */
object RemoteProtocol {

    /** Client → server. Each `toLine()` produces exactly one line, without the trailing newline. */
    sealed interface ClientMessage {
        fun toLine(id: String): String

        data class Pair(val code: String) : ClientMessage {
            override fun toLine(id: String) =
                """{"type":"pair","id":${RemoteJson.quote(id)},"code":${RemoteJson.quote(code)}}"""
        }

        data class Auth(val token: String) : ClientMessage {
            override fun toLine(id: String) =
                """{"type":"auth","id":${RemoteJson.quote(id)},"token":${RemoteJson.quote(token)}}"""
        }

        data object Library : ClientMessage {
            override fun toLine(id: String) = """{"type":"library","id":${RemoteJson.quote(id)}}"""
        }

        data class Play(val path: String) : ClientMessage {
            override fun toLine(id: String) =
                """{"type":"play","id":${RemoteJson.quote(id)},"path":${RemoteJson.quote(path)}}"""
        }

        data object Pause : ClientMessage {
            override fun toLine(id: String) = """{"type":"pause","id":${RemoteJson.quote(id)}}"""
        }

        data object Resume : ClientMessage {
            override fun toLine(id: String) = """{"type":"resume","id":${RemoteJson.quote(id)}}"""
        }

        data object Toggle : ClientMessage {
            override fun toLine(id: String) = """{"type":"toggle","id":${RemoteJson.quote(id)}}"""
        }

        data class Seek(val positionMs: Long) : ClientMessage {
            override fun toLine(id: String) =
                """{"type":"seek","id":${RemoteJson.quote(id)},"position_ms":$positionMs}"""
        }

        data class SetVolume(val level: Int) : ClientMessage {
            override fun toLine(id: String) =
                """{"type":"volume","id":${RemoteJson.quote(id)},"level":$level}"""
        }

        /** Ask the server to re-walk its library root right now. Answered with a [RescanResult];
         * the `library_version` it bumps also arrives on the next `state` push, which is what
         * actually drives the listing refresh — see [RemoteViewModel]. */
        data object Rescan : ClientMessage {
            override fun toLine(id: String) = """{"type":"rescan","id":${RemoteJson.quote(id)}}"""
        }

        /** `docs/15-next-generation-engines.md` §D: the handful of things a headless server cannot
         * otherwise tell this phone about itself. Answered with a [HealthReport]. */
        data object Health : ClientMessage {
            override fun toLine(id: String) = """{"type":"health","id":${RemoteJson.quote(id)}}"""
        }
    }

    /** Server → client. */
    sealed interface ServerMessage {
        /** Pushed on connect and on every change; carries no id, unlike everything else here. */
        data class State(val playback: PlaybackState) : ServerMessage

        data class Paired(val id: String, val token: String) : ServerMessage

        /** An `ok` reply. The Rust side's `ReplyBody` is an enum — a reply carries a library
         * listing, a rescan result, a health report, or nothing — and at most one of these three is
         * ever non-null here. Modelled as three nullable fields rather than a sealed hierarchy of
         * its own because the caller that sent the request already knows which one it is waiting
         * for; it only needs to check that field, and `RemoteClient` does exactly that. A reply to
         * a plain command, or one whose `result` is a shape this client does not know, has all
         * three null — never an exception, for the same reason [Unknown] exists. */
        data class ReplyOk(
            val id: String,
            val library: List<LibraryEntry>?,
            val rescan: RescanResult? = null,
            val health: HealthReport? = null,
        ) : ServerMessage

        data class ReplyError(val id: String, val message: String) : ServerMessage

        /** Well-formed JSON naming a type this client does not recognise, or malformed input.
         * Never thrown for — an older client talking to a newer server, or one stray corrupt line,
         * must not take the reader loop down with it. */
        data object Unknown : ServerMessage
    }

    data class PlaybackState(val nowPlaying: NowPlaying?, val libraryVersion: Long)

    data class NowPlaying(
        val path: String,
        val title: String,
        val durationMs: Long,
        val positionMs: Long,
        val paused: Boolean,
        val volume: Int,
    )

    data class LibraryEntry(val path: String, val title: String, val durationMs: Long)

    /** What a completed [ClientMessage.Rescan] found: how many playable files the fresh walk
     * counted, and the `library_version` the server now stands at — the same number the next
     * `state` push will carry. */
    data class RescanResult(val fileCount: Long, val libraryVersion: Long)

    /**
     * Mirrors the Rust side's `HealthReport` field for field, including which ones are optional.
     *
     * The three nullable fields are nullable because the server deliberately sends `null` for
     * "unknown" — a certificate persisted before expiry was tracked, a library that has never been
     * reindexed, a platform disk-space call that failed — and a `0` standing in for any of those
     * would read as a genuine answer ("expires now", "reindexed in 1970", "disk full"). The two
     * non-null fields are always present when the reply arrives at all: a wedged player surfaces
     * as a protocol-level error reply, not a degraded number here.
     */
    data class HealthReport(
        val mpvRoundtripMs: Long,
        /** Negative once the certificate has already lapsed — "expired 3 days ago" and "expires in
         * 3 days" are different situations, and the server does not clamp them together. */
        val tlsCertExpiresInSecs: Long?,
        val libraryLastIndexedUnixSecs: Long?,
        val freeDiskBytes: Long?,
        val pairedClientCount: Long,
    )

    /** A number that the server may legitimately send as `null`: absent or JSON `null` become
     * Kotlin `null`, never a zero. A field of the wrong type (a string where a number belongs) is
     * treated the same way — the honest reading is "no usable value", not "zero". */
    private fun JsonValue.optLong(key: String): Long? = field(key)?.asLong()

    /** Parse one line from the server. */
    fun parseServerMessage(line: String): ServerMessage {
        val obj = RemoteJson.parse(line) ?: return ServerMessage.Unknown
        val type = obj.field("type")?.asString() ?: return ServerMessage.Unknown
        return when (type) {
            "state" -> {
                val npField = obj.field("now_playing")
                val np = when {
                    npField == null || npField is JsonValue.Null -> null
                    else -> {
                        val n = npField as? JsonValue.Obj ?: return ServerMessage.Unknown
                        NowPlaying(
                            path = n.field("path")?.asString() ?: return ServerMessage.Unknown,
                            title = n.field("title")?.asString() ?: return ServerMessage.Unknown,
                            durationMs = n.field("duration_ms")?.asLong() ?: return ServerMessage.Unknown,
                            positionMs = n.field("position_ms")?.asLong() ?: return ServerMessage.Unknown,
                            paused = n.field("paused")?.asBoolean() ?: return ServerMessage.Unknown,
                            volume = n.field("volume")?.asLong()?.toInt() ?: return ServerMessage.Unknown,
                        )
                    }
                }
                val version = obj.field("library_version")?.asLong() ?: return ServerMessage.Unknown
                ServerMessage.State(PlaybackState(np, version))
            }
            "paired" -> {
                val id = obj.field("id")?.asString() ?: return ServerMessage.Unknown
                val token = obj.field("token")?.asString() ?: return ServerMessage.Unknown
                ServerMessage.Paired(id, token)
            }
            "reply" -> {
                val id = obj.field("id")?.asString() ?: return ServerMessage.Unknown
                val ok = obj.field("ok")?.asBoolean() ?: false
                if (!ok) {
                    val message = obj.field("error")?.asString() ?: "unknown error"
                    return ServerMessage.ReplyError(id, message)
                }
                val result = obj.field("result")
                val library = result?.items()?.mapNotNull { e ->
                    val path = e.field("path")?.asString() ?: return@mapNotNull null
                    val title = e.field("title")?.asString() ?: return@mapNotNull null
                    val duration = e.field("duration_ms")?.asLong() ?: return@mapNotNull null
                    LibraryEntry(path, title, duration)
                }
                // A `result` object is either a rescan result or a health report — the Rust side
                // emits no other object-shaped reply. Told apart by the keys the writer always
                // includes, not by which request this is answering: this parser does not know that,
                // RemoteClient does. Each is required to carry *every* one of its always-present
                // fields, so a half-recognised object (say, a future reply type that happens to
                // share one key) parses as neither rather than as a report with invented zeros.
                val rescan = result?.let { r ->
                    val fileCount = r.optLong("file_count") ?: return@let null
                    val version = r.optLong("library_version") ?: return@let null
                    RescanResult(fileCount, version)
                }
                val health = result?.let { r ->
                    val roundtrip = r.optLong("mpv_roundtrip_ms") ?: return@let null
                    val clients = r.optLong("paired_client_count") ?: return@let null
                    HealthReport(
                        mpvRoundtripMs = roundtrip,
                        tlsCertExpiresInSecs = r.optLong("tls_cert_expires_in_secs"),
                        libraryLastIndexedUnixSecs = r.optLong("library_last_indexed_unix_secs"),
                        freeDiskBytes = r.optLong("free_disk_bytes"),
                        pairedClientCount = clients,
                    )
                }
                ServerMessage.ReplyOk(id, library, rescan, health)
            }
            else -> ServerMessage.Unknown
        }
    }
}
