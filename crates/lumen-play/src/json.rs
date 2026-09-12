//! A minimal JSON reader, sized for mpv's IPC protocol.
//!
//! The S1 spike gets away with substring matching because it only ever reads numbers. This does not:
//! mpv's events carry strings — file paths, codec names, the `reason` on `end-file` — and a path is
//! exactly the kind of string that contains braces, commas, and quotes. Substring matching on those
//! silently returns the wrong answer rather than failing, which is the worst way to be wrong.
//!
//! Written by hand rather than pulled in, to keep the workspace dependency-free. The scope is
//! deliberately small: parse one line of machine-generated JSON into a tree and read fields off it.
//! There is no serialisation here and no derive machinery.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            // mpv returns some numeric properties as strings depending on the request form, so a
            // caller asking for a number should get one either way.
            Self::Str(s) => s.parse().ok(),
            Self::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "json: {}", self.0)
    }
}

pub fn parse(text: &str) -> Result<Value, ParseError> {
    let bytes = text.as_bytes();
    let mut p = Parser { b: bytes, i: 0, depth: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i != p.b.len() {
        return Err(ParseError(format!("trailing input at byte {}", p.i)));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

/// Nesting limit. mpv's deepest reply is `track-list`, an array of flat objects — three levels. The
/// limit exists so a malformed or hostile line cannot recurse the parser into a stack overflow, which
/// on a player watching a shared folder would be a remote crash rather than a parse error.
const MAX_DEPTH: u32 = 64;

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), ParseError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(ParseError(format!(
                "expected `{}` at byte {}, found {:?}",
                c as char,
                self.i,
                self.peek().map(|b| b as char)
            )))
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError("nesting too deep".into()));
        }
        let v = match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(_) => self.number(),
            None => Err(ParseError("unexpected end of input".into())),
        };
        self.depth -= 1;
        v
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, ParseError> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(ParseError(format!("bad literal at byte {}", self.i)))
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.i += 1;
        }
        let raw = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| ParseError("non-utf8 in number".into()))?;
        raw.parse::<f64>().map(Value::Num).map_err(|_| ParseError(format!("bad number {raw:?}")))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or_else(|| ParseError("unterminated string".into()))?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.peek().ok_or_else(|| ParseError("dangling escape".into()))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        other => {
                            return Err(ParseError(format!("bad escape `\\{}`", other as char)));
                        }
                    }
                }
                // Multi-byte UTF-8 arrives as raw bytes; collect the continuation bytes with it.
                // A path with an accented character is completely ordinary and must survive intact.
                _ => {
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    self.i = (start + len).min(self.b.len());
                    let s = std::str::from_utf8(&self.b[start..self.i])
                        .map_err(|_| ParseError(format!("invalid utf-8 at byte {start}")))?;
                    out.push_str(s);
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let cp = self.hex4()?;
        // Surrogate pair: any character above the BMP is escaped as two halves. Decoding only the
        // first half yields an unpaired surrogate, which is not a `char` at all.
        if (0xD800..0xDC00).contains(&cp) {
            if self.b[self.i..].starts_with(b"\\u") {
                self.i += 2;
                let lo = self.hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let c = 0x1_0000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    return char::from_u32(c).ok_or_else(|| ParseError("bad surrogate".into()));
                }
            }
            return Err(ParseError("unpaired surrogate".into()));
        }
        char::from_u32(cp).ok_or_else(|| ParseError(format!("bad code point U+{cp:04X}")))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let end = self.i + 4;
        let raw = self.b.get(self.i..end).ok_or_else(|| ParseError("short \\u escape".into()))?;
        let s = std::str::from_utf8(raw).map_err(|_| ParseError("bad \\u escape".into()))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| ParseError("bad \\u escape".into()))?;
        self.i = end;
        Ok(v)
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.skip_ws();
            out.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(out));
                }
                _ => return Err(ParseError(format!("bad array at byte {}", self.i))),
            }
        }
    }

    fn object(&mut self) -> Result<Value, ParseError> {
        self.expect(b'{')?;
        let mut out = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Object(out));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let val = self.value()?;
            out.insert(key, val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(out));
                }
                _ => return Err(ParseError(format!("bad object at byte {}", self.i))),
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        // A continuation byte in leading position is malformed; consuming one byte lets the
        // from_utf8 check below report it rather than looping forever.
        _ => 1,
    }
}

/// Escape a string into a JSON string literal, including the surrounding quotes.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_property_reply_is_read() {
        let v = parse(r#"{"data":23.976,"request_id":1,"error":"success"}"#).unwrap();
        assert_eq!(v.get("data").and_then(Value::as_f64), Some(23.976));
        assert_eq!(v.get("error").and_then(Value::as_str), Some("success"));
    }

    #[test]
    fn a_path_containing_json_punctuation_survives() {
        // The exact case substring matching gets wrong, and gets wrong *silently*. Release names
        // with brackets and commas are completely ordinary.
        let line = r#"{"event":"start-file","path":"/m/Movie (2019) [1080p, x265] {tag}.mkv"}"#;
        let v = parse(line).unwrap();
        assert_eq!(
            v.get("path").and_then(Value::as_str),
            Some("/m/Movie (2019) [1080p, x265] {tag}.mkv")
        );
    }

    #[test]
    fn escapes_in_paths_are_decoded() {
        // A Windows path is nothing but escaped backslashes.
        let v = parse(r#"{"path":"C:\\Media\\Film \"Director's Cut\".mkv"}"#).unwrap();
        assert_eq!(
            v.get("path").and_then(Value::as_str),
            Some(r#"C:\Media\Film "Director's Cut".mkv"#)
        );
    }

    #[test]
    fn non_ascii_paths_survive_both_raw_and_escaped() {
        // Real libraries are full of these, and losing one means failing to play the file.
        let raw = parse(r#"{"path":"/媒体/映画 Amélie.mkv"}"#).unwrap();
        assert_eq!(raw.get("path").and_then(Value::as_str), Some("/媒体/映画 Amélie.mkv"));
        let escaped = parse(r#"{"t":"Am\u00e9lie"}"#).unwrap();
        assert_eq!(escaped.get("t").and_then(Value::as_str), Some("Amélie"));
    }

    #[test]
    fn characters_above_the_bmp_need_both_surrogate_halves() {
        // Emoji in a filename are escaped as a surrogate pair. Decoding half of one is not a char.
        let v = parse(r#"{"t":"clip \ud83c\udfac done"}"#).unwrap();
        assert_eq!(v.get("t").and_then(Value::as_str), Some("clip 🎬 done"));
        assert!(parse(r#"{"t":"\ud83c"}"#).is_err(), "an unpaired surrogate must not parse");
    }

    #[test]
    fn nested_structures_are_readable() {
        let v = parse(r#"{"data":{"video-params":{"w":3840,"h":2160}}}"#).unwrap();
        let w = v.get("data").and_then(|d| d.get("video-params")).and_then(|p| p.get("w"));
        assert_eq!(w.and_then(Value::as_f64), Some(3840.0));
        assert_eq!(v.get("data").and_then(|d| d.get("missing")), None);
    }

    #[test]
    fn track_lists_parse_as_arrays_of_objects() {
        let line = r#"{"data":[{"id":1,"type":"video","codec":"hevc"},
                               {"id":2,"type":"audio","codec":"truehd","lang":"eng"}]}"#;
        let v = parse(line).unwrap();
        let tracks = v.get("data").and_then(Value::as_array).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].get("codec").and_then(Value::as_str), Some("truehd"));
    }

    #[test]
    fn empty_containers_parse() {
        assert_eq!(parse("[]").unwrap(), Value::Array(vec![]));
        assert_eq!(parse("{}").unwrap(), Value::Object(BTreeMap::new()));
        assert_eq!(parse(r#"{"data":[]}"#).unwrap().get("data").unwrap().as_array(), Some(&[][..]));
    }

    #[test]
    fn malformed_input_is_an_error_rather_than_a_panic() {
        // Everything here is something a truncated socket read can produce.
        for bad in [
            "",
            "{",
            "}",
            "[",
            r#"{"a"}"#,
            r#"{"a":}"#,
            r#"{"a":1,}"#,
            r#""unterminated"#,
            "{\"a\":1}trailing",
            r#"{"a":"\q"}"#,
            r#"{"a":"\u00"}"#,
            "nul",
            "tru",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed when it should not have");
        }
    }

    #[test]
    fn deep_nesting_is_rejected_rather_than_overflowing_the_stack() {
        // A player watching a shared folder must not be crashable by a malformed line.
        let deep = format!("{}1{}", "[".repeat(500), "]".repeat(500));
        assert!(parse(&deep).is_err());
    }

    /// A tiny xorshift PRNG, deterministic from a seed. No `rand` dependency: this file's whole
    /// reason to exist is staying dependency-free (see the module doc), and a fuzz-style test is not
    /// worth breaking that for.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn no_random_byte_sequence_ever_panics_the_parser_only_errors() {
        // mpv's own replies are machine-generated and well-formed, but a truncated socket read (this
        // parser's actual adversary, per every other test above) can hand it any prefix of any byte
        // sequence -- not just the hand-picked malformed cases above. A real fuzzer would be the
        // rigorous version of this; a seeded PRNG hammering the entry point for a few hundred
        // thousand iterations, entirely within one fast unit test, is the version that costs nothing
        // extra to keep running on every `cargo test`.
        let mut state = 0x2545F4914F6CDD1Du64; // any nonzero seed
        // A byte alphabet biased toward JSON's own punctuation and a few multi-byte UTF-8 lead bytes,
        // rather than uniform-random bytes: uniform noise almost always fails on the very first
        // character and never reaches the deeper code paths (escapes, surrogate pairs, nesting) this
        // exists to stress.
        const ALPHABET: &[u8] = b"{}[]\":,.-+0123456789tfnul\\ru\xC3\xA9\xF0\x9F\x8E\xAC \t\n";
        for _ in 0..200_000 {
            let len = (xorshift(&mut state) % 24) as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                let idx = (xorshift(&mut state) as usize) % ALPHABET.len();
                buf.push(ALPHABET[idx]);
            }
            // Not every random byte sequence is valid UTF-8 (the parser's own contract, `&str` in);
            // that is `String::from_utf8`'s job to reject, not this parser's.
            if let Ok(s) = String::from_utf8(buf) {
                let _ = parse(&s); // must not panic, whatever it returns
            }
        }
    }

    #[test]
    fn numbers_cover_the_forms_mpv_emits() {
        assert_eq!(parse("-0.004").unwrap().as_f64(), Some(-0.004));
        assert_eq!(parse("1e3").unwrap().as_f64(), Some(1000.0));
        assert_eq!(parse("0").unwrap().as_f64(), Some(0.0));
    }

    #[test]
    fn quoting_round_trips_through_the_parser() {
        for s in [
            r#"C:\Media\Film "Cut".mkv"#,
            "tab\there",
            "Amélie 🎬",
            "/m/Movie (2019) [1080p, x265].mkv",
        ] {
            let line = format!("{{\"p\":{}}}", quote(s));
            let back = parse(&line).unwrap();
            assert_eq!(
                back.get("p").and_then(Value::as_str),
                Some(s),
                "round trip failed for {s:?}"
            );
        }
    }
}
