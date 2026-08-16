//! `lumen serve`: drive one persistent mpv session and let paired clients watch and steer it.
//!
//! **One thread owns the mpv connection.** `ipc::Mpv` is `Send` but not `Sync` — its socket reads
//! and writes are not safe to interleave from two threads at once, the same reason `session.rs`
//! never shares a live connection either. Every client-handling thread reaches mpv by sending a
//! [`Command`] down a channel and waiting on its own reply channel; the driver thread is the only
//! thing that ever calls a method on `Mpv` directly. That single thread also polls mpv's own state
//! on a short interval and publishes it to [`SharedState`], which every connected client's writer
//! thread reads independently — no broadcast/fan-out machinery, because "did the version number
//! change since I last sent one" is enough to know whether to push.
//!
//! **Two threads per connection**, split with `TcpStream::try_clone`: a reader thread blocks on
//! incoming lines and turns them into commands, a writer thread polls the shared state and pushes
//! whenever it changes. `std` has no readiness multiplexing for sockets, so this is the plain way to
//! do "read on one side, write independently on the other" without another dependency.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::ipc::{self, Mpv};
use crate::remote::pairing::{self, AttemptLimiter, PairResult, PendingCode, TokenStore};
use crate::remote::protocol::{
    ClientMessage, LibraryEntry, NowPlaying, PlaybackState, ReplyBody, ServerMessage,
};
use crate::scan::{self, Scan, ScanOptions};

/// How often the writer thread checks whether the state has moved on. Short enough that a seek
/// feels immediate on the client, long enough that idle connections cost nothing worth measuring.
const STATE_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// How often the driver thread re-reads mpv's own properties to build the next `PlaybackState`.
const MPV_POLL_INTERVAL: Duration = Duration::from_millis(400);

/// A request from a client thread to the mpv driver thread, with its own private reply channel —
/// this is what lets several client connections issue commands concurrently without stepping on
/// each other's replies, despite there being exactly one thread that can act on any of them.
struct Command {
    body: CommandBody,
    reply: Sender<Result<ReplyBody, String>>,
}

enum CommandBody {
    Play(String),
    Pause,
    Resume,
    Toggle,
    Seek(i64),
    SetVolume(u8),
}

/// State every connected client can read without going through the driver thread at all.
struct SharedState {
    state: Mutex<PlaybackState>,
    /// Bumped on every write to `state`. A writer thread's "have I already sent this" check is
    /// comparing against this rather than diffing the state itself, which is cheaper and cannot miss
    /// a change that happened to round-trip back to an equal value.
    version: AtomicU64,
}

impl SharedState {
    fn new() -> Self {
        Self { state: Mutex::new(PlaybackState::default()), version: AtomicU64::new(0) }
    }

    fn snapshot(&self) -> (PlaybackState, u64) {
        (self.state.lock().unwrap().clone(), self.version.load(Ordering::Acquire))
    }

    fn publish(&self, new_state: PlaybackState) {
        let mut guard = self.state.lock().unwrap();
        if *guard == new_state {
            return;
        }
        *guard = new_state;
        drop(guard);
        self.version.fetch_add(1, Ordering::Release);
    }
}

/// Everything the accept loop hands each connection.
struct ServerContext {
    commands: Sender<Command>,
    shared: Arc<SharedState>,
    tokens: Arc<Mutex<TokenStore>>,
    token_path: PathBuf,
    pending_code: Arc<Mutex<Option<PendingCode>>>,
    /// Shared across every connection on purpose: a fresh TCP connection costs a guesser nothing, so
    /// the limit on wrong-code guesses has to be global, not per-socket. See `pairing::AttemptLimiter`.
    pair_attempts: Arc<Mutex<AttemptLimiter>>,
    library: Arc<Mutex<Scan>>,
    /// The canonicalized root `lumen serve` was pointed at. Every `Play` request is checked against
    /// this before being forwarded to mpv — see `resolve_playable_path` — so a paired client can only
    /// ever open a file the server itself already scanned, not an arbitrary path on the host.
    library_root: PathBuf,
}

/// Run the server. Blocks until the process is killed — this is meant to run in the foreground of a
/// terminal left open, the same way a person leaves a media server running.
pub fn run(
    library_path: &Path,
    bind: &str,
    port: u16,
    extra_mpv_args: &[String],
    log: impl Fn(&str) + Send + Sync + 'static,
) -> Result<(), String> {
    // Canonicalized once, up front: every `Play` request is checked against this root later, and
    // resolving symlinks/`..` here rather than per-request is both cheaper and means a single
    // definition of "inside the library" is shared by the scan and the containment check.
    let library_root = library_path
        .canonicalize()
        .map_err(|e| format!("cannot resolve library path {}: {e}", library_path.display()))?;

    let scan =
        scan::scan(std::slice::from_ref(&library_path.to_path_buf()), &ScanOptions::default());
    log(&format!(
        "library: {} playable files under {}",
        scan.playable().count(),
        library_path.display()
    ));

    let ipc_path = ipc::default_ipc_path("serve");
    let _ = std::fs::remove_file(&ipc_path);
    let mut child = spawn_idle_mpv(&ipc_path, extra_mpv_args)
        .map_err(|e| format!("cannot launch mpv: {e}\n\n{}", crate::mpvbin::install_hint()))?;
    let mpv = Mpv::connect(&ipc_path, Duration::from_secs(20)).map_err(|e| {
        let _ = child.kill();
        format!("mpv started but its IPC socket never appeared ({e})")
    })?;

    let (tx, rx) = mpsc::channel::<Command>();
    let shared = Arc::new(SharedState::new());
    let driver_shared = Arc::clone(&shared);
    std::thread::spawn(move || drive_mpv(mpv, rx, &driver_shared));

    let token_path = TokenStore::default_path();
    let tokens = Arc::new(Mutex::new(TokenStore::load(&token_path)));

    let code = pairing::generate_code(random_u32());
    log(&format!(
        "pairing code: {code}  (valid {} minutes; enter it once in a client, which then stores a \
         token and does not need it again)",
        pairing::CODE_LIFETIME.as_secs() / 60
    ));
    let pending_code = Arc::new(Mutex::new(Some(PendingCode {
        code,
        expires_at: SystemTime::now() + pairing::CODE_LIFETIME,
    })));

    let ctx = Arc::new(ServerContext {
        commands: tx,
        shared,
        tokens,
        token_path,
        pending_code,
        pair_attempts: Arc::new(Mutex::new(AttemptLimiter::default_policy())),
        library: Arc::new(Mutex::new(scan)),
        library_root,
    });

    let listener = TcpListener::bind((bind, port))
        .map_err(|e| format!("cannot listen on {bind}:{port}: {e}"))?;
    log(&format!("listening on {bind}:{port}"));

    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle_connection(stream, &ctx));
    }
    Ok(())
}

/// mpv with nothing loaded, waiting for the first `Play` command. `--idle=yes` is what lets it stay
/// up with no file open instead of exiting the instant it has nothing to do — the same flag
/// `session.rs` relies on to survive a broken file, used here to survive having *no* file yet.
///
/// `extra_args` come last so they win, the same convention `session.rs`'s `--` passthrough follows —
/// this is what lets an operator override the video output (`--vo=null` on a headless box) or
/// hardware decode mode without this function needing to know either exists.
fn spawn_idle_mpv(ipc_path: &str, extra_args: &[String]) -> std::io::Result<std::process::Child> {
    let mut args = vec![
        format!("--input-ipc-server={ipc_path}"),
        "--idle=yes".into(),
        "--force-window=yes".into(),
        "--keep-open=yes".into(),
        "--term-status-msg=".into(),
        "--msg-level=all=error".into(),
        "--no-input-terminal".into(),
    ];
    args.extend(extra_args.iter().cloned());
    std::process::Command::new(
        crate::mpvbin::find()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "mpv not found"))?,
    )
    .args(args)
    .stdin(std::process::Stdio::null())
    .spawn()
}

/// The one thread that ever touches the mpv connection: executes queued commands, then polls mpv's
/// properties into `shared`. Interleaved in a loop rather than two threads for the same reason
/// `session.rs`'s run loop interleaves control and events — a command and a property read can never
/// race each other if the same loop iteration is the only place either happens.
fn drive_mpv(mut mpv: Mpv, commands: Receiver<Command>, shared: &SharedState) {
    let mut last_poll = std::time::Instant::now() - MPV_POLL_INTERVAL;
    loop {
        if mpv.is_closed() {
            return;
        }
        // Drain whatever commands arrived since the last pass without blocking the poll behind them.
        while let Ok(cmd) = commands.try_recv() {
            let _ = cmd.reply.send(execute(&mut mpv, cmd.body));
        }
        if last_poll.elapsed() >= MPV_POLL_INTERVAL {
            shared.publish(read_state(&mut mpv));
            last_poll = std::time::Instant::now();
        }
        // A short blocking wait on mpv's own event socket doubles as the loop's tick rate, so this
        // is not a busy spin between polls.
        mpv.next_event(Duration::from_millis(100));
    }
}

fn execute(mpv: &mut Mpv, body: CommandBody) -> Result<ReplyBody, String> {
    let ok = |r: std::io::Result<Option<crate::json::Value>>| {
        r.map(|_| ReplyBody::Ok).map_err(|e| e.to_string())
    };
    match body {
        CommandBody::Play(path) => ok(mpv.command(&["loadfile", &path, "replace"])),
        CommandBody::Pause => ok(mpv.command(&["set_property", "pause", "yes"])),
        CommandBody::Resume => ok(mpv.command(&["set_property", "pause", "no"])),
        CommandBody::Toggle => ok(mpv.command(&["cycle", "pause"])),
        CommandBody::Seek(ms) => {
            ok(mpv.command(&["seek", &(ms as f64 / 1000.0).to_string(), "absolute"]))
        }
        CommandBody::SetVolume(level) => {
            ok(mpv.command(&["set_property", "volume", &level.to_string()]))
        }
    }
}

fn read_state(mpv: &mut Mpv) -> PlaybackState {
    let path = mpv.get_string("path");
    let Some(path) = path else {
        return PlaybackState { now_playing: None, library_version: 0 };
    };
    let title = mpv.get_string("media-title").unwrap_or_else(|| path.clone());
    let duration_ms = (mpv.get_f64("duration").unwrap_or(0.0) * 1000.0) as i64;
    let position_ms = (mpv.get_f64("time-pos").unwrap_or(0.0) * 1000.0) as i64;
    let paused = mpv.get("pause").and_then(|v| v.as_bool()).unwrap_or(false);
    let volume = mpv.get_f64("volume").unwrap_or(100.0).clamp(0.0, 100.0) as u8;
    PlaybackState {
        now_playing: Some(NowPlaying { path, title, duration_ms, position_ms, paused, volume }),
        library_version: 0,
    }
}

fn handle_connection(stream: TcpStream, ctx: &ServerContext) {
    let Ok(reader_stream) = stream.try_clone() else { return };
    let writer = Arc::new(Mutex::new(stream));
    let authed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let push_writer = Arc::clone(&writer);
    let push_shared = Arc::clone(&ctx.shared);
    let push_authed = Arc::clone(&authed);
    let pusher =
        std::thread::spawn(move || push_state_loop(&push_writer, &push_shared, &push_authed));

    read_command_loop(reader_stream, ctx, &writer, &authed);
    let _ = pusher.join();
}

/// Push a `State` line whenever the version moves on, but only once the connection has authenticated
/// — an unauthenticated socket gets nothing but the ability to pair, including no free look at
/// what is currently playing.
fn push_state_loop(
    writer: &Arc<Mutex<TcpStream>>,
    shared: &Arc<SharedState>,
    authed: &Arc<std::sync::atomic::AtomicBool>,
) {
    let mut last_sent_version = u64::MAX; // Never equal to a real version until one is observed.
    loop {
        std::thread::sleep(STATE_POLL_INTERVAL);
        if !authed.load(Ordering::Acquire) {
            continue;
        }
        let (state, version) = shared.snapshot();
        if version == last_sent_version {
            continue;
        }
        let mut w = writer.lock().unwrap();
        if w.write_all(ServerMessage::State(state).to_line().as_bytes()).is_err() {
            return; // The reader thread will notice the same disconnect and exit on its own.
        }
        drop(w);
        last_sent_version = version;
    }
}

fn read_command_loop(
    stream: TcpStream,
    ctx: &ServerContext,
    writer: &Arc<Mutex<TcpStream>>,
    authed: &Arc<std::sync::atomic::AtomicBool>,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Some(Ok(line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(msg) = ClientMessage::parse(&line) else {
            send(
                writer,
                &ServerMessage::Error { id: "?".into(), message: "malformed message".into() },
            );
            continue;
        };

        if !authed.load(Ordering::Acquire) && !msg.is_pre_auth() {
            send(
                writer,
                &ServerMessage::Error {
                    id: msg.id().to_string(),
                    message: "pair or authenticate first".into(),
                },
            );
            continue;
        }

        let reply = dispatch(msg, ctx, authed);
        send(writer, &reply);
    }
}

fn dispatch(
    msg: ClientMessage,
    ctx: &ServerContext,
    authed: &Arc<std::sync::atomic::AtomicBool>,
) -> ServerMessage {
    let id = msg.id().to_string();
    match msg {
        ClientMessage::Pair { code, .. } => {
            if !ctx.pair_attempts.lock().unwrap().record(SystemTime::now()) {
                return ServerMessage::Error {
                    id,
                    message: "too many wrong pairing attempts; wait a minute and try again".into(),
                };
            }
            let mut pending = ctx.pending_code.lock().unwrap();
            let Some(p) = pending.as_ref() else {
                return ServerMessage::Error { id, message: "no pairing code is active".into() };
            };
            match pairing::judge(p, &code, SystemTime::now()) {
                PairResult::Accepted => {
                    let token = pairing::generate_token(random_bytes_16());
                    ctx.tokens.lock().unwrap().add(token.clone());
                    // A code is single-use: consumed the moment it works, so the same six digits
                    // cannot be replayed by whoever glimpsed them.
                    *pending = None;
                    if let Err(e) = ctx.tokens.lock().unwrap().persist_new(&ctx.token_path, &token) {
                        // The pairing still succeeds for this session — losing persistence should
                        // not fail the thing the user is looking at right now — but it means a
                        // restart will require pairing again, which is worth knowing.
                        eprintln!("warning: could not persist pairing token: {e}");
                    }
                    authed.store(true, Ordering::Release);
                    ServerMessage::Paired { id, token }
                }
                PairResult::WrongCode => {
                    ServerMessage::Error { id, message: "wrong pairing code".into() }
                }
                PairResult::Expired => ServerMessage::Error {
                    id,
                    message: "pairing code expired; restart the server or check its terminal for a new one".into(),
                },
            }
        }
        ClientMessage::Auth { token, .. } => {
            if ctx.tokens.lock().unwrap().is_valid(&token) {
                authed.store(true, Ordering::Release);
                ServerMessage::Reply { id, result: ReplyBody::Ok }
            } else {
                ServerMessage::Error { id, message: "unknown token; pair again".into() }
            }
        }
        ClientMessage::Library { .. } => {
            let scan = ctx.library.lock().unwrap();
            let entries = scan
                .playable()
                .map(|f| LibraryEntry {
                    path: f.path.to_string_lossy().into_owned(),
                    title: f.label(),
                    // Not probed here: a library listing has to stay as cheap as `lumen scan` is
                    // today, and opening every file to learn its length would make listing a large
                    // collection minutes slower than playing anything in it. 0 means "not yet known";
                    // the real duration arrives the moment the file is actually played, in the next
                    // `State` push.
                    duration_ms: 0,
                })
                .collect();
            ServerMessage::Reply { id, result: ReplyBody::Library(entries) }
        }
        ClientMessage::Play { path, .. } => match resolve_playable_path(ctx, &path) {
            Ok(real_path) => run_command(ctx, id, CommandBody::Play(real_path)),
            Err(message) => ServerMessage::Error { id, message },
        },
        ClientMessage::Pause { .. } => run_command(ctx, id, CommandBody::Pause),
        ClientMessage::Resume { .. } => run_command(ctx, id, CommandBody::Resume),
        ClientMessage::TogglePlayPause { .. } => run_command(ctx, id, CommandBody::Toggle),
        ClientMessage::Seek { position_ms, .. } => {
            run_command(ctx, id, CommandBody::Seek(position_ms))
        }
        ClientMessage::SetVolume { level, .. } => {
            run_command(ctx, id, CommandBody::SetVolume(level))
        }
        // No queue exists yet — see the module doc. Refused honestly rather than silently doing
        // nothing, which would look to a client like the button simply did not work.
        ClientMessage::Next { .. } | ClientMessage::Previous { .. } => ServerMessage::Error {
            id,
            message: "no queue yet; play a specific file instead".into(),
        },
    }
}

/// Refuse to hand mpv a path outside the directory `lumen serve` was pointed at.
///
/// Without this, a paired client's `Play{path}` is forwarded to mpv's `loadfile` verbatim — pairing
/// gates *that you can send commands at all*, but said nothing about *which files* a command could
/// name, so a legitimately paired client (or a token stolen off disk) could open any file readable by
/// the mpv process, not just something in the library it was shown. Canonicalizing and checking
/// ancestry closes that: the requested file must resolve to somewhere under the scanned root.
fn resolve_playable_path(ctx: &ServerContext, requested: &str) -> Result<String, String> {
    contain_within_library(&ctx.library_root, requested)
}

/// The pure check behind [`resolve_playable_path`], split out so it can be tested without spinning
/// up a whole `ServerContext` — the containment logic is what matters here, not the plumbing around
/// it.
fn contain_within_library(library_root: &Path, requested: &str) -> Result<String, String> {
    let real = Path::new(requested)
        .canonicalize()
        .map_err(|_| "that file does not exist on this server".to_string())?;
    if !real.starts_with(library_root) {
        return Err("that file is outside the served library".into());
    }
    Ok(real.to_string_lossy().into_owned())
}

fn run_command(ctx: &ServerContext, id: String, body: CommandBody) -> ServerMessage {
    let (reply_tx, reply_rx) = mpsc::channel();
    if ctx.commands.send(Command { body, reply: reply_tx }).is_err() {
        return ServerMessage::Error { id, message: "the player is not responding".into() };
    }
    match reply_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => ServerMessage::Reply { id, result },
        Ok(Err(e)) => ServerMessage::Error { id, message: e },
        Err(_) => ServerMessage::Error { id, message: "timed out waiting for the player".into() },
    }
}

fn send(writer: &Arc<Mutex<TcpStream>>, msg: &ServerMessage) {
    let mut w = writer.lock().unwrap();
    let _ = w.write_all(msg.to_line().as_bytes());
}

/// A `u32` worth of entropy for the pairing code. Not security-critical on its own — the code is
/// six decimal digits either way — but it should not be predictable, and `std` has no random source
/// to reach for without either `unsafe` (denied workspace-wide) or a dependency.
fn random_u32() -> u32 {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("the OS random source must be available");
    u32::from_le_bytes(buf)
}

fn random_bytes_16() -> [u8; 16] {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("the OS random source must be available");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumen-server-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_file_inside_the_library_root_is_allowed() {
        let root = temp_dir("inside");
        let file = root.join("Movie.mkv");
        std::fs::write(&file, b"not a real container, just needs to exist").unwrap();

        let resolved = contain_within_library(&root, file.to_str().unwrap())
            .expect("a file under the served root must be playable");
        assert_eq!(Path::new(&resolved), file);
    }

    #[test]
    fn a_file_outside_the_library_root_is_refused() {
        let root = temp_dir("outside-root");
        let outsider_dir = temp_dir("outside-victim");
        let outsider = outsider_dir.join("not-in-the-library.mkv");
        std::fs::write(&outsider, b"secret").unwrap();

        let err = contain_within_library(&root, outsider.to_str().unwrap())
            .expect_err("a path outside the served root must be refused");
        assert!(err.contains("outside"), "unexpected message: {err}");
    }

    #[test]
    fn a_path_traversal_attempt_out_of_the_library_root_is_refused() {
        let root = temp_dir("traversal-root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let victim_dir = temp_dir("traversal-victim");
        let victim = victim_dir.join("secret.mkv");
        std::fs::write(&victim, b"secret").unwrap();

        // `../` out of the scanned root and into a sibling directory the server was never pointed at.
        let traversal = root
            .join("sub")
            .join("..")
            .join("..")
            .join(victim_dir.file_name().unwrap())
            .join(victim.file_name().unwrap());
        let err = contain_within_library(&root, traversal.to_str().unwrap())
            .expect_err("a `..`-traversal out of the served root must be refused");
        assert!(err.contains("outside"), "unexpected message: {err}");
    }

    #[test]
    fn a_path_that_does_not_exist_is_refused_rather_than_leaking_whether_it_would_be_in_scope() {
        let root = temp_dir("nonexistent-root");
        let missing = root.join("nope.mkv");
        assert!(contain_within_library(&root, missing.to_str().unwrap()).is_err());
    }
}
