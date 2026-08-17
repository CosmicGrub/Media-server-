package dev.lumen.player.remote

/**
 * A minimal JSON reader and writer, hand-written rather than `org.json`.
 *
 * `org.json` is bundled in the Android SDK's stub jar, and this project's local unit tests run with
 * `isReturnDefaultValues = true` so a stray Android-framework call returns a default instead of
 * throwing "not mocked". That flag would make `org.json.JSONObject` silently return empty defaults
 * instead of actually parsing anything in a JVM unit test — the protocol tests would pass without
 * testing the protocol. Writing this by hand, exactly as the desktop side's own `json.rs` already
 * does for the same reason, means the parser is real Kotlin with no Android dependency: identical
 * behaviour on a device and in a plain JVM test, and nothing to accidentally stub.
 *
 * Scoped to what `RemoteProtocol` needs to read and write — one line of machine-generated JSON —
 * not a general-purpose library.
 */
sealed interface JsonValue {
    data object Null : JsonValue
    data class Bool(val value: Boolean) : JsonValue
    data class Num(val value: Double) : JsonValue
    data class Str(val value: String) : JsonValue
    data class Arr(val items: List<JsonValue>) : JsonValue
    data class Obj(val fields: Map<String, JsonValue>) : JsonValue

    fun asString(): String? = (this as? Str)?.value
    fun asLong(): Long? = (this as? Num)?.value?.toLong()
    fun asBoolean(): Boolean? = (this as? Bool)?.value
    fun field(key: String): JsonValue? = (this as? Obj)?.fields?.get(key)
    fun items(): List<JsonValue>? = (this as? Arr)?.items
}

object RemoteJson {

    /** `null` on anything malformed, never an exception — the caller decides what an unreadable
     * line means (usually: ignore it and keep reading, since one bad line must not end the socket). */
    fun parse(text: String): JsonValue? = runCatching {
        val p = Parser(text)
        val v = p.parseValue()
        p.skipWs()
        if (!p.atEnd()) return null
        v
    }.getOrNull()

    private class Parser(private val s: String) {
        var i = 0

        fun atEnd() = i >= s.length
        fun skipWs() {
            while (i < s.length && s[i].isWhitespace()) i++
        }

        fun parseValue(): JsonValue {
            skipWs()
            if (atEnd()) error("unexpected end of input")
            return when (s[i]) {
                '{' -> parseObject()
                '[' -> parseArray()
                '"' -> JsonValue.Str(parseStringLiteral())
                't' -> literal("true", JsonValue.Bool(true))
                'f' -> literal("false", JsonValue.Bool(false))
                'n' -> literal("null", JsonValue.Null)
                else -> parseNumber()
            }
        }

        private fun literal(word: String, value: JsonValue): JsonValue {
            if (i + word.length > s.length || s.substring(i, i + word.length) != word) {
                error("expected $word")
            }
            i += word.length
            return value
        }

        private fun parseObject(): JsonValue.Obj {
            expect('{')
            val fields = LinkedHashMap<String, JsonValue>()
            skipWs()
            if (peekIs('}')) {
                i++
                return JsonValue.Obj(fields)
            }
            while (true) {
                skipWs()
                val key = parseStringLiteral()
                skipWs()
                expect(':')
                val value = parseValue()
                fields[key] = value
                skipWs()
                when {
                    peekIs(',') -> {
                        i++
                    }
                    peekIs('}') -> {
                        i++
                        return JsonValue.Obj(fields)
                    }
                    else -> error("expected ',' or '}'")
                }
            }
        }

        private fun parseArray(): JsonValue.Arr {
            expect('[')
            val items = mutableListOf<JsonValue>()
            skipWs()
            if (peekIs(']')) {
                i++
                return JsonValue.Arr(items)
            }
            while (true) {
                items.add(parseValue())
                skipWs()
                when {
                    peekIs(',') -> {
                        i++
                    }
                    peekIs(']') -> {
                        i++
                        return JsonValue.Arr(items)
                    }
                    else -> error("expected ',' or ']'")
                }
            }
        }

        private fun parseStringLiteral(): String {
            expect('"')
            val sb = StringBuilder()
            while (true) {
                if (atEnd()) error("unterminated string")
                val c = s[i++]
                when (c) {
                    '"' -> return sb.toString()
                    '\\' -> {
                        if (atEnd()) error("unterminated escape")
                        when (val esc = s[i++]) {
                            '"' -> sb.append('"')
                            '\\' -> sb.append('\\')
                            '/' -> sb.append('/')
                            'b' -> sb.append('\b')
                            'f' -> sb.append('\u000C')
                            'n' -> sb.append('\n')
                            'r' -> sb.append('\r')
                            't' -> sb.append('\t')
                            'u' -> {
                                if (i + 4 > s.length) error("truncated \\u escape")
                                val hex = s.substring(i, i + 4)
                                i += 4
                                // Surrogate pairs pass through as-is: Kotlin/JVM strings are UTF-16,
                                // so appending each `\uXXXX` code unit in order reconstructs the
                                // original character exactly, astral or not, without decoding it here.
                                sb.append(hex.toInt(16).toChar())
                            }
                            else -> error("unknown escape \\$esc")
                        }
                    }
                    else -> sb.append(c)
                }
            }
        }

        private fun parseNumber(): JsonValue.Num {
            val start = i
            if (peekIs('-')) i++
            while (i < s.length && s[i].isDigit()) i++
            if (i < s.length && s[i] == '.') {
                i++
                while (i < s.length && s[i].isDigit()) i++
            }
            if (i < s.length && (s[i] == 'e' || s[i] == 'E')) {
                i++
                if (i < s.length && (s[i] == '+' || s[i] == '-')) i++
                while (i < s.length && s[i].isDigit()) i++
            }
            if (i == start) error("expected a value")
            return JsonValue.Num(s.substring(start, i).toDouble())
        }

        private fun expect(c: Char) {
            if (atEnd() || s[i] != c) error("expected '$c'")
            i++
        }

        private fun peekIs(c: Char): Boolean = i < s.length && s[i] == c
    }

    /** Escape a string for embedding in a JSON document. Only what the grammar requires — `"`, `\`
     * and control characters — since everything else, including any non-ASCII character, is legal
     * inside a JSON string literal unescaped. */
    fun quote(value: String): String {
        val sb = StringBuilder(value.length + 2)
        sb.append('"')
        for (c in value) {
            when {
                c == '"' -> sb.append("\\\"")
                c == '\\' -> sb.append("\\\\")
                c == '\n' -> sb.append("\\n")
                c == '\r' -> sb.append("\\r")
                c == '\t' -> sb.append("\\t")
                c.code < 0x20 -> sb.append("\\u%04x".format(c.code))
                else -> sb.append(c)
            }
        }
        sb.append('"')
        return sb.toString()
    }
}
