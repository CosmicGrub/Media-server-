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
    }

    /** Server → client. */
    sealed interface ServerMessage {
        /** Pushed on connect and on every change; carries no id, unlike everything else here. */
        data class State(val playback: PlaybackState) : ServerMessage

        data class Paired(val id: String, val token: String) : ServerMessage

        data class ReplyOk(val id: String, val library: List<LibraryEntry>?) : ServerMessage

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
                val library = obj.field("result")?.items()?.mapNotNull { e ->
                    val path = e.field("path")?.asString() ?: return@mapNotNull null
                    val title = e.field("title")?.asString() ?: return@mapNotNull null
                    val duration = e.field("duration_ms")?.asLong() ?: return@mapNotNull null
                    LibraryEntry(path, title, duration)
                }
                ServerMessage.ReplyOk(id, library)
            }
            else -> ServerMessage.Unknown
        }
    }
}
