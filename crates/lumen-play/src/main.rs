//! `lumen` — a runnable player and library test harness.
//!
//! Point it at your collection. It walks the tree, works out what each file actually is from its
//! bytes, plays the lot through mpv, and records what happened to every file.
//!
//! ```text
//! lumen doctor                          is this machine able to play anything?
//! lumen scan  ~/Media                    what is in the library, and what looks wrong
//! lumen play  ~/Media                    play it
//! lumen test  ~/Media --seconds 20       open every file for 20 s and report the failures
//! ```
//!
//! `test` is the mode worth running first on a large collection: it walks a thousand files in an
//! evening rather than a fortnight, and the output is a list of exactly which ones failed and why.

mod calibration;
mod fidelity;
mod ipc;
mod json;
mod mpvbin;
mod reindex;
mod remote;
mod report;
mod scan;
mod session;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;

use scan::{ScanOptions, playlist_order};
use session::PlayOptions;

const USAGE: &str = "\
lumen — media library player and test harness

  lumen doctor                        check mpv and this machine's decoding support
  lumen setup                         fetch mpv into this folder (Windows), or explain how
  lumen scan  <paths...>              walk the library and report what is there
  lumen items <paths...>              the collection, grouped into films and seasons
  lumen play  <paths...>              play everything found
  lumen test  <paths...>              open every file briefly and report which fail
  lumen serve <path>                  run a persistent player a phone can pair with and control
  lumen unpair [<token>] [--all]      list, or revoke, devices previously paired with `serve`
  lumen reindex <path> [--index <db>] persist an incremental library index; re-probes only what changed
  lumen verify  <path> [--index <db>] re-check indexed files' bytes against their last confirmed digest

Options
  --seconds <n>       play only n seconds of each file (default 20 for `test`)
  --limit <n>         stop after n playable files
  --depth <n>         maximum directory depth
  --identify          compute a content identity per file and report duplicates
  --include-samples   keep files that look like sample clips
  --shuffle           play in random order
  --windowed          do not go fullscreen
  --paused            start paused
  --vo <name>         video output (default gpu-next)
  --hwdec <mode>      hardware decoding (default auto-safe)
  --dry-run           print the mpv command and playlist, launch nothing
  --json <path>       write the machine-readable report here
  --                  everything after this is passed to mpv verbatim

`serve` options
  --port <n>          TCP port to listen on (default 7890)
  --bind <addr>       address to bind (default 0.0.0.0 — every interface)

`unpair` — with no arguments, lists every currently-paired device (by a short prefix, never the
full token). With one, revokes whichever token starts with it — the whole token, or the prefix
`unpair` just showed you. `--all` revokes every paired device at once. The server does not need to
be running; this edits the same token file `serve` reads on its next start.

`reindex` — a persistent alternative to `scan`: the first run probes everything and writes an index
file (default `<path>/.lumen-index`, override with `--index`); every run after that re-probes only
files whose size or modified time actually changed, recognises a renamed-but-identical file as moved
rather than lost, and keeps a tombstoned record for a file that temporarily disappears rather than
forgetting it outright. Safe to interrupt and re-run at any time — nothing is left half-written.

`verify` options
  --reverify-days <n> re-check an already-confirmed file at most this often (default 30; a large
                       file comes due sooner than this on its own — see below)
  --budget <bytes>     stop after reading roughly this many bytes this run (default 8 GiB)

`verify` — re-reads indexed files' actual bytes and compares them to the digest recorded the last
time they were checked, catching corruption a size/mtime check can't: bit rot, a failed write, a bad
sector. Never overprints a mismatch as newly fine — a confirmed-good file that later disagrees stays
flagged, and is re-checked before anything else, until a later pass finds it matching again or
`reindex` sees the file legitimately change. Prioritised, not a flat oldest-first queue: an
unresolved mismatch first, then a file that has never been verified at all, then one `reindex` itself
already flagged as worth a look, then everything else due — oldest-confirmed-first, with a bigger
file coming due sooner than a small one on the same interval. Bounded by `--budget` per run, so a
library too large to verify in one sitting is covered by repeated invocations (a scheduled task, same
as `serve`'s own) rather than one run that never ends. Exits 1 if a mismatch or a read failure was
found this run, 0 otherwise, so a script can act on the difference.

Other
  --help              this text
  --version           version and target platform

Exit codes: 0 all played, 1 at least one file failed, 2 usage or setup error.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("doctor") => doctor(),
        Some("setup") => setup(),
        Some(c @ ("scan" | "items" | "play" | "test")) => match run(c, &args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Some("serve") => match serve(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Some("unpair") => match unpair(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Some("reindex") => match reindex_cmd(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Some("verify") => match verify_cmd(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        // Expected of anything shipped, and the first thing anyone asks a binary that misbehaves.
        // Reports the target it was built for, not the host running it: a bug report saying
        // "lumen 0.1.0" is far less useful than one naming the architecture.
        Some("--version" | "-V" | "version") => {
            println!(
                "lumen {} ({} {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().take_while(|a| *a != "--").any(|a| a == name)
}

fn value(args: &[String], name: &str) -> Option<String> {
    let stop = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let i = args[..stop].iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

/// Arguments after a bare `--`, passed to mpv verbatim.
fn passthrough(args: &[String]) -> Vec<String> {
    match args.iter().position(|a| a == "--") {
        Some(i) => args[i + 1..].to_vec(),
        None => Vec::new(),
    }
}

/// Everything that is neither a flag, a flag's value, nor after `--`.
fn positional(args: &[String]) -> Vec<PathBuf> {
    const TAKES_VALUE: &[&str] = &["--seconds", "--limit", "--depth", "--vo", "--hwdec", "--json"];
    let stop = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < stop {
        let a = &args[i];
        if TAKES_VALUE.contains(&a.as_str()) {
            i += 2;
            continue;
        }
        if a.starts_with("--") {
            i += 1;
            continue;
        }
        out.push(PathBuf::from(a));
        i += 1;
    }
    out
}

fn run(cmd: &str, args: &[String]) -> Result<ExitCode, String> {
    let roots = positional(args);
    if roots.is_empty() {
        return Err("give me at least one file or folder to scan".into());
    }

    let opts = ScanOptions {
        identify: flag(args, "--identify"),
        include_samples: flag(args, "--include-samples"),
        limit: value(args, "--limit").and_then(|v| v.parse().ok()),
        max_depth: value(args, "--depth").and_then(|v| v.parse().ok()),
    };

    eprintln!("scanning {} path(s)...", roots.len());
    let found = scan::scan(&roots, &opts);
    println!("{}", report::render_scan(&found));

    if cmd == "items" {
        println!("{}", report::render_items(&found));
        write_json(args, &found, None)?;
        return Ok(ExitCode::SUCCESS);
    }
    if cmd == "scan" {
        write_json(args, &found, None)?;
        return Ok(ExitCode::SUCCESS);
    }

    let order = playlist_order(&found);
    if order.is_empty() {
        return Err(
            "the scan found no playable files. Check the path, or pass a file directly.".into()
        );
    }

    // `test` exists to walk a whole library quickly, so it defaults to a short look at each file.
    // `play` is for watching, so it has no limit unless one is asked for.
    let default_seconds = if cmd == "test" { Some(20) } else { None };
    // Built from the defaults rather than field by field, so the default hwdec and video output
    // live in exactly one place and cannot drift between here and the session layer.
    let mut play = PlayOptions::new();
    play.seconds_each = value(args, "--seconds").and_then(|v| v.parse().ok()).or(default_seconds);
    play.start_paused = flag(args, "--paused");
    play.vo = value(args, "--vo");
    play.fullscreen = !flag(args, "--windowed");
    play.shuffle = flag(args, "--shuffle");
    play.extra_args = passthrough(args);
    play.dry_run = flag(args, "--dry-run");
    if let Some(h) = value(args, "--hwdec") {
        play.hwdec = h;
    }

    match &play.seconds_each {
        Some(n) => println!("playing {} files, {n} s each\n", order.len()),
        None => println!("playing {} files\n", order.len()),
    }

    let session = session::run(&found, &order, &play, |r, n, total| {
        println!("{}", report::render_progress(r, n, total));
    })?;

    if play.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    println!("{}", report::render_session(&session));
    record_calibration(&session);
    write_json(args, &found, Some(&session))?;

    // A failed file is the finding. Exiting zero on a run that could not open half the library would
    // make this useless in a script.
    Ok(if session.failed().count() > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// Append a calibration entry for every file this session actually played -- `docs/15` §C. A file
/// that failed or was never reached, or one with no video track to compare, yields nothing to record
/// (`calibration::observe` already says so); failure to write the log is reported once rather than
/// aborting a run that otherwise succeeded, since the log is evidence for later, not something this
/// session's own exit code should depend on.
fn record_calibration(session: &session::SessionReport) {
    let log_path = calibration::default_log_path();
    let mut misses = 0usize;
    let mut recorded = 0usize;
    for r in &session.results {
        let Some(entry) = calibration::observe(r) else { continue };
        if entry.hardware_decode_as_predicted() == Some(false) {
            misses += 1;
        }
        match calibration::append(&log_path, &entry) {
            Ok(()) => recorded += 1,
            Err(e) => {
                eprintln!("calibration: could not write {}: {e}", log_path.display());
                break;
            }
        }
    }
    if misses > 0 {
        eprintln!(
            "calibration: {misses}/{recorded} played file{} did not decode the way the fidelity \
             model predicted -- see `lumen doctor` for details",
            if recorded == 1 { "" } else { "s" }
        );
    }
}

/// A persistent, remotely controllable player. Runs until the process is killed.
fn serve(args: &[String]) -> Result<ExitCode, String> {
    // The one positional argument: the library path. A small hand-rolled walk rather than reusing
    // `positional()`'s `TAKES_VALUE` list, which belongs to `play`/`test`'s own options and has
    // nothing to do with `--port`/`--bind`.
    let stop = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut root = None;
    let mut i = 0;
    while i < stop {
        let a = &args[i];
        if a == "--port" || a == "--bind" {
            i += 2;
            continue;
        }
        if !a.starts_with("--") && root.is_none() {
            root = Some(PathBuf::from(a));
        }
        i += 1;
    }
    let root = root.ok_or("usage: lumen serve <path> [--port <n>] [--bind <addr>]")?;

    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }

    let port: u16 = value(args, "--port")
        .map(|v| v.parse().map_err(|_| format!("--port must be a number, got {v:?}")))
        .transpose()?
        .unwrap_or(7890);
    let bind = value(args, "--bind").unwrap_or_else(|| "0.0.0.0".to_string());

    println!(
        "lumen serve — do not forward this port through your router; it is meant for your own LAN"
    );
    remote::server::run(&root, &bind, port, &passthrough(args), |line| println!("{line}"))?;
    Ok(ExitCode::SUCCESS)
}

/// List or revoke devices previously paired with `serve`, without needing the server running.
///
/// The one gap the pairing design (`remote/pairing.rs`) left open on purpose, until now: a token
/// was append-only, so getting rid of a compromised or no-longer-trusted device meant finding and
/// hand-editing `paired-clients.txt`. This is that same file, edited by the tool that owns its
/// format instead of by hand.
fn unpair(args: &[String]) -> Result<ExitCode, String> {
    let path = remote::pairing::TokenStore::default_path();
    let mut store = remote::pairing::TokenStore::load(&path);

    if args.iter().any(|a| a == "--all") {
        let n = store.clear();
        store
            .persist_all(&path)
            .map_err(|e| format!("could not update {}: {e}", path.display()))?;
        println!("revoked {n} paired device{}", if n == 1 { "" } else { "s" });
        return Ok(ExitCode::SUCCESS);
    }

    let Some(target) = args.iter().find(|a| !a.starts_with("--")) else {
        if store.is_empty() {
            println!("no devices are currently paired");
        } else {
            println!("{} paired device{}:", store.len(), if store.len() == 1 { "" } else { "s" });
            // A prefix, never the full token: this is printed to a terminal, and a terminal's
            // scrollback is exactly the kind of place a bearer token should not end up sitting.
            let mut shown: Vec<&str> = store.tokens().collect();
            shown.sort_unstable();
            for t in shown {
                println!("  {}…", &t[..8.min(t.len())]);
            }
            println!("\nrevoke one with: lumen unpair <prefix-shown-above>");
        }
        return Ok(ExitCode::SUCCESS);
    };

    let removed = store.remove_matching(target);
    if removed.is_empty() {
        return Err(format!("no paired device matches {target:?}"));
    }
    store.persist_all(&path).map_err(|e| format!("could not update {}: {e}", path.display()))?;
    println!("revoked {} device{}", removed.len(), if removed.len() == 1 { "" } else { "s" });
    Ok(ExitCode::SUCCESS)
}

/// Persist an incremental library index: probe everything the first time, only what changed after
/// that. See `reindex.rs` for the actual reconciliation; this is just argument parsing and the
/// human-readable summary line.
///
/// Always exits 0, deliberately unlike `verify` below, even when `report.failed > 0`. A probe
/// flagging `needs_review` (an extension mismatch, a suspicious size) is the same soft, "worth a
/// look" signal `Index::verify`'s own tier 3 treats as elevated-but-not-urgent -- distinct from a
/// verify pass's mismatch or read failure, which is a confirmed problem a script gating on exit code
/// should actually stop on. Two different exit-code conventions for two genuinely different strengths
/// of signal, not an inconsistency between siblings.
fn reindex_cmd(args: &[String]) -> Result<ExitCode, String> {
    let root = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .ok_or("usage: lumen reindex <path> [--index <db>]")?;
    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }

    let db =
        value(args, "--index").map_or_else(|| reindex::default_index_path(&root), PathBuf::from);

    let (index, report) = reindex::run(&root, &db)?;
    println!("{}", reindex::summarize(&index, &report));
    if report.failed > 0 {
        println!("(see the index file for which paths need a look: {})", db.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Re-check indexed files' actual bytes against their last confirmed digest. See `verify.rs` for
/// the tier-prioritised selection and the real file read; this is argument parsing, the summary
/// line, and the exit-code convention documented in `USAGE` (1 means "something needs a look").
fn verify_cmd(args: &[String]) -> Result<ExitCode, String> {
    let root = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from).ok_or(
        "usage: lumen verify <path> [--index <db>] [--reverify-days <n>] [--budget <bytes>]",
    )?;
    // `reindex_cmd` checks this; `verify` used to silently accept a nonexistent root and report
    // "0 confirmed" instead -- misleading either way root actually matters here: with `--index`
    // given explicitly, the root is otherwise unused entirely and a typo would go unnoticed, and
    // without it, the derived default index path would just as silently never exist either.
    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }

    let db =
        value(args, "--index").map_or_else(|| reindex::default_index_path(&root), PathBuf::from);

    let reverify_days = value(args, "--reverify-days")
        .map(|v| v.parse().map_err(|_| format!("--reverify-days must be a number, got {v:?}")))
        .transpose()?
        .unwrap_or(verify::DEFAULT_REVERIFY_DAYS);
    let budget_bytes = value(args, "--budget")
        .map(|v| v.parse().map_err(|_| format!("--budget must be a number of bytes, got {v:?}")))
        .transpose()?
        .unwrap_or(verify::DEFAULT_BUDGET_BYTES);

    let (_, report) = verify::run(&db, reverify_days, budget_bytes)?;
    println!("{}", verify::summarize(&report));
    Ok(if report.found_a_problem() { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn write_json(
    args: &[String],
    found: &scan::Scan,
    session: Option<&session::SessionReport>,
) -> Result<(), String> {
    let Some(path) = value(args, "--json") else { return Ok(()) };
    let text = report::render_json(found, session);
    std::fs::write(&path, text).map_err(|e| format!("cannot write {path}: {e}"))?;
    println!("report written to {path}");
    Ok(())
}

/// Is this machine able to play anything, and with what?
fn doctor() -> ExitCode {
    println!(
        "lumen {}  ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if let Ok(p) = std::env::current_exe() {
        println!("  running from {}", p.display());
    }

    println!("\nmpv");
    let Some(mpv) = mpvbin::find() else {
        println!("  NOT FOUND\n");
        for line in mpvbin::install_hint().lines() {
            println!("  {line}");
        }
        return ExitCode::from(2);
    };
    println!("  {}", mpvbin::version(&mpv).unwrap_or_else(|| "(no version line)".into()));
    println!("  at {}", mpv.display());

    let vos = mpvbin::list(&mpv, "--vo=help");
    println!("\nvideo output");
    if vos.iter().any(|v| v == "gpu-next") {
        println!("  gpu-next present — the libplacebo renderer this product is built on");
    } else if vos.is_empty() {
        println!("  could not be queried; this mpv may be a wrapper rather than the real binary");
    } else {
        println!(
            "  gpu-next MISSING (have: {}). Playback still works via `gpu`, but HDR handling\n  \
             and tone mapping differ from what the product will ship. Run with --vo gpu.",
            vos.join(", ")
        );
    }

    let hw = mpvbin::list(&mpv, "--hwdec=help");
    let real: Vec<&String> = hw.iter().filter(|h| *h != "no" && *h != "auto").collect();
    println!("\nhardware decoding");
    if real.is_empty() {
        println!(
            "  none reported. Files will decode on the CPU: 4K HEVC and AV1 may stutter, and\n  \
             that is a driver problem rather than anything to do with the file."
        );
    } else {
        println!("  {}", real.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
        // `--hwdec=help` lists what mpv was *compiled* with, not what works here. A build with
        // nvdec support on a machine with no NVIDIA driver lists nvdec and then fails to load
        // libcuda at playback. `lumen test` reports what each file actually decoded with, which is
        // the number to trust.
        println!("  (compiled-in support; `lumen test` reports what actually decoded)");
    }

    println!("\nfidelity calibration");
    let log_path = calibration::default_log_path();
    match calibration::read_all(&log_path) {
        Ok(entries) => println!("  {}", calibration::summarize(&entries)),
        Err(e) => println!("  could not read {}: {e}", log_path.display()),
    }

    println!("\nnext");
    println!("  lumen scan  <your media folder>");
    println!("  lumen test  <your media folder> --limit 5");
    ExitCode::SUCCESS
}

/// Fetch mpv into the folder holding this binary, so the pair is portable afterwards.
///
/// Deliberately does not install anything system-wide, touch the registry, or require an elevated
/// prompt. It puts one file next to this one; deleting the folder undoes it completely.
fn setup() -> ExitCode {
    if let Some(existing) = mpvbin::find() {
        println!("mpv is already available at {}", existing.display());
        println!("Nothing to do. Run `lumen doctor` to see what it supports.");
        return ExitCode::SUCCESS;
    }

    let Ok(exe) = std::env::current_exe() else {
        eprintln!("cannot determine where this binary lives; install mpv by hand:\n");
        eprintln!("{}", mpvbin::install_hint());
        return ExitCode::from(2);
    };
    let dir = exe.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();

    if !cfg!(windows) {
        // On Linux and macOS the package manager is the right answer and needs no help from here.
        println!("{}", mpvbin::install_hint());
        return ExitCode::from(2);
    }

    println!("mpv is not present. Fetching a Windows build into:\n  {}\n", dir.display());
    println!("This downloads from the official mpv Windows builds and extracts mpv.exe here.");
    println!("Nothing is installed system-wide and no registry keys are written.\n");

    // PowerShell does the work: it is present on every supported Windows, handles TLS and redirects,
    // and `tar` (libarchive, shipped since Windows 10 1803) reads the 7-Zip archive the builds use.
    // Shelling out beats reimplementing HTTPS and 7-Zip in a binary that has no dependencies.
    //
    // SourceForge first because that is the source mpv.io itself links to for Windows, and its URL
    // shape is stable. GitHub is the fallback: its API is rate-limited unauthenticated, which is
    // fine for one call but not something to depend on first.
    //
    // Both sources are tried for the *download itself*, not just the version lookup — a GitHub
    // Actions runner hitting SourceForge's `/download` redirector has been observed getting back a
    // small mirror-selection or rate-limit page instead of the archive, on a run where the listing
    // page it walked moments earlier loaded fine. A `try` around the lookup alone never sees that
    // failure, so the GitHub fallback never fires: the fix is a real fallback chain over the whole
    // fetch, and validating the result by its actual 7-Zip file signature rather than by size, since
    // an interstitial page is not guaranteed to land under any particular byte threshold.
    let script = r#"
$ErrorActionPreference = 'Stop'
$dest = $args[0]
$tmp  = Join-Path $env:TEMP ("lumen-mpv-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
$ProgressPreference = 'SilentlyContinue'
# A generic browser-like UA: SourceForge has been seen serving a different (small, non-archive)
# response to requests that look scripted, and Invoke-WebRequest's default UA names itself and the
# PowerShell version outright.
$ua = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) lumen-setup'

function Get-Candidates {
  $out = @()
  try {
    Write-Host 'Looking up the latest mpv build (sourceforge)...'
    $listing = Invoke-WebRequest -UseBasicParsing -UserAgent $ua -Uri 'https://sourceforge.net/projects/mpv-player-windows/files/release/'
    $vers = [regex]::Matches($listing.Content, 'mpv-(\d+\.\d+\.\d+)-x86_64\.7z') |
            ForEach-Object { $_.Groups[1].Value } | Sort-Object { [version]$_ } -Unique
    if ($vers) {
      $v = $vers[-1]
      Write-Host ("  mpv " + $v + " (sourceforge)")
      $out += "https://sourceforge.net/projects/mpv-player-windows/files/release/mpv-$v-x86_64.7z/download"
    }
  } catch { Write-Host '  sourceforge lookup failed' }
  try {
    $rel = Invoke-RestMethod -Uri 'https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest' -Headers @{ 'User-Agent' = 'lumen' }
    $a = $rel.assets | Where-Object { $_.name -like 'mpv-x86_64-2*' -and $_.name -like '*.7z' -and $_.name -notlike '*v3*' } | Select-Object -First 1
    if ($a) { Write-Host ("  " + $a.name + " (github)"); $out += $a.browser_download_url }
  } catch { }
  return $out
}

# A 7-Zip archive always opens with this six-byte signature. Checking that, rather than a size
# threshold, is what actually distinguishes "the real archive" from "some other small-ish response
# that happened to clear an arbitrary byte count" — a captive portal or rate-limit page is not
# guaranteed to land under any particular size.
function Test-SevenZipSignature($path) {
  $sig = [byte[]](0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C)
  $bytes = [System.IO.File]::ReadAllBytes($path)
  if ($bytes.Length -lt $sig.Length) { return $false }
  for ($i = 0; $i -lt $sig.Length; $i++) { if ($bytes[$i] -ne $sig[$i]) { return $false } }
  return $true
}

try {
  $candidates = Get-Candidates
  if (-not $candidates) { throw 'could not find an mpv download' }

  $archive = Join-Path $tmp 'mpv.7z'
  $downloaded = $false
  foreach ($url in $candidates) {
    try {
      Write-Host "Downloading from $url ..."
      Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing -UserAgent $ua
      if (Test-SevenZipSignature $archive) { $downloaded = $true; break }
      Write-Host '  not a real 7-Zip archive (likely a mirror-selection or rate-limit page) -- trying the next source'
    } catch {
      Write-Host "  download failed: $($_.Exception.Message) -- trying the next source"
    }
  }
  if (-not $downloaded) { throw 'every mpv download source returned something that was not a real archive' }

  Write-Host 'Extracting...'
  $extracted = $false
  # 7z if the user has it; otherwise tar.exe, which is libarchive and reads 7-Zip.
  foreach ($try in @(
      { & 7z x $archive ('-o' + $tmp) -y | Out-Null },
      { & tar -xf $archive -C $tmp })) {
    try { & $try; if (Get-ChildItem -Path $tmp -Filter mpv.exe -Recurse) { $extracted = $true; break } }
    catch { }
  }
  if (-not $extracted) {
    throw 'could not unpack the archive. Install 7-Zip (https://7-zip.org) and run setup again, or extract mpv.exe by hand.'
  }

  $found = Get-ChildItem -Path $tmp -Filter mpv.exe -Recurse | Select-Object -First 1
  Copy-Item $found.FullName (Join-Path $dest 'mpv.exe') -Force
  # Shader compilation on the older D3D paths wants this; it ships in the upstream archive.
  $d3d = Get-ChildItem -Path $tmp -Filter 'd3dcompiler_*.dll' -Recurse | Select-Object -First 1
  if ($d3d) { Copy-Item $d3d.FullName $dest -Force }
  Write-Host ('Installed mpv.exe into ' + $dest)
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
"#;

    // Written to a temp file and run with `-File` rather than passed inline via `-Command`.
    // `-Command`'s documented `-args <arg-array>` passthrough is specified for an actual ScriptBlock
    // literal (`{ ... }`, only constructible from inside PowerShell itself) — not for plain script
    // text handed in from an external process, which is all this ever was. That mismatch is exactly
    // what left `$dest` as `$null` here: nothing in the script reads it until the very last few
    // lines, so it went unnoticed until `lumen setup` was actually run to completion for the first
    // time. `-File` binds trailing arguments to `$args` unambiguously, which is the whole reason it
    // exists.
    let script_path = std::env::temp_dir().join(format!("lumen-setup-{}.ps1", std::process::id()));
    if let Err(e) = std::fs::write(&script_path, script) {
        eprintln!("cannot write a temporary setup script to {}: {e}\n", script_path.display());
        eprintln!("{}", mpvbin::install_hint());
        return ExitCode::from(2);
    }

    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg(&dir)
        .status();
    let _ = std::fs::remove_file(&script_path);

    match status {
        Ok(s) if s.success() => {
            println!("\nDone. Checking what it can do:\n");
            doctor()
        }
        _ => {
            // A failed download is not a dead end — the manual route is three steps and always works.
            eprintln!("\nAutomatic setup did not complete. Do it by hand instead:\n");
            eprintln!("{}", mpvbin::install_hint());
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn paths_are_separated_from_flags_and_their_values() {
        let a = argv(&["/media/films", "--seconds", "20", "--shuffle", "/media/tv"]);
        assert_eq!(positional(&a), vec![PathBuf::from("/media/films"), PathBuf::from("/media/tv")]);
        assert_eq!(value(&a, "--seconds").as_deref(), Some("20"));
        assert!(flag(&a, "--shuffle"));
        assert!(!flag(&a, "--paused"));
    }

    #[test]
    fn a_flag_value_is_never_mistaken_for_a_path() {
        // Without the takes-a-value list, `20` would become a path and the scan would report it
        // missing — a confusing failure for a perfectly correct command line.
        let a = argv(&["--limit", "20", "/media"]);
        assert_eq!(positional(&a), vec![PathBuf::from("/media")]);
    }

    #[test]
    fn everything_after_a_bare_dash_dash_goes_to_mpv() {
        let a = argv(&["/media", "--", "--vo=x11", "--gpu-api=vulkan"]);
        assert_eq!(passthrough(&a), vec!["--vo=x11", "--gpu-api=vulkan"]);
        assert_eq!(positional(&a), vec![PathBuf::from("/media")]);
        // A flag after `--` belongs to mpv, not to us. Reading it as ours would silently change
        // behaviour the user meant for the player.
        let b = argv(&["/media", "--", "--shuffle"]);
        assert!(!flag(&b, "--shuffle"));
        assert_eq!(value(&b, "--vo"), None);
    }

    #[test]
    fn a_path_after_the_separator_is_left_to_mpv() {
        let a = argv(&["/media", "--", "--sub-file", "/subs/a.srt"]);
        assert_eq!(positional(&a), vec![PathBuf::from("/media")]);
        assert_eq!(passthrough(&a).len(), 2);
    }

    #[test]
    fn a_missing_value_is_none_rather_than_a_panic() {
        let a = argv(&["--limit"]);
        assert_eq!(value(&a, "--limit"), None);
        assert!(positional(&a).is_empty());
    }

    #[test]
    fn no_paths_is_an_actionable_error() {
        let err = run("scan", &argv(&["--shuffle"])).unwrap_err();
        assert!(err.contains("at least one file or folder"), "{err}");
    }
}
