//! mpv JSON-IPC client.
//!
//! Richer than the S1 spike's: that one only reads numbers, while a player has to follow *events* —
//! `start-file`, `end-file` and the reason attached to it — to know what happened to each file.
//!
//! A reader thread owns the socket and pushes parsed lines into a channel. That is what makes a
//! blocking read impossible to get stuck on: a Windows named pipe opened as a `File` has no read
//! timeout, so polling it on the main thread would hang the player the moment mpv stopped talking.
//!
//! **Events encountered while waiting for a command reply are queued, not dropped.** mpv interleaves
//! them freely, so a `get_property` issued at the wrong moment would otherwise swallow the very
//! `end-file` event that says why a file failed — and the outcome would silently become "unknown".

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use crate::json::{self, Value};

/// Platform-appropriate socket path.
pub fn default_ipc_path(tag: &str) -> String {
    if cfg!(windows) {
        format!(r"\\.\pipe\lumen-{tag}")
    } else {
        // Not /tmp: a multi-user machine shares it, and a predictable name there is a socket another
        // user can create first. The runtime dir is per-user.
        let base = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
        format!("{base}/lumen-{tag}-{}.sock", std::process::id())
    }
}

pub struct Mpv {
    writer: Box<dyn Write + Send>,
    rx: Receiver<Value>,
    next_id: u64,
    /// Events seen while waiting for a command reply.
    queued: std::collections::VecDeque<Value>,
    /// Set once the socket closes, so callers can tell "mpv exited" from "nothing happened yet".
    closed: bool,
}

/// Hand-written because the socket handle behind `writer` is not `Debug`. Shows the state that
/// matters when something is wrong: how many commands have gone out and whether mpv is still there.
impl std::fmt::Debug for Mpv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mpv")
            .field("requests_sent", &self.next_id)
            .field("queued_events", &self.queued.len())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl Mpv {
    /// Connect, retrying until `timeout`. mpv creates the socket shortly after launch, so the first
    /// few attempts failing is normal rather than an error.
    pub fn connect(path: &str, timeout: Duration) -> std::io::Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            match Self::try_connect(path) {
                Ok(m) => return Ok(m),
                // The last attempt's error is the one worth reporting — it describes the state the
                // socket was actually left in, not the "not there yet" of the first try.
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    #[cfg(unix)]
    fn try_connect(path: &str) -> std::io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        let reader = stream.try_clone()?;
        Ok(Self::spawn_reader(Box::new(stream), Box::new(reader)))
    }

    // `std::fs::OpenOptions`/`File::try_clone` used to be used here, the same pattern as the Unix
    // branch above: open once, `DuplicateHandle` a second handle for the reader thread. It compiled
    // and connected fine, and then deadlocked the instant a real command was sent -- every write
    // blocked forever, and so did the reader thread's very next read. The two duplicated handles are
    // still the *same* underlying kernel pipe object, and Windows serializes synchronous (i.e.
    // non-overlapped, which `File` always is) I/O per file object: a pending blocking `ReadFile` on
    // one handle holds up a `WriteFile` on the other, and since mpv was never going to reply until it
    // received that write, the two sides waited on each other forever. `interprocess`'s named-pipe
    // support exists specifically to give each half its own overlapped I/O state instead of sharing
    // one synchronous handle across threads, which is what actually lets a read and a write happen
    // concurrently without contention.
    #[cfg(windows)]
    fn try_connect(path: &str) -> std::io::Result<Self> {
        use interprocess::os::windows::named_pipe::{DuplexPipeStream, pipe_mode};
        let conn = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(path)?;
        let (reader, writer) = conn.split();
        Ok(Self::spawn_reader(Box::new(writer), Box::new(reader)))
    }

    #[cfg(not(any(unix, windows)))]
    fn try_connect(_path: &str) -> std::io::Result<Self> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no IPC transport here"))
    }

    fn spawn_reader(writer: Box<dyn Write + Send>, reader: Box<dyn std::io::Read + Send>) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // TODO(diagnostic, remove once the Windows Play-timeout investigation concludes): proves
            // whether this thread is actually blocked reading the pipe/socket at all, vs. reading fine
            // but the main thread's recv side is what stalls.
            eprintln!("ipc: reader thread starting");
            let mut lines = BufReader::new(reader).lines();
            loop {
                eprintln!("ipc: reader thread calling lines.next() (blocking read)");
                let next = lines.next();
                eprintln!(
                    "ipc: reader thread lines.next() returned: {:?}",
                    next.as_ref().map(|r| r.is_ok())
                );
                let Some(Ok(line)) = next else {
                    eprintln!("ipc: reader thread exiting (EOF or read error)");
                    return;
                };
                if line.trim().is_empty() {
                    continue;
                }
                // A line that will not parse is dropped rather than fatal. mpv has been known to emit
                // a stray non-JSON line on some builds, and killing the session over one would lose
                // a whole playback run's worth of results.
                if let Ok(v) = json::parse(&line) {
                    eprintln!("ipc: reader thread parsed a line, sending to channel: {line}");
                    if tx.send(v).is_err() {
                        return; // the player hung up
                    }
                } else {
                    eprintln!("ipc: reader thread got a non-JSON line, dropping: {line}");
                }
            }
        });
        Self { writer, rx, next_id: 0, queued: std::collections::VecDeque::new(), closed: false }
    }

    /// True once mpv's socket has closed — the process is gone.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn send(&mut self, payload: &str) -> std::io::Result<()> {
        self.writer.write_all(payload.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    /// Run a command and return its `data` field, waiting up to five seconds for a reply.
    ///
    /// `Ok(None)` means mpv answered with an error — an unknown property, or one this build does not
    /// expose. That is information, not a failure: an older mpv missing one property should not end
    /// the run.
    pub fn command(&mut self, args: &[&str]) -> std::io::Result<Option<Value>> {
        self.command_timeout(args, Duration::from_secs(5))
    }

    /// Same as [`command`](Self::command), but with an explicit deadline instead of the fixed five
    /// seconds.
    ///
    /// `remote::server`'s background state-polling loop uses this with a short deadline rather than
    /// `command`'s own: that loop and every client's own commands share one single-threaded driver
    /// (`drive_mpv`), so a slow reply to a routine property poll must never be allowed to block a
    /// real command — like the `Play` a client is waiting on — for as long as `command`'s own
    /// generous five-second budget. A stale poll reading back as "unknown" for one cycle is a far
    /// smaller cost than a client's command appearing to hang.
    pub fn command_timeout(
        &mut self,
        args: &[&str],
        timeout: Duration,
    ) -> std::io::Result<Option<Value>> {
        self.next_id += 1;
        let id = self.next_id;
        let quoted: Vec<String> = args.iter().map(|a| json::quote(a)).collect();
        let desc = args.join(" ");
        // TODO(diagnostic, remove once the Windows Play-timeout investigation concludes): pins down
        // whether a blocked write() (the pipe's write side backing up) or a blocked/unbounded receive
        // loop is where drive_mpv's first iteration is actually losing time -- the heartbeat diagnostic
        // proved the loop's own logging never gets a chance to run again once this function is entered,
        // but not which half of it is stuck.
        let call_start = Instant::now();
        eprintln!("ipc: command_timeout({desc}) id={id} sending");
        self.send(&format!("{{\"command\":[{}],\"request_id\":{id}}}", quoted.join(",")))?;
        eprintln!(
            "ipc: command_timeout({desc}) id={id} write+flush returned after {:?}",
            call_start.elapsed()
        );

        let deadline = Instant::now() + timeout;
        loop {
            let recv_start = Instant::now();
            let Some(v) = self.recv_until(deadline) else {
                eprintln!(
                    "ipc: command_timeout({desc}) id={id} recv_until gave up after {:?} (total {:?}); treating as unknown",
                    recv_start.elapsed(),
                    call_start.elapsed()
                );
                return Ok(None); // timed out; the caller treats a missing answer as unknown
            };
            eprintln!(
                "ipc: command_timeout({desc}) id={id} recv_until got a value after {:?}: {v:?}",
                recv_start.elapsed()
            );
            if v.get("request_id").and_then(Value::as_f64) == Some(id as f64) {
                if v.get("error").and_then(Value::as_str) != Some("success") {
                    eprintln!(
                        "ipc: command_timeout({desc}) id={id} matched our request_id with a non-success error after {:?} total",
                        call_start.elapsed()
                    );
                    return Ok(None);
                }
                eprintln!(
                    "ipc: command_timeout({desc}) id={id} matched our request_id with success after {:?} total",
                    call_start.elapsed()
                );
                return Ok(v.get("data").cloned());
            }
            // Not our reply. If it is an event it must survive — dropping it here is how an
            // `end-file` reason goes missing and a real failure gets recorded as "unknown".
            if v.get("event").is_some() {
                self.queued.push_back(v);
            }
        }
    }

    fn recv_until(&mut self, deadline: Instant) -> Option<Value> {
        let now = Instant::now();
        let wait = deadline.saturating_duration_since(now);
        match self.rx.recv_timeout(wait) {
            Ok(v) => Some(v),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                None
            }
        }
    }

    /// Read a property, or `None` when this build does not expose it.
    pub fn get(&mut self, property: &str) -> Option<Value> {
        self.get_timeout(property, Duration::from_secs(5))
    }

    /// Same as [`get`](Self::get), with an explicit deadline — see
    /// [`command_timeout`](Self::command_timeout) for why this exists.
    pub fn get_timeout(&mut self, property: &str, timeout: Duration) -> Option<Value> {
        self.command_timeout(&["get_property", property], timeout).ok().flatten()
    }

    pub fn get_string(&mut self, property: &str) -> Option<String> {
        self.get_string_timeout(property, Duration::from_secs(5))
    }

    pub fn get_string_timeout(&mut self, property: &str, timeout: Duration) -> Option<String> {
        match self.get_timeout(property, timeout)? {
            Value::Str(s) if !s.is_empty() => Some(s),
            Value::Num(n) => Some(format!("{n}")),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn get_f64(&mut self, property: &str) -> Option<f64> {
        self.get_f64_timeout(property, Duration::from_secs(5))
    }

    pub fn get_f64_timeout(&mut self, property: &str, timeout: Duration) -> Option<f64> {
        self.get_timeout(property, timeout)?.as_f64()
    }

    /// Wait for the next event, up to `timeout`.
    pub fn next_event(&mut self, timeout: Duration) -> Option<Value> {
        if let Some(v) = self.queued.pop_front() {
            return Some(v);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let v = self.recv_until(deadline)?;
            if v.get("event").is_some() {
                return Some(v);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    pub fn quit(&mut self) {
        let _ = self.send("{\"command\":[\"quit\"]}");
    }
}

/// The name of an event, if this value is one.
pub fn event_name(v: &Value) -> Option<&str> {
    v.get("event")?.as_str()
}

/// Human-readable meaning of an `end-file` reason.
///
/// `error` is the only one that means something went wrong. `stop` covers both a user skip and our
/// own `playlist-next`, so it must never be reported as a failure — doing so would make every file
/// in a timed test run look broken.
pub fn end_reason_is_failure(reason: &str) -> bool {
    reason == "error"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    #[test]
    fn an_end_file_error_is_the_only_failing_reason() {
        assert!(end_reason_is_failure("error"));
        // `stop` is what our own playlist-next produces. Reading it as a failure would make every
        // file in a timed run look broken — the exact opposite of the truth.
        for ok in ["eof", "stop", "quit", "redirect", "unknown"] {
            assert!(!end_reason_is_failure(ok), "{ok} must not read as a failure");
        }
    }

    #[test]
    fn event_names_are_read_off_real_event_lines() {
        let e = parse(
            r#"{"event":"end-file","reason":"error","file_error":"Unrecognized file format"}"#,
        )
        .unwrap();
        assert_eq!(event_name(&e), Some("end-file"));
        assert_eq!(e.get("reason").and_then(Value::as_str), Some("error"));
        assert_eq!(
            e.get("file_error").and_then(Value::as_str),
            Some("Unrecognized file format"),
            "the error text is the whole reason to read this event"
        );
        assert_eq!(event_name(&parse(r#"{"data":1,"request_id":2}"#).unwrap()), None);
    }

    #[test]
    fn ipc_paths_are_per_process_so_two_runs_do_not_collide() {
        let a = default_ipc_path("play");
        assert!(a.contains(&std::process::id().to_string()) || cfg!(windows), "{a}");
    }

    /// The queueing behaviour, exercised without a socket: `command` must preserve events it meets
    /// while waiting, because those events carry the outcome of the file being played.
    #[test]
    fn events_seen_while_awaiting_a_reply_are_queued_rather_than_dropped() {
        let (tx, rx) = channel();
        let mut mpv = Mpv {
            writer: Box::new(std::io::sink()),
            rx,
            next_id: 0,
            queued: std::collections::VecDeque::new(),
            closed: false,
        };
        // mpv answers request 1, but an end-file event arrives first.
        tx.send(parse(r#"{"event":"end-file","reason":"error"}"#).unwrap()).unwrap();
        tx.send(parse(r#"{"data":"hevc","request_id":1,"error":"success"}"#).unwrap()).unwrap();

        let data = mpv.command(&["get_property", "video-codec"]).unwrap();
        assert_eq!(data.as_ref().and_then(Value::as_str), Some("hevc"));

        let ev = mpv.next_event(Duration::from_millis(50)).expect("the event must have survived");
        assert_eq!(event_name(&ev), Some("end-file"));
    }

    #[test]
    fn an_error_reply_is_none_rather_than_an_error() {
        // An older mpv missing one property must not end the run.
        let (tx, rx) = channel();
        let mut mpv = Mpv {
            writer: Box::new(std::io::sink()),
            rx,
            next_id: 0,
            queued: std::collections::VecDeque::new(),
            closed: false,
        };
        tx.send(parse(r#"{"error":"property unavailable","request_id":1}"#).unwrap()).unwrap();
        assert_eq!(mpv.command(&["get_property", "nonsense"]).unwrap(), None);
    }

    #[test]
    fn a_closed_socket_is_reported_rather_than_looping() {
        let (tx, rx) = channel::<Value>();
        drop(tx);
        let mut mpv = Mpv {
            writer: Box::new(std::io::sink()),
            rx,
            next_id: 0,
            queued: std::collections::VecDeque::new(),
            closed: false,
        };
        assert_eq!(mpv.next_event(Duration::from_millis(50)), None);
        assert!(
            mpv.is_closed(),
            "the caller must be able to tell mpv exited from nothing happening"
        );
    }

    #[test]
    fn command_arguments_are_json_quoted_so_paths_survive() {
        // A path with a quote or a backslash in it would otherwise produce a malformed command that
        // mpv rejects — and the file would look unplayable when it is fine.
        assert_eq!(json::quote(r#"C:\Media\a "b".mkv"#), r#""C:\\Media\\a \"b\".mkv""#);
    }

    fn silent_mpv(rx: Receiver<Value>) -> Mpv {
        Mpv {
            writer: Box::new(std::io::sink()),
            rx,
            next_id: 0,
            queued: std::collections::VecDeque::new(),
            closed: false,
        }
    }

    #[test]
    fn a_reply_within_the_short_deadline_still_answers() {
        let (tx, rx) = channel();
        let mut mpv = silent_mpv(rx);
        tx.send(parse(r#"{"data":"idle","request_id":1,"error":"success"}"#).unwrap()).unwrap();
        let data =
            mpv.command_timeout(&["get_property", "path"], Duration::from_millis(50)).unwrap();
        assert_eq!(data.as_ref().and_then(Value::as_str), Some("idle"));
    }

    /// The whole point of `command_timeout`: a caller that cannot afford `command`'s own five-second
    /// wait (`remote::server`'s background state poll, so it never blocks a real client command for
    /// that long) gets to bound it itself instead.
    #[test]
    fn a_reply_that_never_arrives_gives_up_at_the_caller_chosen_deadline_not_five_seconds() {
        let (_tx, rx) = channel::<Value>();
        let mut mpv = silent_mpv(rx);
        let start = Instant::now();
        let data =
            mpv.command_timeout(&["get_property", "path"], Duration::from_millis(50)).unwrap();
        assert_eq!(data, None);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "must give up at the short deadline, not command()'s own five seconds: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn get_string_and_get_f64_timeout_variants_parse_the_same_as_their_defaults() {
        let (tx, rx) = channel();
        let mut mpv = silent_mpv(rx);
        tx.send(parse(r#"{"data":"in.mkv","request_id":1,"error":"success"}"#).unwrap()).unwrap();
        assert_eq!(
            mpv.get_string_timeout("path", Duration::from_millis(50)),
            Some("in.mkv".to_string())
        );

        let (tx, rx) = channel();
        let mut mpv = silent_mpv(rx);
        tx.send(parse(r#"{"data":42.5,"request_id":1,"error":"success"}"#).unwrap()).unwrap();
        assert_eq!(mpv.get_f64_timeout("duration", Duration::from_millis(50)), Some(42.5));
    }
}
