package dev.lumen.player.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The hand-rolled JSON reader/writer, checked without a device.
 *
 * Written by hand specifically so it needs no Android framework and can be tested on a plain JVM —
 * see the doc comment on [RemoteJson] for why `org.json` was not safe to use here. These tests are
 * the proof that choice actually works: every one of them would silently pass-by-doing-nothing if
 * `isReturnDefaultValues` were quietly stubbing the thing under test.
 */
class RemoteJsonTest {

    @Test
    fun `every primitive round trips`() {
        assertEquals(JsonValue.Null, RemoteJson.parse("null"))
        assertEquals(JsonValue.Bool(true), RemoteJson.parse("true"))
        assertEquals(JsonValue.Bool(false), RemoteJson.parse("false"))
        assertEquals(42.0, (RemoteJson.parse("42") as JsonValue.Num).value, 0.0001)
        assertEquals(-3.5, (RemoteJson.parse("-3.5") as JsonValue.Num).value, 0.0001)
        assertEquals(1.5e3, (RemoteJson.parse("1.5e3") as JsonValue.Num).value, 0.0001)
    }

    @Test
    fun `an object with several fields parses`() {
        val v = RemoteJson.parse("""{"a":1,"b":"x","c":true,"d":null}""")!!
        assertEquals(1L, v.field("a")?.asLong())
        assertEquals("x", v.field("b")?.asString())
        assertEquals(true, v.field("c")?.asBoolean())
        assertEquals(JsonValue.Null, v.field("d"))
    }

    @Test
    fun `a nested object and array parse`() {
        val v = RemoteJson.parse("""{"list":[{"x":1},{"x":2}],"inner":{"y":"z"}}""")!!
        val list = v.field("list")!!.items()!!
        assertEquals(2, list.size)
        assertEquals(1L, list[0].field("x")?.asLong())
        assertEquals(2L, list[1].field("x")?.asLong())
        assertEquals("z", v.field("inner")?.field("y")?.asString())
    }

    @Test
    fun `empty objects and arrays parse`() {
        assertTrue((RemoteJson.parse("{}") as JsonValue.Obj).fields.isEmpty())
        assertTrue((RemoteJson.parse("[]") as JsonValue.Arr).items.isEmpty())
    }

    @Test
    fun `standard escapes decode to the real character`() {
        val v = RemoteJson.parse(""""a\"b\\c\nd\te"""")
        assertEquals("a\"b\\c\nd\te", v?.asString())
    }

    @Test
    fun `a unicode escape decodes to the real character`() {
        assertEquals("é", RemoteJson.parse(""""é"""")?.asString())
    }

    @Test
    fun `raw non-ascii characters do not need escaping to parse`() {
        // The JSON grammar permits any Unicode character but the control set unescaped in a string
        // literal, and the desktop's own writer relies on exactly this.
        assertEquals("Amélie", RemoteJson.parse(""""Amélie"""")?.asString())
    }

    @Test
    fun `whitespace between tokens is ignored`() {
        val v = RemoteJson.parse(" { \"a\" : 1 , \"b\" : [ 1 , 2 ] } ")!!
        assertEquals(1L, v.field("a")?.asLong())
        assertEquals(listOf(1L, 2L), v.field("b")?.items()?.map { it.asLong() })
    }

    @Test
    fun `malformed input is null rather than an exception`() {
        assertNull(RemoteJson.parse(""))
        assertNull(RemoteJson.parse("not json"))
        assertNull(RemoteJson.parse("{"))
        assertNull(RemoteJson.parse("""{"a":}"""))
        assertNull(RemoteJson.parse("""{"a":1"""))
        assertNull(RemoteJson.parse("""{"a":1,}""")) // trailing comma is not valid JSON
        assertNull(RemoteJson.parse("tru")) // a truncated literal
    }

    @Test
    fun `trailing garbage after a complete value is rejected`() {
        // A line with a second, unrelated value tacked on the end must not be silently accepted as
        // the first one — that would parse only a prefix of what was actually sent.
        assertNull(RemoteJson.parse("""{"a":1} garbage"""))
    }

    @Test
    fun `quote produces something the parser reads back identically`() {
        for (s in listOf(
            "plain",
            "with \"quotes\"",
            "with\\backslash",
            "with\nnewline\tand\rtab",
            "controlchar",
            "Amélie 日本語",
            "",
        )) {
            val line = RemoteJson.quote(s)
            assertEquals("round trip failed for $s", s, RemoteJson.parse(line)?.asString())
        }
    }

    @Test
    fun `quote always produces a quoted string even for empty input`() {
        assertEquals("\"\"", RemoteJson.quote(""))
    }
}
