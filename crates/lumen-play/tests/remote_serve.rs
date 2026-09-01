//! End-to-end verification of `lumen serve` against real mpv.
//!
//! Every other test of `remote::protocol` and `remote::pairing` checks logic in isolation. This is
//! the one that proves the pieces actually work assembled: a real `lumen serve` process, a TLS
//! connection standing in for a phone -- pinning the fingerprint the server prints, the same way a
//! real client is meant to on first pair -- pairing with the code the server printed, then driving a
//! real mpv instance through play, seek, and volume and reading real state back. It is skipped
//! rather than failed when mpv is not on `PATH`, the same convention the rest of the crate's
//! mpv-dependent tests use — this is infrastructure, not a defect, when it happens in CI on a
//! platform without mpv preinstalled.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};

/// One TLS connection standing in for a phone. `rustls::StreamOwned` implements both `Write` and
/// `BufRead` itself (see `remote/tls.rs`'s doc on why the production server relies on the same
/// thing) — no separate `BufReader` wrapper needed, and no `try_clone` split either, since this test
/// only ever writes a request and then reads its reply, never both at once.
type ClientTls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// Verifies a server's certificate against one pinned fingerprint, exactly the way a real client is
/// meant to the moment it is shown a fingerprint alongside a pairing code (trust-on-first-use — see
/// `remote/tls.rs`'s module doc). No hostname or CA chain is ever checked; the fingerprint is this
/// server's entire identity as far as this test (and a real client) is concerned.
#[derive(Debug)]
struct PinnedFingerprint(String);

impl ServerCertVerifier for PinnedFingerprint {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let digest = Sha256::digest(end_entity.as_ref());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        if hex == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "certificate fingerprint mismatch: pinned {}, got {hex}",
                self.0
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

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
    /// A nested object field — `Health`'s reply carries its payload under `result` rather than at the
    /// top level, the same shape `Library`'s reply already uses.
    fn map(&self, key: &str) -> Option<serde_json_lite::Map> {
        match self.0.get(key) {
            Some(serde_json_lite::Val::Map(m)) => Some(m.clone()),
            _ => None,
        }
    }
}

/// A numeric field inside any parsed object, not just a top-level [`Reply`] — needed for `Health`'s
/// `result` object, which [`Reply`]'s own accessors do not reach into.
fn num_in(m: &serde_json_lite::Map, key: &str) -> Option<f64> {
    match m.get(key) {
        Some(serde_json_lite::Val::Num(n)) => Some(*n),
        _ => None,
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

/// Kills the wrapped `lumen serve` child process (and, via `Child::kill`, its own child mpv) on drop
/// -- including when a panicking assertion unwinds through the scope holding it, which a bare `Child`
/// never does (`std` never kills a child process just because its handle was dropped). Without this,
/// a single failed assertion anywhere between spawning the server and this test's own explicit
/// `kill()`/`wait()` at the end leaks a `lumen serve` process still bound to this test's port --
/// exactly the kind of leak that turns one flaky assertion into an unrelated "address already in use"
/// failure in a later run. `Deref`/`DerefMut` to `Child` so every existing call site
/// (`.stdout.take()`, `.stderr.take()`, `.kill()`, `.wait()`) keeps working unchanged.
struct KillOnDrop(std::process::Child);
impl std::ops::Deref for KillOnDrop {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

    // `dirs_next_config_dir` (pairing tokens, the pinned TLS cert, its expiry sidecar) resolves from
    // `XDG_CONFIG_HOME`/`HOME`/`APPDATA` depending on platform -- pointed at a fresh directory under
    // this test's own `TempDir` rather than left to fall through to whatever this machine's real
    // `~/.config/lumen` happens to already hold. Without this, a cert left over from an earlier run
    // (generated before, say, expiry tracking existed) gets silently reused, and this test's own
    // assertions about a *freshly generated* cert would be exercising leftover state instead.
    let config_dir = dir.0.join("config");
    // A distinct, unlikely-to-collide port per test run, so this test can run alongside CI's other
    // suites without fighting anyone else for a fixed port number.
    let port = 17000 + (std::process::id() % 4000) as u16;
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = KillOnDrop(
        std::process::Command::new(bin)
            .args([
                "serve",
                dir.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--",
                "--vo=null", // No display in CI or this container; audio/video pipeline still runs.
                "--ao=null", // No audio device either, for the same reason.
                // `spawn_idle_mpv` hardcodes `--force-window=yes` so a real desktop `lumen serve` shows a
                // window the moment it starts, before any file is loaded. `--vo=null` alone does not
                // cancel that: mpv still tries to create the window, and on a Windows CI runner (which has
                // no interactive window station — it runs as a service session) that creation call can
                // block indefinitely, well before mpv ever reaches its IPC command loop. That is why every
                // property/command sent over the pipe just sat there un-drained: mpv was never getting far
                // enough into startup to read it, not any bug in the client-side IPC or driver code. Extra
                // args come last and win (see `spawn_idle_mpv`'s doc comment), so this cancels it.
                "--force-window=no",
            ])
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("APPDATA", &config_dir)
            .env("HOME", &config_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("lumen must be runnable"),
    );

    // mpv's own errors -- an unopenable file, a missing decoder, a rejected IPC command -- land on
    // the server's stderr and were previously discarded outright, so a failure here had no more to
    // go on than "the player is not responding". Forwarded live rather than buffered and printed on
    // panic: the reader thread outlives the `Command` handle, and a buffer nothing ever flushes is
    // no more useful than /dev/null.
    if let Some(stderr) = server.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[lumen serve stderr] {line}");
            }
        });
    }

    // The pairing code is on stdout, the first useful thing the server prints. Read lines until it
    // shows up rather than sleeping a fixed guess, which would be either too slow or too flaky.
    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut code = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        // Every line the server prints before this point was previously discarded silently unless
        // it happened to be the one this loop was looking for -- which swallowed whole diagnostics
        // (`drive_mpv`'s own startup/timing lines among them) before a human ever saw them.
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("pairing code: ") {
            code = Some(rest.split_whitespace().next().unwrap().to_string());
            break;
        }
    }
    let code = code.expect("the server must print a pairing code on startup");
    assert_eq!(code.len(), 6, "pairing code should be six digits, got {code:?}");

    // The TLS fingerprint is on stdout too, right after the pairing code -- a real client pins this
    // the same moment it is shown the code (see remote/tls.rs's module doc). Keep reading the same
    // `lines` iterator rather than starting a new deadline loop, so the two lines cannot be confused
    // even if the server's print order ever changes.
    let mut fingerprint = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("tls fingerprint: ") {
            fingerprint = Some(rest.split("  ").next().unwrap().to_string());
            break;
        }
    }
    let fingerprint = fingerprint.expect("the server must print a TLS fingerprint on startup");

    // Everything the server logs after this point (including `drive_mpv`'s own per-command timing)
    // still has to be drained, or two real bugs follow: the lines themselves are invisible to this
    // test's own output, and -- worse -- once the OS pipe buffer behind `Stdio::piped()` fills, the
    // server's next `println!` blocks waiting for a reader that will never come back, which would
    // make the server itself appear to hang for a reason that has nothing to do with mpv at all.
    std::thread::spawn(move || {
        for line in lines.map_while(Result::ok) {
            println!("[lumen serve stdout] {line}");
        }
    });

    rustls::crypto::ring::default_provider().install_default().ok();

    // Give the listener a moment to actually be accepting connections — it starts after the
    // pairing code is printed, so there is a real (if short) window between the two.
    let mut tls = connect_tls(port, &fingerprint, Duration::from_secs(10));

    // Pair.
    tls.write_all(request("1", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let paired = read_reply(&mut tls);
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
    let mut second = connect_tls(port, &fingerprint, Duration::from_secs(5));
    second
        .write_all(request("x", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let replay = read_reply(&mut second);
    assert_eq!(replay.bool("ok"), Some(false), "a consumed pairing code must not work twice");
    drop(second);

    // Play the real file.
    let escaped = file.to_str().unwrap().replace('\\', "\\\\").replace('"', "\\\"");
    tls.write_all(request("2", &format!("\"type\":\"play\",\"path\":\"{escaped}\"")).as_bytes())
        .unwrap();
    let play_reply = read_reply(&mut tls);
    assert_eq!(play_reply.bool("ok"), Some(true), "play must be accepted: {:?}", play_reply.0);

    // A state push should arrive on its own — nobody asked for it — naming the file now playing.
    // Read lines until a `state` message names a path (an earlier idle `state` with nothing playing
    // may arrive first, since the driver thread polls on its own schedule).
    let mut saw_playing = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let msg = read_message(&mut tls);
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
    tls.write_all(request("3", "\"type\":\"seek\",\"position_ms\":1500").as_bytes()).unwrap();
    assert_eq!(read_reply(&mut tls).bool("ok"), Some(true));

    tls.write_all(request("4", "\"type\":\"volume\",\"level\":40").as_bytes()).unwrap();
    assert_eq!(read_reply(&mut tls).bool("ok"), Some(true));

    // An unauthenticated second connection must be refused any command other than pair/auth — this
    // is the one behaviour that, if broken, means anyone on the LAN controls the player.
    let mut stranger = connect_tls(port, &fingerprint, Duration::from_secs(5));
    stranger.write_all(request("5", "\"type\":\"pause\"").as_bytes()).unwrap();
    let refused = read_reply(&mut stranger);
    assert_eq!(
        refused.bool("ok"),
        Some(false),
        "an unauthenticated socket must not control playback"
    );

    // A returning client authenticates with the token rather than the (already consumed) code.
    stranger
        .write_all(request("6", &format!("\"type\":\"auth\",\"token\":\"{token}\"")).as_bytes())
        .unwrap();
    let authed = read_reply(&mut stranger);
    assert_eq!(
        authed.bool("ok"),
        Some(true),
        "the token from pairing must work on a new connection"
    );

    // Health -- docs/15 §D. Two connections (`tls` and `stranger`) are authenticated and still open
    // at this point, so the count must reflect both, not just the one that asked.
    tls.write_all(request("7", "\"type\":\"health\"").as_bytes()).unwrap();
    let health = read_reply(&mut tls);
    assert_eq!(health.bool("ok"), Some(true), "health must be accepted: {:?}", health.0);
    let result = health.map("result").expect("a health reply must carry a result object");

    let roundtrip = num_in(&result, "mpv_roundtrip_ms")
        .expect("mpv_roundtrip_ms must be a real number for a live, responsive mpv");
    assert!(
        roundtrip < 5000.0,
        "a live mpv should answer well under the 5s command deadline: {roundtrip}ms"
    );

    let cert_expiry = num_in(&result, "tls_cert_expires_in_secs")
        .expect("a freshly generated certificate must report a real expiry, not null");
    assert!(
        cert_expiry > 0.0,
        "a freshly generated cert must not already be expired: {cert_expiry}s"
    );

    let disk = num_in(&result, "free_disk_bytes")
        .expect("free disk space on a real, existing temp directory must be a real number");
    assert!(disk > 0.0, "a real volume must report some free space: {disk}");

    let paired =
        num_in(&result, "paired_client_count").expect("paired_client_count must be a number");
    assert!(paired >= 2.0, "both `tls` and `stranger` are authenticated and still open: {paired}");

    // This library was only ever scanned in memory by `lumen serve`, never reindexed via `lumen
    // reindex`/`lumen verify` -- honestly unknown, not fabricated as "just now".
    assert!(
        matches!(
            result.get("library_last_indexed_unix_secs"),
            None | Some(serde_json_lite::Val::Null)
        ),
        "a never-reindexed library must report unknown freshness, got {:?}",
        result.get("library_last_indexed_unix_secs")
    );

    // Tier 3a: HTTP media streaming, multiplexed onto the same TLS listener and pinned fingerprint --
    // no second port, no second certificate to trust. A real GET, authenticated with the token pairing
    // already issued, must return the file's real bytes with a container-derived Content-Type.
    let full_bytes = std::fs::read(&file).unwrap();
    let encoded_path = file.to_str().unwrap().replace(' ', "%20");

    let mut whole = connect_tls(port, &fingerprint, Duration::from_secs(5));
    whole
        .write_all(
            format!("GET /stream/{encoded_path}?token={token} HTTP/1.1\r\nHost: x\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let (status, headers, body) = read_http_response(&mut whole);
    assert_eq!(status, 200, "a plain GET with a valid token must succeed");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("matroska")),
        "expected a Matroska content-type, got {headers:?}"
    );
    assert_eq!(body, full_bytes, "the streamed bytes must match the file on disk exactly");

    // A ranged request -- the mechanism every real player uses to seek -- gets back exactly the slice
    // asked for, as 206, not the whole file.
    let mut ranged = connect_tls(port, &fingerprint, Duration::from_secs(5));
    ranged
        .write_all(
            format!(
                "GET /stream/{encoded_path}?token={token} HTTP/1.1\r\nHost: x\r\n\
                 Range: bytes=0-99\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let (status, headers, body) = read_http_response(&mut ranged);
    assert_eq!(status, 206, "a Range request must be answered as Partial Content");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-range") && v.starts_with("bytes 0-99/")),
        "expected a Content-Range header naming the served slice, got {headers:?}"
    );
    assert_eq!(body, &full_bytes[0..100], "a ranged GET must return exactly that slice");

    // No token at all -- the whole point of putting streaming behind the same auth as control -- must
    // be refused, not silently served.
    let mut anon = connect_tls(port, &fingerprint, Duration::from_secs(5));
    anon.write_all(format!("GET /stream/{encoded_path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .unwrap();
    let (status, _, _) = read_http_response(&mut anon);
    assert_eq!(status, 401, "streaming without a token must be refused");

    // Tier 3d: the VR cinema shell is the same static bytes for anyone, no token required to fetch
    // the page itself -- only the `/stream/<path>` URL it builds client-side is ever checked.
    let mut vr = connect_tls(port, &fingerprint, Duration::from_secs(5));
    vr.write_all(b"GET /vr HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let (status, headers, body) = read_http_response(&mut vr);
    assert_eq!(status, 200, "the VR shell needs no token to fetch");
    assert!(
        headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("html")),
        "expected an HTML content-type, got {headers:?}"
    );
    let page = String::from_utf8(body).expect("the VR page must be valid UTF-8");
    assert!(page.contains("requestSession"), "the page must actually request an XR session");
    assert!(page.contains("/stream/"), "the page must build a /stream/ URL, not invent a new one");

    // Kept explicit even though `KillOnDrop` will do this again on scope exit regardless -- a
    // double kill()/wait() is harmless (both discard their `Result`), and an explicit stop here
    // reaps the process the moment the test's real work is done rather than waiting for drop order.
    let _ = server.kill();
    let _ = server.wait();
}

/// `docs/15` §A's manual-trigger MVP, proven against a real `lumen serve` process: `library_version`
/// starts at 0 (unchanged from before this existed), a file dropped into the library after startup is
/// invisible until a real `rescan` request re-walks the tree, and the version that comes back in the
/// `Rescan` reply is the same one the very next unprompted `state` push carries -- not a second,
/// independently-tracked number that could drift from what clients actually see.
#[test]
fn rescan_makes_library_version_real_and_reflects_a_newly_added_file() {
    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("rescan");
    let first = encode_probe_file(&dir.0);
    let config_dir = dir.0.join("config");
    let port = 21500 + (std::process::id() % 4000) as u16;
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = KillOnDrop(
        std::process::Command::new(bin)
            .args([
                "serve",
                dir.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--",
                "--vo=null",
                "--ao=null",
                "--force-window=no",
            ])
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("APPDATA", &config_dir)
            .env("HOME", &config_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("lumen must be runnable"),
    );

    if let Some(stderr) = server.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[lumen serve stderr] {line}");
            }
        });
    }

    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut code = None;
    let mut fingerprint = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("pairing code: ") {
            code = Some(rest.split_whitespace().next().unwrap().to_string());
        }
        if let Some(rest) = line.strip_prefix("tls fingerprint: ") {
            fingerprint = Some(rest.split("  ").next().unwrap().to_string());
        }
        if code.is_some() && fingerprint.is_some() {
            break;
        }
    }
    let code = code.expect("the server must print a pairing code on startup");
    let fingerprint = fingerprint.expect("the server must print a TLS fingerprint on startup");
    std::thread::spawn(move || {
        for line in lines.map_while(Result::ok) {
            println!("[lumen serve stdout] {line}");
        }
    });

    rustls::crypto::ring::default_provider().install_default().ok();
    let mut tls = connect_tls(port, &fingerprint, Duration::from_secs(10));
    tls.write_all(request("1", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let paired = read_reply(&mut tls);
    assert_eq!(paired.ty().as_deref(), Some("paired"));

    // The very first unprompted state push must carry the same `library_version: 0` every state push
    // did before this feature existed -- a server that has never been asked to rescan must not report
    // a version that implies it already re-walked something on its own.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_initial_version = None;
    while std::time::Instant::now() < deadline {
        let msg = read_message(&mut tls);
        if msg.ty().as_deref() == Some("state") {
            saw_initial_version = num_in(&msg.0, "library_version");
            break;
        }
    }
    assert_eq!(
        saw_initial_version,
        Some(0.0),
        "library_version must start at 0, unchanged from before a rescan was ever requested"
    );

    // Drop a second real file into the library while the server is already running -- exactly the
    // scenario `server.rs`'s own "one snapshot taken at startup, never refreshed" limitation named.
    let second = encode_probe_file2(&dir.0);
    assert_ne!(second, first, "the two probe files must actually be distinct paths");

    tls.write_all(request("2", "\"type\":\"rescan\"").as_bytes()).unwrap();
    let rescan_reply = read_reply(&mut tls);
    assert_eq!(
        rescan_reply.bool("ok"),
        Some(true),
        "rescan must be accepted: {:?}",
        rescan_reply.0
    );
    let result = rescan_reply.map("result").expect("a rescan reply must carry a result object");
    assert_eq!(
        num_in(&result, "file_count"),
        Some(2.0),
        "the fresh walk must see both the original and the newly added file"
    );
    assert_eq!(
        num_in(&result, "library_version"),
        Some(1.0),
        "the first rescan must bump the version from 0 to 1"
    );

    // The very next state push must carry the same version the rescan reply just reported -- a client
    // watching state pushes alone (never issuing its own rescan) must see the same number a client
    // that requested the rescan directly was told, not a second, independently-tracked value.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_bumped_version = None;
    while std::time::Instant::now() < deadline {
        let msg = read_message(&mut tls);
        if msg.ty().as_deref() == Some("state") {
            let v = num_in(&msg.0, "library_version");
            if v == Some(1.0) {
                saw_bumped_version = v;
                break;
            }
        }
    }
    assert_eq!(
        saw_bumped_version,
        Some(1.0),
        "the next state push must carry the version the rescan reply just reported"
    );

    // A second rescan with nothing new on disk still bumps the version -- this is a re-walk trigger,
    // not a diff against the previous run (that finer-grained tracking is the larger, deliberately
    // deferred `lumen-index`-backed engine `docs/15` §A describes, not this).
    tls.write_all(request("3", "\"type\":\"rescan\"").as_bytes()).unwrap();
    let second_rescan = read_reply(&mut tls);
    let result = second_rescan.map("result").expect("a rescan reply must carry a result object");
    assert_eq!(num_in(&result, "file_count"), Some(2.0), "still the same two real files");
    assert_eq!(
        num_in(&result, "library_version"),
        Some(2.0),
        "every completed rescan bumps the version, changed or not"
    );

    // An unauthenticated socket must not be able to trigger a filesystem walk any more than it can
    // control playback -- the same posture the existing test already proves for `pause`.
    let mut stranger = connect_tls(port, &fingerprint, Duration::from_secs(5));
    stranger.write_all(request("4", "\"type\":\"rescan\"").as_bytes()).unwrap();
    let refused = read_reply(&mut stranger);
    assert_eq!(
        refused.bool("ok"),
        Some(false),
        "an unauthenticated socket must not trigger a rescan"
    );

    let _ = server.kill();
    let _ = server.wait();
}

/// A second, distinctly-named real encoded clip, for the rescan test's "a file appeared after startup"
/// scenario -- kept separate from `encode_probe_file` rather than parameterizing it, since every other
/// call site wants exactly `Probe.mkv` and gains nothing from a filename argument threaded through it.
fn encode_probe_file2(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("SecondProbe.mkv");
    let status = std::process::Command::new("mpv")
        .args([
            "av://lavfi:testsrc2=size=160x90:rate=8:duration=2",
            "--audio-file=av://lavfi:sine=frequency=220:duration=2",
            &format!("--o={}", path.display()),
            "--ovc=libx264",
            "--ovcopts=preset=ultrafast",
            "--oac=aac",
            "--msg-level=all=error",
        ])
        .status()
        .expect("mpv must be runnable to encode the second probe file");
    assert!(status.success(), "encoding the second probe file failed");
    assert!(path.exists());
    path
}

/// Connect over TCP, then complete a TLS handshake pinned to `fingerprint` — the same trust-on-first-
/// use a real client performs, not a bypass of it. `ServerName` is required by the API but never
/// actually checked: [`PinnedFingerprint`] verifies the certificate by its hash alone.
fn connect_tls(port: u16, fingerprint: &str, timeout: Duration) -> ClientTls {
    let tcp = connect_with_retry(port, timeout);
    let pinned = fingerprint.replace(':', "").to_lowercase();
    let verifier = Arc::new(PinnedFingerprint(pinned));
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let name = ServerName::try_from("lumen-serve").unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(config), name)
        .expect("a valid pinned-verifier config must build a client connection");
    rustls::StreamOwned::new(conn, tcp)
}

/// Skips any `state` lines to find the reply matching what was just sent — the two interleave on the
/// same socket by design, and a naive "read one line, assume it is the reply" would be exactly the
/// bug the id-echoing protocol exists to make impossible to write correctly by accident.
fn read_reply(tls: &mut ClientTls) -> Reply {
    loop {
        let msg = read_message(tls);
        if msg.ty().as_deref() != Some("state") {
            return msg;
        }
    }
}

fn read_message(tls: &mut ClientTls) -> Reply {
    let mut line = String::new();
    tls.read_line(&mut line).expect("the server must not have closed the connection");
    assert!(!line.is_empty(), "connection closed while a message was expected");
    Reply::parse(&line)
}

/// Read one full HTTP/1.1 response -- status code, headers, and a body read to exactly
/// `Content-Length` (or nothing, if the header is absent). A minimal, purpose-built reader, the same
/// "test independently of the crate's own parser" reasoning [`serde_json_lite`] states for the JSON
/// side of this file.
fn read_http_response(tls: &mut ClientTls) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = tls.read(&mut chunk).expect("reading the HTTP response must not fail");
        assert!(n > 0, "connection closed before a complete header block arrived");
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = std::str::from_utf8(&buf[..header_end]).expect("headers must be valid UTF-8");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("a response must have a status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("a status line must carry a code")
        .parse()
        .expect("the status code must be numeric");

    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v.parse::<usize>().ok();
        }
        headers.push((k, v));
    }

    let mut body = buf[header_end..].to_vec();
    let want = content_length.unwrap_or(0);
    while body.len() < want {
        let n = tls.read(&mut chunk).expect("reading the HTTP body must not fail");
        assert!(n > 0, "connection closed before the full body arrived");
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(want);
    (status, headers, body)
}

/// Tier 5 integration coverage: HLS delivery wired into `lumen serve`'s HTTP surface (see
/// `remote::server::hls`). A real `ffmpeg` binary is not required -- a tiny fake shell script stands
/// in for it, the same "spawn a real subprocess, verify against real files it actually wrote" trick
/// `lumen-segment`'s own `command.rs` tests already use, pointed at via `LUMEN_FFMPEG` (see
/// `ffmpegbin::find`). Real mpv is still required: `server::run` unconditionally spawns an idle mpv on
/// startup regardless of whether any HLS route is ever hit, so this test is skipped under the same
/// convention as every other mpv-dependent test in this crate when mpv is not on `PATH`.
#[cfg(unix)]
#[test]
fn hls_playlist_and_segments_are_generated_lazily_cached_and_authenticated() {
    use std::os::unix::fs::PermissionsExt;

    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("hls");
    let outside = TempDir::new("hls-outside");

    // Three dummy sources. Their bytes are never read by ffmpeg -- the fake script below ignores its
    // real input entirely and always writes the same fixed output -- only their existence, size, and
    // mtime matter, since those alone feed `hls::cache_key`.
    let source_a = dir.0.join("A.mkv");
    let source_b = dir.0.join("B.mkv");
    let source_c = dir.0.join("C.mkv");
    std::fs::write(&source_a, b"source a").unwrap();
    std::fs::write(&source_b, b"source b").unwrap();
    std::fs::write(&source_c, b"source c").unwrap();
    let outsider = outside.0.join("Secret.mkv");
    std::fs::write(&outsider, b"not in the library").unwrap();

    // `$init` (the value following `-hls_fmp4_init_filename`) is a bare file name, not a path --
    // `lumen_segment::command::build_command` gives ffmpeg only "init.mp4", matching a real ffmpeg
    // build's own behavior (confirmed against ffmpeg 6.1.1): unlike `-hls_segment_filename` and the
    // playlist path, which it honors as given even when absolute, its HLS muxer resolves
    // `-hls_fmp4_init_filename` relative to the output's own directory regardless -- an absolute
    // value there is naively concatenated onto that directory rather than replacing it, which is
    // exactly the bug a real-ffmpeg test in `lumen-segment` now exists to catch. This fake script
    // mirrors that same resolution rule (`$dir/$init`) rather than treating `$init` as standalone.
    let log_path = dir.0.join("ffmpeg-invocations.log");
    let fake_ffmpeg = dir.0.join("fake-ffmpeg.sh");
    std::fs::write(
        &fake_ffmpeg,
        format!(
            "#!/bin/sh\n\
             echo invoked >> \"{log}\"\n\
             prev=\"\"\n\
             init=\"\"\n\
             for a in \"$@\"; do\n\
             \x20\x20if [ \"$prev\" = \"-hls_fmp4_init_filename\" ]; then init=\"$a\"; fi\n\
             \x20\x20prev=\"$a\"\n\
             \x20\x20last=\"$a\"\n\
             done\n\
             dir=$(dirname \"$last\")\n\
             printf 'seg0' > \"$dir/seg_00000.m4s\"\n\
             printf 'seg1' > \"$dir/seg_00001.m4s\"\n\
             printf 'seg2' > \"$dir/seg_00002.m4s\"\n\
             printf 'init' > \"$dir/$init\"\n\
             printf '#EXTM3U\\n' > \"$last\"\n\
             printf '#EXT-X-VERSION:7\\n' >> \"$last\"\n\
             printf '#EXT-X-TARGETDURATION:6\\n' >> \"$last\"\n\
             printf '#EXTINF:6.000,\\n' >> \"$last\"\n\
             printf 'seg_00000.m4s\\n' >> \"$last\"\n\
             printf '#EXTINF:6.000,\\n' >> \"$last\"\n\
             printf 'seg_00001.m4s\\n' >> \"$last\"\n\
             printf '#EXTINF:3.000,\\n' >> \"$last\"\n\
             printf 'seg_00002.m4s\\n' >> \"$last\"\n\
             printf '#EXT-X-ENDLIST\\n' >> \"$last\"\n\
             exit 0\n",
            log = log_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

    let invocation_count = |log: &std::path::Path| -> usize {
        std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
    };

    let config_dir = dir.0.join("config");
    // A disjoint port range from the main pairing/playback test above (17000..20999), so both tests
    // can run concurrently in the same `cargo test` process without contending for a listener.
    let port = 21000 + (std::process::id() % 4000) as u16;
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = KillOnDrop(
        std::process::Command::new(bin)
            .args([
                "serve",
                dir.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--",
                "--vo=null",
                "--ao=null",
                "--force-window=no",
            ])
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("APPDATA", &config_dir)
            .env("HOME", &config_dir)
            .env("LUMEN_FFMPEG", &fake_ffmpeg)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("lumen must be runnable"),
    );

    if let Some(stderr) = server.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[lumen serve stderr] {line}");
            }
        });
    }

    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut code = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("pairing code: ") {
            code = Some(rest.split_whitespace().next().unwrap().to_string());
            break;
        }
    }
    let code = code.expect("the server must print a pairing code on startup");

    let mut fingerprint = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("tls fingerprint: ") {
            fingerprint = Some(rest.split("  ").next().unwrap().to_string());
            break;
        }
    }
    let fingerprint = fingerprint.expect("the server must print a TLS fingerprint on startup");

    std::thread::spawn(move || {
        for line in lines.map_while(Result::ok) {
            println!("[lumen serve stdout] {line}");
        }
    });

    rustls::crypto::ring::default_provider().install_default().ok();

    let mut tls = connect_tls(port, &fingerprint, Duration::from_secs(10));
    tls.write_all(request("1", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let paired = read_reply(&mut tls);
    assert_eq!(
        paired.ty().as_deref(),
        Some("paired"),
        "expected a paired reply, got {:?}",
        paired.0
    );
    let token = paired.str("token").expect("a paired reply must carry a token");
    drop(tls);

    let enc = |p: &std::path::Path| p.to_str().unwrap().replace(' ', "%20");

    // A missing/invalid token is refused before any generation is even attempted.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /hls/notarealtoken00000000000000000/{}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&source_a)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 401, "an invalid token must be refused before touching ffmpeg at all");
    }

    // A source outside the served library root is refused, exactly like `/stream/`.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /hls/{token}/{}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&outsider)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 404, "a source outside the library root must not be segmentable");
    }

    // A segment name requested before any playlist request for that source has ever run is a stale
    // or forged URL, not a legitimate race -- 404, never a wait-and-retry.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /hls/{token}/{}/seg_00000.m4s HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&source_b)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 404, "a segment can never be fetched before its playlist generates it");
    }

    // The first playlist request for a genuinely new source triggers real generation (through the
    // fake ffmpeg) and returns bare, relative segment/init URIs -- never the absolute build-directory
    // paths ffmpeg itself was invoked with.
    let before = invocation_count(&log_path);
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /hls/{token}/{}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, headers, body) = read_http_response(&mut c);
    assert_eq!(status, 200, "generation must succeed against the fake ffmpeg");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("mpegurl")),
        "expected an HLS playlist content-type, got {headers:?}"
    );
    let playlist = String::from_utf8(body).expect("a playlist must be valid UTF-8");
    assert!(playlist.contains("#EXTM3U"), "not a real playlist: {playlist:?}");
    assert!(playlist.contains("seg_00000.m4s"), "expected a bare segment URI: {playlist:?}");
    assert!(
        !playlist.contains(dir.0.to_str().unwrap()),
        "the served playlist must never leak the server's own build-directory path: {playlist:?}"
    );
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "exactly one ffmpeg run for a fresh source"
    );

    // A segment named by that playlist now resolves, with the fake script's own fixed bytes.
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /hls/{token}/{}/seg_00000.m4s HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, headers, body) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert!(
        headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "video/mp4")
    );
    assert_eq!(body, b"seg0", "expected the fake ffmpeg's own segment bytes");

    // A second request for the *same, unchanged* source must not regenerate anything -- served
    // straight from the on-disk cache.
    let before = invocation_count(&log_path);
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /hls/{token}/{}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, _, _) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert_eq!(
        invocation_count(&log_path),
        before,
        "an unchanged, already-cached source must not re-run ffmpeg"
    );

    // Concurrent first-time requests for one new source coalesce onto exactly one ffmpeg run.
    let before = invocation_count(&log_path);
    let fp = fingerprint.clone();
    let tok = token.clone();
    let src = enc(&source_c);
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let fp = fp.clone();
            let tok = tok.clone();
            let src = src.clone();
            std::thread::spawn(move || {
                let mut c = connect_tls(port, &fp, Duration::from_secs(10));
                c.write_all(
                    format!("GET /hls/{tok}/{src}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
                read_http_response(&mut c).0
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 200, "every coalesced requester must still see success");
    }
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "six concurrent requests for the same new source must invoke ffmpeg exactly once"
    );

    // Touching the source's mtime changes its cache key -- the next playlist request must generate
    // fresh output, not keep serving what was cached under the old key.
    let before = invocation_count(&log_path);
    let touched = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::open(&source_a).unwrap().set_modified(touched).unwrap();
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /hls/{token}/{}/playlist.m3u8 HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, _, body) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("#EXTM3U"));
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "a changed mtime must trigger a fresh generation under its new cache key"
    );

    // Kept explicit even though `KillOnDrop` will do this again on scope exit regardless -- see the
    // matching comment at the end of the other integration test in this file.
    let _ = server.kill();
    let _ = server.wait();
}

/// Tier 5 integration coverage: DASH delivery wired into `lumen serve`'s HTTP surface (see
/// `remote::server::dash`), mirroring the HLS integration test above closely -- same fake-ffmpeg
/// approach, same coverage shape, adapted to DASH's own artifact names and its bare
/// `-init_seg_name`/`-media_seg_name` addressing (confirmed live against a real ffmpeg build in
/// `lumen-segment`'s own `dash.rs` tests -- see that crate for the genuine subprocess/ffprobe proof;
/// this test only needs a fake ffmpeg for CI speed, per this session's own established split between
/// "prove the real tool's behavior once, at the crate level" and "prove the wiring, fast, everywhere
/// else"). Real mpv is still required for the same reason the HLS test needs it: `server::run`
/// unconditionally spawns an idle mpv on startup regardless of whether any DASH route is ever hit.
#[cfg(unix)]
#[test]
fn dash_manifest_and_segments_are_generated_lazily_cached_and_authenticated() {
    use std::os::unix::fs::PermissionsExt;

    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("dash");
    let outside = TempDir::new("dash-outside");

    // Three dummy sources. Their bytes are never read by ffmpeg -- the fake script below ignores its
    // real input entirely and always writes the same fixed output -- only their existence, size, and
    // mtime matter, since those alone feed `dash::cache_key`.
    let source_a = dir.0.join("A.mkv");
    let source_b = dir.0.join("B.mkv");
    let source_c = dir.0.join("C.mkv");
    std::fs::write(&source_a, b"source a").unwrap();
    std::fs::write(&source_b, b"source b").unwrap();
    std::fs::write(&source_c, b"source c").unwrap();
    let outsider = outside.0.join("Secret.mkv");
    std::fs::write(&outsider, b"not in the library").unwrap();

    // Writes a manifest shaped exactly like a real ffmpeg DASH-MPD run against a video+audio source:
    // two representations ("0" video, "1" audio), each with its own bare-relative-named
    // `SegmentTemplate`, representation "0" producing one chunk and representation "1" producing two
    // -- reproducing, not just asserting, the independent-per-representation segment counts confirmed
    // live against a real ffmpeg build (see `lumen-segment/src/dash.rs`'s own module doc).
    let log_path = dir.0.join("ffmpeg-invocations.log");
    let fake_ffmpeg = dir.0.join("fake-ffmpeg.sh");
    std::fs::write(
        &fake_ffmpeg,
        format!(
            "#!/bin/sh\n\
             echo invoked >> \"{log}\"\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf 'chunk0' > \"$dir/chunk-0-00001.m4s\"\n\
             printf 'achunk0' > \"$dir/chunk-1-00001.m4s\"\n\
             printf 'achunk1' > \"$dir/chunk-1-00002.m4s\"\n\
             printf 'init0' > \"$dir/init-0.m4s\"\n\
             printf 'init1' > \"$dir/init-1.m4s\"\n\
             printf '<?xml version=\"1.0\" encoding=\"utf-8\"?><MPD><Period>' > \"$last\"\n\
             printf '<AdaptationSet><Representation id=\"0\"><SegmentTemplate \
             initialization=\"init-$RepresentationID$.m4s\" \
             media=\"chunk-$RepresentationID$-$Number%%05d$.m4s\"/></Representation></AdaptationSet>' \
             >> \"$last\"\n\
             printf '<AdaptationSet><Representation id=\"1\"><SegmentTemplate \
             initialization=\"init-$RepresentationID$.m4s\" \
             media=\"chunk-$RepresentationID$-$Number%%05d$.m4s\"/></Representation></AdaptationSet>' \
             >> \"$last\"\n\
             printf '</Period></MPD>' >> \"$last\"\n\
             exit 0\n",
            log = log_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

    let invocation_count = |log: &std::path::Path| -> usize {
        std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
    };

    let config_dir = dir.0.join("config");
    // A disjoint port range from both the pairing/playback test (17000..20999) and the HLS test
    // (21000..24999) above, so all three can run concurrently in the same `cargo test` process
    // without contending for a listener.
    let port = 25000 + (std::process::id() % 4000) as u16;
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = KillOnDrop(
        std::process::Command::new(bin)
            .args([
                "serve",
                dir.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--",
                "--vo=null",
                "--ao=null",
                "--force-window=no",
            ])
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("APPDATA", &config_dir)
            .env("HOME", &config_dir)
            .env("LUMEN_FFMPEG", &fake_ffmpeg)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("lumen must be runnable"),
    );

    if let Some(stderr) = server.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[lumen serve stderr] {line}");
            }
        });
    }

    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut code = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("pairing code: ") {
            code = Some(rest.split_whitespace().next().unwrap().to_string());
            break;
        }
    }
    let code = code.expect("the server must print a pairing code on startup");

    let mut fingerprint = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if let Some(rest) = line.strip_prefix("tls fingerprint: ") {
            fingerprint = Some(rest.split("  ").next().unwrap().to_string());
            break;
        }
    }
    let fingerprint = fingerprint.expect("the server must print a TLS fingerprint on startup");

    std::thread::spawn(move || {
        for line in lines.map_while(Result::ok) {
            println!("[lumen serve stdout] {line}");
        }
    });

    rustls::crypto::ring::default_provider().install_default().ok();

    let mut tls = connect_tls(port, &fingerprint, Duration::from_secs(10));
    tls.write_all(request("1", &format!("\"type\":\"pair\",\"code\":\"{code}\"")).as_bytes())
        .unwrap();
    let paired = read_reply(&mut tls);
    assert_eq!(
        paired.ty().as_deref(),
        Some("paired"),
        "expected a paired reply, got {:?}",
        paired.0
    );
    let token = paired.str("token").expect("a paired reply must carry a token");
    drop(tls);

    let enc = |p: &std::path::Path| p.to_str().unwrap().replace(' ', "%20");

    // A missing/invalid token is refused before any generation is even attempted.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /dash/notarealtoken00000000000000000/{}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&source_a)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 401, "an invalid token must be refused before touching ffmpeg at all");
    }

    // A source outside the served library root is refused, exactly like `/stream/` and `/hls/`.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /dash/{token}/{}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&outsider)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 404, "a source outside the library root must not be segmentable");
    }

    // A chunk name requested before any manifest request for that source has ever run is a stale or
    // forged URL, not a legitimate race -- 404, never a wait-and-retry.
    {
        let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
        c.write_all(
            format!(
                "GET /dash/{token}/{}/chunk-0-00001.m4s HTTP/1.1\r\nHost: x\r\n\r\n",
                enc(&source_b)
            )
            .as_bytes(),
        )
        .unwrap();
        let (status, _, _) = read_http_response(&mut c);
        assert_eq!(status, 404, "a chunk can never be fetched before its manifest generates it");
    }

    // The first manifest request for a genuinely new source triggers real generation (through the
    // fake ffmpeg) and returns bare, relative init/chunk URIs -- never the absolute build-directory
    // paths ffmpeg itself was invoked with.
    let before = invocation_count(&log_path);
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /dash/{token}/{}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, headers, body) = read_http_response(&mut c);
    assert_eq!(status, 200, "generation must succeed against the fake ffmpeg");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("dash+xml")),
        "expected a DASH manifest content-type, got {headers:?}"
    );
    let manifest = String::from_utf8(body).expect("a manifest must be valid UTF-8");
    assert!(manifest.contains("<MPD"), "not a real manifest: {manifest:?}");
    // A manifest's own `SegmentTemplate` carries the *pattern*, never a resolved literal segment
    // name -- `$RepresentationID$` is filled in by the player per representation, not by ffmpeg when
    // it writes the manifest (confirmed live: see `lumen-segment/src/dash.rs`'s own module doc).
    assert!(
        manifest.contains(r#"initialization="init-$RepresentationID$.m4s""#)
            && manifest.contains(r#"media="chunk-$RepresentationID$-$Number%05d$.m4s""#),
        "expected bare segment template patterns for both representations: {manifest:?}"
    );
    assert!(
        !manifest.contains(dir.0.to_str().unwrap()),
        "the served manifest must never leak the server's own build-directory path: {manifest:?}"
    );
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "exactly one ffmpeg run for a fresh source"
    );

    // An init segment named by that manifest now resolves, with the fake script's own fixed bytes.
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /dash/{token}/{}/init-0.m4s HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, headers, body) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert!(
        headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "video/mp4")
    );
    assert_eq!(body, b"init0", "expected the fake ffmpeg's own init segment bytes");

    // Representation "1"'s *second* chunk resolves too -- proving this route family does not assume
    // every representation shares one segment count the way HLS's single timeline would.
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!(
            "GET /dash/{token}/{}/chunk-1-00002.m4s HTTP/1.1\r\nHost: x\r\n\r\n",
            enc(&source_a)
        )
        .as_bytes(),
    )
    .unwrap();
    let (status, _, body) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert_eq!(body, b"achunk1", "expected the fake ffmpeg's own second-chunk bytes");

    // A second request for the *same, unchanged* source must not regenerate anything -- served
    // straight from the on-disk cache.
    let before = invocation_count(&log_path);
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /dash/{token}/{}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, _, _) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert_eq!(
        invocation_count(&log_path),
        before,
        "an unchanged, already-cached source must not re-run ffmpeg"
    );

    // Concurrent first-time requests for one new source coalesce onto exactly one ffmpeg run.
    let before = invocation_count(&log_path);
    let fp = fingerprint.clone();
    let tok = token.clone();
    let src = enc(&source_c);
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let fp = fp.clone();
            let tok = tok.clone();
            let src = src.clone();
            std::thread::spawn(move || {
                let mut c = connect_tls(port, &fp, Duration::from_secs(10));
                c.write_all(
                    format!("GET /dash/{tok}/{src}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
                read_http_response(&mut c).0
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 200, "every coalesced requester must still see success");
    }
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "six concurrent requests for the same new source must invoke ffmpeg exactly once"
    );

    // Touching the source's mtime changes its cache key -- the next manifest request must generate
    // fresh output, not keep serving what was cached under the old key.
    let before = invocation_count(&log_path);
    let touched = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::open(&source_a).unwrap().set_modified(touched).unwrap();
    let mut c = connect_tls(port, &fingerprint, Duration::from_secs(5));
    c.write_all(
        format!("GET /dash/{token}/{}/manifest.mpd HTTP/1.1\r\nHost: x\r\n\r\n", enc(&source_a))
            .as_bytes(),
    )
    .unwrap();
    let (status, _, body) = read_http_response(&mut c);
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("<MPD"));
    assert_eq!(
        invocation_count(&log_path),
        before + 1,
        "a changed mtime must trigger a fresh generation under its new cache key"
    );

    // Kept explicit even though `KillOnDrop` will do this again on scope exit regardless -- see the
    // matching comment at the end of the other integration tests in this file.
    let _ = server.kill();
    let _ = server.wait();
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
