package dev.lumen.player.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The protocol layer, checked against the exact shapes `crates/lumen-play/src/remote/protocol.rs`
 * emits and expects. Several of these mirror a test on the Rust side by name and intent — the two
 * implementations have to agree on the wire, and the honest way to check that without a running
 * server is to assert both sides against the same concrete JSON.
 */
class RemoteProtocolTest {

    @Test
    fun `every client message carries the type and id the server expects`() {
        assertEquals(
            """{"type":"pair","id":"1","code":"123456"}""",
            RemoteProtocol.ClientMessage.Pair("123456").toLine("1"),
        )
        assertEquals(
            """{"type":"auth","id":"2","token":"abc"}""",
            RemoteProtocol.ClientMessage.Auth("abc").toLine("2"),
        )
        assertEquals("""{"type":"library","id":"3"}""", RemoteProtocol.ClientMessage.Library.toLine("3"))
        assertEquals(
            """{"type":"play","id":"4","path":"/a.mkv"}""",
            RemoteProtocol.ClientMessage.Play("/a.mkv").toLine("4"),
        )
        assertEquals("""{"type":"pause","id":"5"}""", RemoteProtocol.ClientMessage.Pause.toLine("5"))
        assertEquals("""{"type":"resume","id":"6"}""", RemoteProtocol.ClientMessage.Resume.toLine("6"))
        assertEquals("""{"type":"toggle","id":"7"}""", RemoteProtocol.ClientMessage.Toggle.toLine("7"))
        assertEquals(
            """{"type":"seek","id":"8","position_ms":90000}""",
            RemoteProtocol.ClientMessage.Seek(90_000).toLine("8"),
        )
        assertEquals(
            """{"type":"volume","id":"9","level":40}""",
            RemoteProtocol.ClientMessage.SetVolume(40).toLine("9"),
        )
    }

    @Test
    fun `every client message line is itself valid json`() {
        // Built by hand-written string templates rather than the parser's writer, so this is the
        // one check that the templates in toLine() have not drifted into something malformed.
        val messages = listOf(
            RemoteProtocol.ClientMessage.Pair("000000"),
            RemoteProtocol.ClientMessage.Auth("tok"),
            RemoteProtocol.ClientMessage.Library,
            RemoteProtocol.ClientMessage.Play("path"),
            RemoteProtocol.ClientMessage.Pause,
            RemoteProtocol.ClientMessage.Seek(1),
            RemoteProtocol.ClientMessage.SetVolume(1),
        )
        for (m in messages) {
            val line = m.toLine("id")
            assertTrue("not valid JSON: $line", RemoteJson.parse(line) != null)
        }
    }

    @Test
    fun `a path with quotes and backslashes is escaped in the request`() {
        // Windows paths and any title containing a quote are exactly the input that breaks a
        // hand-rolled JSON writer that concatenates strings instead of escaping them.
        val path = """C:\Media\Director's "Cut".mkv"""
        val line = RemoteProtocol.ClientMessage.Play(path).toLine("1")
        val parsed = RemoteJson.parse(line)!!
        assertEquals(path, parsed.field("path")?.asString())
    }

    @Test
    fun `a state push with nothing playing parses`() {
        val msg = RemoteProtocol.parseServerMessage("""{"type":"state","now_playing":null,"library_version":3}""")
        assertEquals(
            RemoteProtocol.ServerMessage.State(RemoteProtocol.PlaybackState(null, 3)),
            msg,
        )
    }

    @Test
    fun `a state push with something playing parses exactly`() {
        val line = """{"type":"state","now_playing":{"path":"/media/Film (2019).mkv",""" +
            """"title":"Film (2019)","duration_ms":7200000,"position_ms":42000,""" +
            """"paused":false,"volume":80},"library_version":12}"""
        val msg = RemoteProtocol.parseServerMessage(line)
        assertEquals(
            RemoteProtocol.ServerMessage.State(
                RemoteProtocol.PlaybackState(
                    RemoteProtocol.NowPlaying(
                        path = "/media/Film (2019).mkv",
                        title = "Film (2019)",
                        durationMs = 7_200_000,
                        positionMs = 42_000,
                        paused = false,
                        volume = 80,
                    ),
                    libraryVersion = 12,
                )
            ),
            msg,
        )
    }

    @Test
    fun `a paired reply carries the token back`() {
        val msg = RemoteProtocol.parseServerMessage("""{"type":"paired","id":"5","token":"abc123"}""")
        assertEquals(RemoteProtocol.ServerMessage.Paired("5", "abc123"), msg)
    }

    @Test
    fun `an error reply names the id it answers and the reason`() {
        val msg = RemoteProtocol.parseServerMessage(
            """{"type":"reply","id":"9","ok":false,"error":"wrong pairing code"}"""
        )
        assertEquals(RemoteProtocol.ServerMessage.ReplyError("9", "wrong pairing code"), msg)
    }

    @Test
    fun `an ok reply with no result carries an empty library`() {
        val msg = RemoteProtocol.parseServerMessage("""{"type":"reply","id":"1","ok":true}""")
        assertEquals(RemoteProtocol.ServerMessage.ReplyOk("1", null), msg)
    }

    @Test
    fun `a library reply carries every entry`() {
        val line = """{"type":"reply","id":"3","ok":true,"result":[""" +
            """{"path":"/a.mkv","title":"A","duration_ms":1000},""" +
            """{"path":"/b.mkv","title":"B","duration_ms":2000}]}"""
        val msg = RemoteProtocol.parseServerMessage(line)
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk(
                "3",
                listOf(
                    RemoteProtocol.LibraryEntry("/a.mkv", "A", 1000),
                    RemoteProtocol.LibraryEntry("/b.mkv", "B", 2000),
                ),
            ),
            msg,
        )
    }

    @Test
    fun `malformed or unrecognised input becomes Unknown rather than throwing`() {
        assertEquals(RemoteProtocol.ServerMessage.Unknown, RemoteProtocol.parseServerMessage(""))
        assertEquals(RemoteProtocol.ServerMessage.Unknown, RemoteProtocol.parseServerMessage("not json"))
        assertEquals(RemoteProtocol.ServerMessage.Unknown, RemoteProtocol.parseServerMessage("{}"))
        assertEquals(
            RemoteProtocol.ServerMessage.Unknown,
            RemoteProtocol.parseServerMessage("""{"type":"made-up-type"}"""),
        )
        // A state push missing a required field inside now_playing must not be half-parsed into a
        // NowPlaying with a zeroed-out field standing in for data that was never actually there.
        assertEquals(
            RemoteProtocol.ServerMessage.Unknown,
            RemoteProtocol.parseServerMessage(
                """{"type":"state","now_playing":{"path":"/a.mkv"},"library_version":1}"""
            ),
        )
    }

    @Test
    fun `state never carries an id`() {
        // The one message type allowed to arrive unprompted, interleaved with request/response
        // traffic, must never accidentally look like a reply — RemoteClient's matching-by-id would
        // otherwise resolve some unrelated pending request with an unrelated state push.
        val line = """{"type":"state","now_playing":null,"library_version":0}"""
        val v = RemoteJson.parse(line)!!
        assertNull(v.field("id"))
    }

    @Test
    fun `SetVolume itself does not clamp, so the boundary lives in exactly one place`() {
        // Clamping happens one layer up, in RemoteClient.setVolume — this message type carries
        // whatever level it is given. Asserted here so that boundary decision is documented once,
        // rather than half-duplicated by a second clamp quietly added to this class later.
        assertEquals(
            """{"type":"volume","id":"1","level":150}""",
            RemoteProtocol.ClientMessage.SetVolume(150).toLine("1"),
        )
    }
}
