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

mod ipc;
mod json;
mod report;
mod scan;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;

use scan::{ScanOptions, playlist_order};
use session::PlayOptions;

const USAGE: &str = "\
lumen — media library player and test harness

  lumen doctor                        check mpv and this machine's decoding support
  lumen scan  <paths...>              walk the library and report what is there
  lumen items <paths...>              the collection, grouped into films and seasons
  lumen play  <paths...>              play everything found
  lumen test  <paths...>              open every file briefly and report which fail

Options
  --seconds <n>       play only n seconds of each file (default 20 for `test`)
  --limit <n>         stop after n playable files
  --depth <n>         maximum directory depth
  --include-samples   keep files that look like sample clips
  --shuffle           play in random order
  --windowed          do not go fullscreen
  --paused            start paused
  --vo <name>         video output (default gpu-next)
  --hwdec <mode>      hardware decoding (default auto-safe)
  --dry-run           print the mpv command and playlist, launch nothing
  --json <path>       write the machine-readable report here
  --                  everything after this is passed to mpv verbatim

Exit codes: 0 all played, 1 at least one file failed, 2 usage or setup error.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("doctor") => doctor(),
        Some(c @ ("scan" | "items" | "play" | "test")) => match run(c, &args[1..]) {
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
    write_json(args, &found, Some(&session))?;

    // A failed file is the finding. Exiting zero on a run that could not open half the library would
    // make this useless in a script.
    Ok(if session.failed().count() > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
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
    let mut ok = true;
    println!("mpv");
    match std::process::Command::new("mpv").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            println!("  {}", text.lines().next().unwrap_or("(no version line)"));
        }
        _ => {
            ok = false;
            println!("  NOT FOUND. Install it — everything here plays through mpv.");
            println!("    Windows   winget install mpv.net");
            println!("    macOS     brew install mpv");
            println!("    Linux     apt install mpv   (or dnf/pacman/zypper)");
        }
    }

    let list = |arg: &str| -> Vec<String> {
        std::process::Command::new("mpv")
            .arg(arg)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                    .filter(|s| !s.is_empty() && !s.starts_with('-'))
                    .collect()
            })
            .unwrap_or_default()
    };

    if ok {
        let vos = list("--vo=help");
        println!("\nvideo output");
        if vos.iter().any(|v| v == "gpu-next") {
            println!("  gpu-next present — the libplacebo renderer this product is built on");
        } else {
            println!(
                "  gpu-next MISSING (have: {}). Playback still works via `gpu`, but HDR handling\n  \
                 and tone mapping differ from what the product will ship.",
                vos.join(", ")
            );
        }

        let hw = list("--hwdec=help");
        println!("\nhardware decoding");
        let real: Vec<&String> = hw.iter().filter(|h| *h != "no" && *h != "auto").collect();
        if real.is_empty() {
            println!(
                "  none reported. Files will decode on the CPU: 4K HEVC and AV1 may stutter, and\n  \
                 that is a driver problem rather than anything to do with the file."
            );
        } else {
            println!("  {}", real.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
        }
    }

    println!("\nnext");
    println!("  lumen scan  <your media folder>");
    println!("  lumen test  <your media folder> --seconds 20 --json report.json");
    if ok { ExitCode::SUCCESS } else { ExitCode::from(2) }
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
