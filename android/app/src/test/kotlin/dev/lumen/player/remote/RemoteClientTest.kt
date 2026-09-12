package dev.lumen.player.remote

import java.io.ByteArrayOutputStream
import java.io.OutputStreamWriter
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

/**
 * The write side of `RemoteClient`, checked on a plain JVM.
 *
 * The client itself needs a TLS socket to a real `lumen serve`, which is the Rust integration test's
 * job. What can be proved here without one is the discipline the server depends on: that whatever
 * runs concurrently on the phone, each line on the wire is exactly one message.
 */
class RemoteClientTest {

    @Test
    fun `concurrent requests never share a line on the wire`() = runBlocking {
        val sink = ByteArrayOutputStream()
        val writer = LineWriter(OutputStreamWriter(sink, Charsets.UTF_8))
        // Enough writers and lines that a per-call rather than per-line lock shows up reliably:
        // two threads doing 20 000 lines each interleaved several percent of them in a harness of
        // the unprotected `write(line); write("\n")` sequence, so 40 000 lines across four threads
        // is far past the point where a regression would slip through by luck.
        val writers = 4
        val linesEach = 10_000
        coroutineScope {
            repeat(writers) { w ->
                launch(Dispatchers.IO) {
                    repeat(linesEach) { i ->
                        writer.writeLine(RemoteProtocol.ClientMessage.Health.toLine("$w-$i"))
                    }
                }
            }
        }

        val lines = sink.toString("UTF-8").split("\n")
        assertEquals("output ends with a newline", "", lines.last())
        val messages = lines.dropLast(1)
        assertEquals(writers * linesEach, messages.size)
        for (line in messages) {
            // The same parser the client reads the server with, and the same rule the server applies
            // to what the client sends: a line is one JSON value with nothing after it.
            val parsed = RemoteJson.parse(line)
            assertNotNull("not exactly one JSON value on the line: $line", parsed)
            assertEquals("health", parsed?.field("type")?.asString())
        }
    }
}
