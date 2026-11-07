//! Test-only helpers shared by every module in this crate that proves a real subprocess spawn,
//! whether that means writing an executable fake-`ffmpeg` script and then running it, or just
//! forking a real `ffmpeg`/`ffprobe` already on `PATH`.
//!
//! `pub(crate)`, and only ever compiled under `cfg(test)` -- this is not part of the crate's real
//! API, just plumbing shared between [`crate::command`]'s and [`crate::dash`]'s own test modules,
//! which would otherwise each need their own copy (or, worse, their own separate `Mutex`, which would
//! not actually serialize against each other -- see [`SCRIPT_WRITE`]'s own doc comment for why that
//! matters).

/// Guards every test in this crate that spawns a subprocess, across both [`crate::command`] and
/// [`crate::dash`] -- not only the ones that write an executable fake-`ffmpeg` script first, but
/// also the ones that just fork a real `ffmpeg`/`ffprobe` already on `PATH`.
///
/// `cargo test`'s default runner puts each `#[test]` function on its own OS thread and runs several
/// at once. Several tests in this crate write an executable fake-`ffmpeg` script to disk and then
/// `exec` it via `std::process::Command` -- in the same test binary, since `command.rs` and `dash.rs`
/// are both part of this one crate. That shape is exactly what a real `cargo test --workspace` run on
/// this codebase's sibling crate `lumen-exec` once hit as `ExecutableFileBusy` ("Text file busy"):
/// Linux refuses to `exec` a file while any write file descriptor referencing it is still open
/// anywhere in the process, and `Command::spawn`'s underlying `fork` inherits the whole process's
/// file-descriptor table, not just the calling thread's -- so one thread's still-open write handle on
/// its own script can make another, entirely unrelated thread's `exec` fail, even one forking a
/// different file (a real `ffmpeg` binary, say) that was never written by anyone. That means a lock
/// held only by the script-writing tests is not enough on its own: a real-`ffmpeg`-spawning test's own
/// `fork`, landing at the wrong moment, is just as capable of tripping another test's exec as a second
/// script-writer would be, and was reproducibly observed doing exactly that (`ExecutableFileBusy`,
/// roughly one run in three under `cargo test -p lumen-segment --lib`, gone entirely under
/// `--test-threads=1`) before every subprocess-spawning test in this crate was made to hold this same
/// lock. Holding it for the full write-(script-)then-run-it span of each test, whether or not that
/// span includes writing a script, removes the overlap outright: nothing in this crate can be mid
/// write-then-exec, or mid an unrelated fork of its own, at the same moment another test forks. It
/// deliberately does **not** retry a spawn that still races: a retry would hide a real regression in
/// this ordering behind a silent, occasionally-slow pass instead of failing loudly, and it touches no
/// production code -- `execute` itself is never retried.
#[cfg(unix)]
pub(crate) static SCRIPT_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writes `contents` to `path` and marks it executable, safe for an immediate `exec`.
///
/// Calls `File::sync_all` and drops the `File` before `set_permissions` runs, so the write is fully
/// flushed to disk and the write handle is closed before anything -- including the caller's own
/// subsequent spawn -- tries to run the file. Callers must still hold [`SCRIPT_WRITE`] for the full
/// span from calling this through spawning the script; see that lock's own doc comment for why a
/// closed handle here is not, by itself, enough.
#[cfg(unix)]
pub(crate) fn write_executable_script(path: &std::path::Path, contents: &str) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
