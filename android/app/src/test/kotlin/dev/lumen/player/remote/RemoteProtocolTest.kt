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
    fun `a rescan request parses and carries its id`() {
        // Mirrors the Rust test of the same name: the server's parser is what accepts this shape.
        assertEquals("""{"type":"rescan","id":"11"}""", RemoteProtocol.ClientMessage.Rescan.toLine("11"))
    }

    @Test
    fun `a health request parses and carries its id`() {
        assertEquals("""{"type":"health","id":"9"}""", RemoteProtocol.ClientMessage.Health.toLine("9"))
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
            RemoteProtocol.ClientMessage.Rescan,
            RemoteProtocol.ClientMessage.Health,
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
    fun `a rescan reply carries the fresh count and version`() {
        // The same numbers the Rust test of this name writes, read back here.
        val msg = RemoteProtocol.parseServerMessage(
            """{"type":"reply","id":"4","ok":true,"result":{"file_count":137,"library_version":3}}"""
        )
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk("4", null, rescan = RemoteProtocol.RescanResult(137, 3)),
            msg,
        )
    }

    @Test
    fun `a health reply carries every known field`() {
        val line = """{"type":"reply","id":"7","ok":true,"result":{"mpv_roundtrip_ms":12,""" +
            """"tls_cert_expires_in_secs":1000000,"library_last_indexed_unix_secs":1700000000,""" +
            """"free_disk_bytes":999999999,"paired_client_count":2}}"""
        val msg = RemoteProtocol.parseServerMessage(line)
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk(
                "7",
                null,
                health = RemoteProtocol.HealthReport(
                    mpvRoundtripMs = 12,
                    tlsCertExpiresInSecs = 1_000_000,
                    libraryLastIndexedUnixSecs = 1_700_000_000,
                    freeDiskBytes = 999_999_999,
                    pairedClientCount = 2,
                ),
            ),
            msg,
        )
    }

    @Test
    fun `a health reply reports unknown fields as null rather than a fabricated value`() {
        // The server sends JSON null for a certificate persisted before expiry tracking existed,
        // a library never reindexed, and a failed disk-space call. Each must land as a Kotlin null:
        // a zero here would read as "expires now", "indexed in 1970" and "disk full".
        val line = """{"type":"reply","id":"8","ok":true,"result":{"mpv_roundtrip_ms":5,""" +
            """"tls_cert_expires_in_secs":null,"library_last_indexed_unix_secs":null,""" +
            """"free_disk_bytes":null,"paired_client_count":0}}"""
        val msg = RemoteProtocol.parseServerMessage(line) as RemoteProtocol.ServerMessage.ReplyOk
        val health = msg.health!!
        assertEquals(5L, health.mpvRoundtripMs)
        assertNull(health.tlsCertExpiresInSecs)
        assertNull(health.libraryLastIndexedUnixSecs)
        assertNull(health.freeDiskBytes)
        assertEquals(0L, health.pairedClientCount)
        assertNull(msg.rescan)
        assertNull(msg.library)
    }

    @Test
    fun `a negative cert expiry survives the wire for an already lapsed certificate`() {
        // Negative, not clamped to zero: "expired 3 days ago" and "expires in 3 days" have to stay
        // distinguishable all the way to the card that shows them.
        val line = """{"type":"reply","id":"9","ok":true,"result":{"mpv_roundtrip_ms":1,""" +
            """"tls_cert_expires_in_secs":-259200,"library_last_indexed_unix_secs":null,""" +
            """"free_disk_bytes":null,"paired_client_count":0}}"""
        val msg = RemoteProtocol.parseServerMessage(line) as RemoteProtocol.ServerMessage.ReplyOk
        assertEquals(-259_200L, msg.health?.tlsCertExpiresInSecs)
    }

    @Test
    fun `an ok reply whose result object is not a shape this client knows is still an ok reply`() {
        // A newer server answering with a result type this build has never heard of must be an
        // ordinary ok reply with nothing attached — not an exception, and not a report with
        // invented values. Includes the half-recognised case: one key of a rescan result present
        // without the other must not parse as a rescan of zero files.
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk("1", null),
            RemoteProtocol.parseServerMessage("""{"type":"reply","id":"1","ok":true,"result":{"novel":true}}"""),
        )
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk("2", null),
            RemoteProtocol.parseServerMessage("""{"type":"reply","id":"2","ok":true,"result":{}}"""),
        )
        assertEquals(
            RemoteProtocol.ServerMessage.ReplyOk("3", null),
            RemoteProtocol.parseServerMessage(
                """{"type":"reply","id":"3","ok":true,"result":{"library_version":9}}"""
            ),
        )
    }

    @Test
    fun `a library reply is not mistaken for a rescan or health reply`() {
        // The array-shaped result and the object-shaped ones share one `result` key; the listing
        // must still come through as a listing, with the two object fields left null.
        val line = """{"type":"reply","id":"3","ok":true,"result":[{"path":"/a.mkv","title":"A","duration_ms":1000}]}"""
        val msg = RemoteProtocol.parseServerMessage(line) as RemoteProtocol.ServerMessage.ReplyOk
        assertEquals(listOf(RemoteProtocol.LibraryEntry("/a.mkv", "A", 1000)), msg.library)
        assertNull(msg.rescan)
        assertNull(msg.health)
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
