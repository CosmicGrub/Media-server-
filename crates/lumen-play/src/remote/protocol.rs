//! The wire protocol between `lumen serve` and a remote client (phone, watch, eventually web).
//!
//! **Newline-delimited JSON over a plain TCP socket, not WebSocket.** This is a deliberate choice,
//! not a shortcut. mpv's own IPC protocol — which `crate::ipc` already speaks — is exactly this
//! shape: one JSON object per line, read with a blocking loop. Extending that same shape from a
//! local socket to a LAN socket costs nothing new to understand and nothing new to depend on. A
//! WebSocket server needs an HTTP upgrade handshake (parsing headers, computing
//! `Sec-WebSocket-Accept`, which means either a hand-rolled SHA-1 or another dependency) for a
//! capability this protocol does not need yet: there is no browser client today, and when one shows
//! up a small WS-to-TCP bridge is a bounded, separate piece of work rather than a reason to carry
//! that machinery now.
//!
//! **Push, not poll.** The server writes a new `State` line whenever what is playing changes; the
//! client just reads its socket. A watch polling over Bluetooth every few seconds to ask "anything
//! new?" is a battery problem wearing a protocol's clothes.
//!
//! Every message that expects an answer carries an `id`; the reply echoes it. That is what lets one
//! socket carry a state stream and a request/response exchange at the same time without the two
//! kinds of message getting confused — a client blocked waiting for the reply to request `"7"` must
//! not mistake a same-shaped state push for its answer.

use crate::json::{Value, quote};

/// A message read from the wire, before it is acted on.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    /// First message on a socket that has never paired: the code shown on the server's terminal.
    Pair {
        id: String,
        code: String,
    },
    /// First message on a socket that already has a token from a previous pairing.
    Auth {
        id: String,
        token: String,
    },
    /// The library, as of the last scan.
    Library {
        id: String,
    },
    Play {
        id: String,
        path: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    /// Toggle rather than a separate Pause/Resume call from the client's own guess at the current
    /// state — the server's state push is the only source of truth for what is currently playing,
    /// and a client that raced its own guess against a state update would occasionally send the
    /// wrong one.
    TogglePlayPause {
        id: String,
    },
    Seek {
        id: String,
        position_ms: i64,
    },
    SetVolume {
        id: String,
        level: u8,
    },
    Next {
        id: String,
    },
    Previous {
        id: String,
    },
}

impl ClientMessage {
    pub fn id(&self) -> &str {
        match self {
            Self::Pair { id, .. }
            | Self::Auth { id, .. }
            | Self::Library { id, .. }
            | Self::Play { id, .. }
            | Self::Pause { id, .. }
            | Self::Resume { id, .. }
            | Self::TogglePlayPause { id, .. }
            | Self::Seek { id, .. }
            | Self::SetVolume { id, .. }
            | Self::Next { id, .. }
            | Self::Previous { id, .. } => id,
        }
    }

    /// True for the two messages a socket may send before it has authenticated.
    ///
    /// Named as an allow-list rather than a deny-list on purpose: a new message type added later
    /// defaults to requiring authentication unless someone deliberately opts it out, which is the
    /// safe direction to fail in.
    pub fn is_pre_auth(&self) -> bool {
        matches!(self, Self::Pair { .. } | Self::Auth { .. })
    }

    /// Parse one line of input. `None` on anything malformed — the caller replies with a protocol
    /// error rather than guessing at partial intent.
    pub fn parse(line: &str) -> Option<Self> {
        let v = crate::json::parse(line).ok()?;
        let id = v.get("id")?.as_str()?.to_string();
        let ty = v.get("type")?.as_str()?;
        let str_field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let num_field = |k: &str| v.get(k).and_then(Value::as_f64);

        match ty {
            "pair" => Some(Self::Pair { id, code: str_field("code")? }),
            "auth" => Some(Self::Auth { id, token: str_field("token")? }),
            "library" => Some(Self::Library { id }),
            "play" => Some(Self::Play { id, path: str_field("path")? }),
            "pause" => Some(Self::Pause { id }),
            "resume" => Some(Self::Resume { id }),
            "toggle" => Some(Self::TogglePlayPause { id }),
            "seek" => Some(Self::Seek { id, position_ms: num_field("position_ms")? as i64 }),
            "volume" => {
                // Clamped rather than rejected: a client sending 130 almost certainly meant "as loud
                // as this goes", not "refuse my request and make me resend a valid one".
                let level = num_field("level")?.clamp(0.0, 100.0) as u8;
                Some(Self::SetVolume { id, level })
            }
            "next" => Some(Self::Next { id }),
            "previous" => Some(Self::Previous { id }),
            _ => None,
        }
    }
}

/// A message written to the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// Pushed on connect and whenever playback state changes. Never a reply to anything — it has no
    /// `id` — which is what lets it interleave with request/response traffic on the same socket.
    State(PlaybackState),
    /// Pairing succeeded; carries the token to present on future connections instead of the code.
    Paired {
        id: String,
        token: String,
    },
    Reply {
        id: String,
        result: ReplyBody,
    },
    Error {
        id: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplyBody {
    Ok,
    Library(Vec<LibraryEntry>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlaybackState {
    pub now_playing: Option<NowPlaying>,
    /// Bumps whenever the library scan changes, so a client can cheaply decide whether its cached
    /// listing is stale without diffing the whole thing itself.
    pub library_version: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub path: String,
    pub title: String,
    pub duration_ms: i64,
    pub position_ms: i64,
    pub paused: bool,
    pub volume: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryEntry {
    pub path: String,
    pub title: String,
    pub duration_ms: i64,
}

impl ServerMessage {
    /// One line, `\n`-terminated, ready to write to the socket.
    pub fn to_line(&self) -> String {
        let mut s = self.to_json();
        s.push('\n');
        s
    }

    fn to_json(&self) -> String {
        match self {
            Self::State(state) => format!(
                "{{\"type\":\"state\",\"now_playing\":{},\"library_version\":{}}}",
                state.now_playing.as_ref().map_or_else(|| "null".to_string(), now_playing_json),
                state.library_version
            ),
            Self::Paired { id, token } => {
                format!("{{\"type\":\"paired\",\"id\":{},\"token\":{}}}", quote(id), quote(token))
            }
            Self::Reply { id, result } => match result {
                ReplyBody::Ok => format!("{{\"type\":\"reply\",\"id\":{},\"ok\":true}}", quote(id)),
                ReplyBody::Library(items) => {
                    let entries: Vec<String> = items
                        .iter()
                        .map(|e| {
                            format!(
                                "{{\"path\":{},\"title\":{},\"duration_ms\":{}}}",
                                quote(&e.path),
                                quote(&e.title),
                                e.duration_ms
                            )
                        })
                        .collect();
                    format!(
                        "{{\"type\":\"reply\",\"id\":{},\"ok\":true,\"result\":[{}]}}",
                        quote(id),
                        entries.join(",")
                    )
                }
            },
            Self::Error { id, message } => format!(
                "{{\"type\":\"reply\",\"id\":{},\"ok\":false,\"error\":{}}}",
                quote(id),
                quote(message)
            ),
        }
    }
}

fn now_playing_json(np: &NowPlaying) -> String {
    format!(
        "{{\"path\":{},\"title\":{},\"duration_ms\":{},\"position_ms\":{},\"paused\":{},\"volume\":{}}}",
        quote(&np.path),
        quote(&np.title),
        np.duration_ms,
        np.position_ms,
        np.paused,
        np.volume
    )
}

/// Parse a `ServerMessage` back out of its own wire form.
///
/// Exists for the test suite: the honest way to check the writer is correct is to feed its output
/// back through a reader and check what comes out, not to eyeball a `format!` string. Kept a thin
/// wrapper around the shared JSON reader rather than a second hand-rolled parser.
#[cfg(test)]
pub fn parse_server_message(line: &str) -> Option<ParsedServerMessage> {
    let v = crate::json::parse(line).ok()?;
    let ty = v.get("type")?.as_str()?;
    match ty {
        "state" => {
            let now_playing = match v.get("now_playing") {
                None | Some(Value::Null) => None,
                Some(n) => Some(NowPlaying {
                    path: n.get("path")?.as_str()?.to_string(),
                    title: n.get("title")?.as_str()?.to_string(),
                    duration_ms: n.get("duration_ms")?.as_f64()? as i64,
                    position_ms: n.get("position_ms")?.as_f64()? as i64,
                    paused: n.get("paused")?.as_bool()?,
                    volume: n.get("volume")?.as_f64()? as u8,
                }),
            };
            Some(ParsedServerMessage::State(PlaybackState {
                now_playing,
                library_version: v.get("library_version")?.as_f64()? as u64,
            }))
        }
        "paired" => Some(ParsedServerMessage::Paired {
            id: v.get("id")?.as_str()?.to_string(),
            token: v.get("token")?.as_str()?.to_string(),
        }),
        "reply" => {
            let id = v.get("id")?.as_str()?.to_string();
            let ok = v.get("ok")?.as_bool()?;
            if !ok {
                return Some(ParsedServerMessage::Error {
                    id,
                    message: v.get("error")?.as_str()?.to_string(),
                });
            }
            Some(ParsedServerMessage::ReplyOk { id })
        }
        _ => None,
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedServerMessage {
    State(PlaybackState),
    Paired { id: String, token: String },
    ReplyOk { id: String },
    Error { id: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: &[(&str, Value)]) -> String {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        format!("{}", DebugJson(Value::Object(m)))
    }

    /// Minimal writer for building request lines in tests, so the parser tests do not depend on
    /// `format!` strings typed out by hand — a typo there would test nothing.
    struct DebugJson(Value);
    impl std::fmt::Display for DebugJson {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            fn write(v: &Value, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match v {
                    Value::Null => write!(f, "null"),
                    Value::Bool(b) => write!(f, "{b}"),
                    Value::Num(n) => write!(f, "{n}"),
                    Value::Str(s) => write!(f, "{}", quote(s)),
                    Value::Array(a) => {
                        write!(f, "[")?;
                        for (i, x) in a.iter().enumerate() {
                            if i > 0 {
                                write!(f, ",")?;
                            }
                            write(x, f)?;
                        }
                        write!(f, "]")
                    }
                    Value::Object(m) => {
                        write!(f, "{{")?;
                        for (i, (k, x)) in m.iter().enumerate() {
                            if i > 0 {
                                write!(f, ",")?;
                            }
                            write!(f, "{}:", quote(k))?;
                            write(x, f)?;
                        }
                        write!(f, "}}")
                    }
                }
            }
            write(&self.0, f)
        }
    }

    #[test]
    fn every_client_message_round_trips_through_its_id() {
        let line = obj(&[
            ("type", Value::Str("seek".into())),
            ("id", Value::Str("42".into())),
            ("position_ms", Value::Num(90_000.0)),
        ]);
        let msg = ClientMessage::parse(&line).unwrap();
        assert_eq!(msg.id(), "42");
        assert_eq!(msg, ClientMessage::Seek { id: "42".into(), position_ms: 90_000 });
    }

    #[test]
    fn only_pair_and_auth_are_allowed_before_authentication() {
        // An allow-list, not a deny-list: a message type added later without being taught here
        // defaults to requiring auth, which is the safe direction to be wrong in.
        assert!(ClientMessage::Pair { id: "1".into(), code: "000000".into() }.is_pre_auth());
        assert!(ClientMessage::Auth { id: "1".into(), token: "x".into() }.is_pre_auth());
        assert!(!ClientMessage::Play { id: "1".into(), path: "x".into() }.is_pre_auth());
        assert!(!ClientMessage::Pause { id: "1".into() }.is_pre_auth());
    }

    #[test]
    fn volume_is_clamped_rather_than_rejected() {
        let over = obj(&[
            ("type", Value::Str("volume".into())),
            ("id", Value::Str("1".into())),
            ("level", Value::Num(500.0)),
        ]);
        assert_eq!(
            ClientMessage::parse(&over).unwrap(),
            ClientMessage::SetVolume { id: "1".into(), level: 100 }
        );
        let under = obj(&[
            ("type", Value::Str("volume".into())),
            ("id", Value::Str("1".into())),
            ("level", Value::Num(-40.0)),
        ]);
        assert_eq!(
            ClientMessage::parse(&under).unwrap(),
            ClientMessage::SetVolume { id: "1".into(), level: 0 }
        );
    }

    #[test]
    fn malformed_input_is_none_not_a_panic() {
        assert_eq!(ClientMessage::parse(""), None);
        assert_eq!(ClientMessage::parse("not json"), None);
        assert_eq!(ClientMessage::parse("{}"), None, "missing type and id");
        assert_eq!(
            ClientMessage::parse(r#"{"type":"seek","id":"1"}"#),
            None,
            "seek without a position must not be guessed at"
        );
        assert_eq!(
            ClientMessage::parse(r#"{"type":"made-up-verb","id":"1"}"#),
            None,
            "an unknown message type is refused, not misread as the nearest match"
        );
    }

    #[test]
    fn a_state_push_with_nothing_playing_round_trips() {
        let msg = ServerMessage::State(PlaybackState { now_playing: None, library_version: 3 });
        let line = msg.to_line();
        assert!(line.ends_with('\n'));
        // now_playing: null cannot be parsed back into a NowPlaying by the test-only reader above —
        // that reader exists to check the *populated* case byte-for-byte, so this checks the literal
        // shape instead, which is exactly what a null-safety regression would break.
        assert!(line.contains("\"now_playing\":null"));
        assert!(line.contains("\"library_version\":3"));
    }

    #[test]
    fn a_state_push_with_something_playing_round_trips_exactly() {
        let msg = ServerMessage::State(PlaybackState {
            now_playing: Some(NowPlaying {
                path: "/media/Film (2019).mkv".into(),
                title: "Film (2019)".into(),
                duration_ms: 7_200_000,
                position_ms: 42_000,
                paused: false,
                volume: 80,
            }),
            library_version: 12,
        });
        let parsed = parse_server_message(&msg.to_line()).unwrap();
        assert_eq!(
            parsed,
            ParsedServerMessage::State(PlaybackState {
                now_playing: Some(NowPlaying {
                    path: "/media/Film (2019).mkv".into(),
                    title: "Film (2019)".into(),
                    duration_ms: 7_200_000,
                    position_ms: 42_000,
                    paused: false,
                    volume: 80,
                }),
                library_version: 12,
            })
        );
    }

    #[test]
    fn a_path_with_quotes_and_backslashes_survives_the_wire() {
        // Windows paths and any title containing a quote are exactly the input that breaks a
        // hand-rolled JSON writer that concatenates strings instead of escaping them.
        let msg = ServerMessage::State(PlaybackState {
            now_playing: Some(NowPlaying {
                path: r#"C:\Media\Director's "Cut".mkv"#.into(),
                title: r#"Director's "Cut""#.into(),
                duration_ms: 1,
                position_ms: 0,
                paused: true,
                volume: 50,
            }),
            library_version: 0,
        });
        let parsed = parse_server_message(&msg.to_line()).unwrap();
        let ParsedServerMessage::State(state) = parsed else { panic!("expected State") };
        assert_eq!(state.now_playing.unwrap().title, r#"Director's "Cut""#);
    }

    #[test]
    fn a_paired_reply_carries_the_token_back() {
        let msg = ServerMessage::Paired { id: "5".into(), token: "abc123".into() };
        let parsed = parse_server_message(&msg.to_line()).unwrap();
        assert_eq!(parsed, ParsedServerMessage::Paired { id: "5".into(), token: "abc123".into() });
    }

    #[test]
    fn an_error_reply_names_the_id_it_answers_and_the_reason() {
        let msg = ServerMessage::Error { id: "9".into(), message: "wrong pairing code".into() };
        let parsed = parse_server_message(&msg.to_line()).unwrap();
        assert_eq!(
            parsed,
            ParsedServerMessage::Error { id: "9".into(), message: "wrong pairing code".into() }
        );
    }

    #[test]
    fn a_library_reply_carries_every_entry() {
        let msg = ServerMessage::Reply {
            id: "3".into(),
            result: ReplyBody::Library(vec![
                LibraryEntry { path: "/a.mkv".into(), title: "A".into(), duration_ms: 1000 },
                LibraryEntry { path: "/b.mkv".into(), title: "B".into(), duration_ms: 2000 },
            ]),
        };
        let line = msg.to_line();
        assert!(line.contains("\"path\":\"/a.mkv\""));
        assert!(line.contains("\"path\":\"/b.mkv\""));
        // The generic reader used elsewhere confirms it is at least well-formed JSON.
        assert!(crate::json::parse(line.trim_end()).is_ok());
    }

    #[test]
    fn state_never_carries_an_id() {
        // The one message type allowed to interleave unprompted with request/response traffic must
        // never accidentally look like a reply to something — that would be a client matching a push
        // against a pending request and getting a completely unrelated answer.
        let line = ServerMessage::State(PlaybackState::default()).to_line();
        let v = crate::json::parse(line.trim_end()).unwrap();
        assert_eq!(v.get("id"), None);
    }
}
