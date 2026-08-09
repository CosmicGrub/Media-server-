//! End-to-end verification of `lumen serve` against real mpv.
//!
//! Every other test of `remote::protocol` and `remote::pairing` checks logic in isolation. This is
//! the one that proves the pieces actually work assembled: a real `lumen serve` process, a raw
//! `TcpStream` standing in for a phone, pairing with the code the server printed, then driving a
//! real mpv instance through play, seek, and volume and reading real state back. It is skipped
//! rather than failed when mpv is not on `PATH`, the same convention the rest of the crate's
//! mpv-dependent tests use — this is infrastructure, not a defect, when it happens in CI on a
//! platform without mpv preinstalled.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// A tiny hand-rolled JSON line, matching exactly the shape `lumen_play::remote::protocol::ClientMessage::parse` expects. Not the `json` module's own writer — that is a private detail of the
/// binary crate, unreachable from an external integration test, and duplicating four lines of
/// `format!` here is cheaper than exposing internals just to test them from the outside.
fn request(id: &str, body: &str) -> String {
    format!("{{\"id\":\"{id}\",{body}}}\n")
}

/// One JSON object per line back from the server. Only pulls out the handful of fields the test
/// needs to assert on — this is a probe, not a second protocol implementation.
struct Reply(serde_json_lite::Map);

mod serde_json_lite {
    //! Just enough of a JSON reader to check the server's replies, independent of the crate under
    //! test's own parser — using `lumen_play`'s own JSON reader to verify `lumen_play`'s own output
    //! would not catch a bug shared by both.
    use std::collections::BTreeMap;

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // Num's payload rounds out the JSON grammar; this probe never reads it back.
    pub enum Val {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Map(BTreeMap<String, Val>),
    }
    pub type Map = BTreeMap<String, Val>;

    pub fn parse(s: &str) -> Map {
        let mut chars = s.trim().chars().peekable();
        let v = value(&mut chars);
        match v {
            Val::Map(m) => m,
            _ => BTreeMap::new(),
        }
    }

    fn skip_ws(c: &mut std::iter::Peekable<std::str::Chars>) {
        while matches!(c.peek(), Some(w) if w.is_whitespace()) {
            c.next();
        }
    }

    fn value(c: &mut std::iter::Peekable<std::str::Chars>) -> Val {
        skip_ws(c);
        match c.peek() {
            Some('{') => object(c),
            Some('"') => Val::Str(string(c)),
            Some('t') => {
                for _ in 0..4 {
                    c.next();
                }
                Val::Bool(true)
            }
            Some('f') => {
                for _ in 0..5 {
                    c.next();
                }
                Val::Bool(false)
            }
            Some('n') => {
                for _ in 0..4 {
                    c.next();
                }
                Val::Null
            }
            Some('[') => {
                c.next();
                loop {
                    skip_ws(c);
                    if c.peek() == Some(&']') {
                        c.next();
                        break;
                    }
                    value(c);
                    skip_ws(c);
                    if c.peek() == Some(&',') {
                        c.next();
                    }
                }
                Val::Null // Arrays are not needed by this probe today.
            }
            _ => {
                let mut s = String::new();
                while matches!(c.peek(), Some(ch) if !",}] \t\n".contains(*ch)) {
                    s.push(c.next().unwrap());
                }
                Val::Num(s.parse().unwrap_or(0.0))
            }
        }
    }

    fn string(c: &mut std::iter::Peekable<std::str::Chars>) -> String {
        c.next(); // opening quote
        let mut s = String::new();
        while let Some(ch) = c.next() {
            match ch {
                '"' => break,
                '\\' => {
                    if let Some(esc) = c.next() {
                        s.push(match esc {
                            'n' => '\n',
                            't' => '\t',
                            '"' => '"',
                            '\\' => '\\',
                            other => other,
                        });
                    }
                }
                other => s.push(other),
            }
        }
        s
    }

    fn object(c: &mut std::iter::Peekable<std::str::Chars>) -> Val {
        c.next(); // '{'
        let mut m = BTreeMap::new();
        loop {
            skip_ws(c);
            if c.peek() == Some(&'}') {
                c.next();
                break;
            }
            skip_ws(c);
            let key = string(c);
            skip_ws(c);
            c.next(); // ':'
            let v = value(c);
            m.insert(key, v);
            skip_ws(c);
            if c.peek() == Some(&',') {
                c.next();
            }
        }
        Val::Map(m)
    }
}

impl Reply {
    fn parse(line: &str) -> Self {
        Self(serde_json_lite::parse(line))
    }
    fn str(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(serde_json_lite::Val::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }
    fn bool(&self, key: &str) -> Option<bool> {
        match self.0.get(key) {
            Some(serde_json_lite::Val::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    fn ty(&self) -> Option<String> {
        self.str("type")
    }
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let anchor = 0u8;
        let d = std::env::temp_dir().join(format!(
            "lumen-serve-it-{tag}-{}-{:x}",
            std::process::id(),
            std::ptr::from_ref(&anchor) as usize
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mpv_on_path() -> bool {
    std::process::Command::new("mpv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A short, real, playable file — encoded once per test run rather than checked into the repo,
/// matching how the rest of the crate's mpv-dependent tests source their media.
fn encode_probe_file(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("Probe.mkv");
    let status = std::process::Command::new("mpv")
        .args([
            "av://lavfi:testsrc2=size=160x90:rate=8:duration=3",
            "--audio-file=av://lavfi:sine=frequency=440:duration=3",
            &format!("--o={}", path.display()),
            "--ovc=libx264",
            "--ovcopts=preset=ultrafast",
            "--oac=aac",
            "--msg-level=all=error",
        ])
        .status()
        .expect("mpv must be runnable to encode the probe file");
    assert!(status.success(), "encoding the probe file failed");
    assert!(path.exists());
    path
}

#[test]
fn a_client_pairs_plays_seeks_and_reads_state_back_from_real_mpv() {
    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("basic");
    let file = encode_probe_file(&dir.0);

    // A distinct, unlikely-to-collide port per test run, so this test can run alongside CI's other
    // suites without fighting anyone else for a fixed port number.
    let port = 17000 + (std::process::id() % 4000) as u16;
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = std::process::Command::new(bin)
        .args([
            "serve",
            dir.0.to_str().unwrap(),
            "--port",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
            "--",
            "--vo=null", // No display in CI or this container; audio/video pipeline still runs.
            // No audio device either. Without this, mpv's `loadfile` blocks on Windows while it
            // probes WASAPI for a device that does not exist on a headless runner -- long enough to
            // trip `run_command`'s 5-second reply timeout in remote/server.rs, which reads as "the
            // player is not responding" even though mpv is fine and would have answered eventually.
            // Linux/macOS were never affected (no such probe stalls audio-less there), which is
            // exactly why this was invisible until this test first ran for real on Windows CI.
            "--ao=null",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("lumen must be runnable");

    // The pairing code is on stdout, the first useful thing the server prints. Read lines until it
    // shows up rather than sleeping a fixed guess, which would be either too slow or too flaky.
    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut code = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        if let Some(rest) = line.strip_prefix("pairing code: ") {
            code = Some(rest.split_whitespace().next().unwrap().to_string());
            break;
        }
    }
    let code = code.expect("the server must print a pairing code on startup");
    assert_eq!(code.len(), 6, "pairing code should be six digits, got {code:?}");

    // Give the listener a moment to actually be accepting connections — it starts after the
    // pairing code is printed, so there is a real (if short) window between the two.
    let mut stream = connect_with_retry(port, Duration::from_secs(10));
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    // Pair.
    stream
        .write_all(request("1", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let paired = read_reply(&mut reader);
    assert_eq!(
        paired.ty().as_deref(),
        Some("paired"),
        "expected a paired reply, got {:?}",
        paired.0
    );
    let token = paired.str("token").expect("a paired reply must carry a token");
    assert_eq!(token.len(), 32, "token should be 32 hex characters");

    // A wrong code afterwards must fail — the code was consumed by the successful pairing above and
    // must not be replayable.
    let mut second = connect_with_retry(port, Duration::from_secs(5));
    let mut second_reader = BufReader::new(second.try_clone().unwrap());
    second
        .write_all(request("x", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let replay = read_reply(&mut second_reader);
    assert_eq!(replay.bool("ok"), Some(false), "a consumed pairing code must not work twice");
    drop(second);

    // Play the real file.
    let escaped = file.to_str().unwrap().replace('\\', "\\\\").replace('"', "\\\"");
    stream
        .write_all(request("2", &format!("\"type\":\"play\",\"path\":\"{escaped}\"")).as_bytes())
        .unwrap();
    let play_reply = read_reply(&mut reader);
    assert_eq!(play_reply.bool("ok"), Some(true), "play must be accepted: {:?}", play_reply.0);

    // A state push should arrive on its own — nobody asked for it — naming the file now playing.
    // Read lines until a `state` message names a path (an earlier idle `state` with nothing playing
    // may arrive first, since the driver thread polls on its own schedule).
    let mut saw_playing = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let msg = read_message(&mut reader);
        if msg.ty().as_deref() == Some("state") {
            if let Some(serde_json_lite::Val::Map(np)) = msg.0.get("now_playing") {
                if np.get("path").is_some() {
                    saw_playing = true;
                    break;
                }
            }
        }
    }
    assert!(saw_playing, "expected an unprompted state push naming the file now playing");

    // Seek and set the volume; both must be accepted by the real mpv process, not just parsed.
    stream.write_all(request("3", "\"type\":\"seek\",\"position_ms\":1500").as_bytes()).unwrap();
    assert_eq!(read_reply(&mut reader).bool("ok"), Some(true));

    stream.write_all(request("4", "\"type\":\"volume\",\"level\":40").as_bytes()).unwrap();
    assert_eq!(read_reply(&mut reader).bool("ok"), Some(true));

    // An unauthenticated second connection must be refused any command other than pair/auth — this
    // is the one behaviour that, if broken, means anyone on the LAN controls the player.
    let mut stranger = connect_with_retry(port, Duration::from_secs(5));
    let mut stranger_reader = BufReader::new(stranger.try_clone().unwrap());
    stranger.write_all(request("5", "\"type\":\"pause\"").as_bytes()).unwrap();
    let refused = read_reply(&mut stranger_reader);
    assert_eq!(
        refused.bool("ok"),
        Some(false),
        "an unauthenticated socket must not control playback"
    );

    // A returning client authenticates with the token rather than the (already consumed) code.
    stranger
        .write_all(request("6", &format!("\"type\":\"auth\",\"token\":\"{token}\"")).as_bytes())
        .unwrap();
    let authed = read_reply(&mut stranger_reader);
    assert_eq!(
        authed.bool("ok"),
        Some(true),
        "the token from pairing must work on a new connection"
    );

    let _ = server.kill();
    let _ = server.wait();
}

/// Skips any `state` lines to find the reply matching what was just sent — the two interleave on the
/// same socket by design, and a naive "read one line, assume it is the reply" would be exactly the
/// bug the id-echoing protocol exists to make impossible to write correctly by accident.
fn read_reply(reader: &mut BufReader<TcpStream>) -> Reply {
    loop {
        let msg = read_message(reader);
        if msg.ty().as_deref() != Some("state") {
            return msg;
        }
    }
}

fn read_message(reader: &mut BufReader<TcpStream>) -> Reply {
    let mut line = String::new();
    reader.read_line(&mut line).expect("the server must not have closed the connection");
    assert!(!line.is_empty(), "connection closed while a message was expected");
    Reply::parse(&line)
}

fn connect_with_retry(port: u16, timeout: Duration) -> TcpStream {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => return s,
            Err(e) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = e;
            }
            Err(e) => panic!("could not connect to the server on port {port}: {e}"),
        }
    }
}
