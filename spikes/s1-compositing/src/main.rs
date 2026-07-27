//! Phase 0 spike **S1**: can a WebView UI composite over hardware video without tearing or lag?
//!
//! See `docs/09-roadmap.md` §2. The answer decides the desktop shell: pass keeps Tauri v2, fail
//! triggers the documented fallback to Qt 6 (ADR-0001).
//!
//! ## What it measures
//!
//! Two stages with **identical player configuration**, compared as a pair:
//!
//! 1. **baseline** — mpv alone, fullscreen. Establishes what this machine can do at all.
//! 2. **composited** — the same clip inside the Tauri shell with an HTML OSD over it.
//!
//! The verdict is the *delta*. Measuring only the composited stage conflates "compositing costs
//! frames" with "this machine cannot decode 4K HDR", and those have opposite consequences.
//!
//! ## Usage
//!
//! ```text
//! cargo run -p s1-compositing -- probe
//! cargo run -p s1-compositing -- run --profile profiles/desktop.toml --clip /path/to/clip.mkv
//! cargo run -p s1-compositing -- run --profile profiles/laptop.toml  --clip /path/to/clip.mkv
//! ```

mod mpv_ipc;
mod pacing;
mod probe;
mod profile;
mod report;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pacing::{Sample, StageStats};
use profile::Profile;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") => {
            let env = probe::probe();
            println!("{}", report::render_environment(&env));
            std::process::ExitCode::SUCCESS
        }
        Some("run") => match run(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::ExitCode::from(2)
            }
        },
        _ => {
            eprintln!("{USAGE}");
            std::process::ExitCode::from(2)
        }
    }
}

const USAGE: &str = "\
S1 compositing spike

  probe                                   report this machine's environment
  run --profile <file> --clip <file>      run the paired measurement

Options for `run`:
  --profile <path>    profiles/desktop.toml or profiles/laptop.toml
  --clip <path>       a demanding test clip (4K HDR remux excerpt is ideal)
  --shell <cmd>       command that launches the Tauri shell. It must accept
                      LUMEN_S1_IPC and LUMEN_S1_CLIP from the environment.
                      Omit to run the baseline stage only.
  --out <path>        write the JSON report here (default: s1-report.json)
  --osd-latency <ms>  OSD latency measured by the shell, if it reported one";

fn flag_value(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn run(args: &[String]) -> Result<std::process::ExitCode, String> {
    let profile_path =
        flag_value(args, "--profile").ok_or("--profile is required (see profiles/)")?;
    let clip = flag_value(args, "--clip").ok_or("--clip is required")?;
    let shell_cmd = flag_value(args, "--shell");
    let out_path = flag_value(args, "--out").unwrap_or_else(|| "s1-report.json".into());
    let osd_latency_ms: Option<f64> =
        flag_value(args, "--osd-latency").and_then(|v| v.parse().ok());

    let profile = Profile::load(&PathBuf::from(&profile_path)).map_err(|e| e.to_string())?;
    if !PathBuf::from(&clip).exists() {
        return Err(format!("clip not found: {clip}"));
    }

    let env = probe::probe();
    let warnings =
        probe::environment_warnings(&env, profile.check_refresh_match, profile.expect_discrete_gpu);

    println!("{}", report::render_environment(&env));
    println!("profile: {} — {}\n", profile.name, profile.description);
    for w in &warnings {
        println!("{}\n", report::render_warning(w));
    }
    if env.mpv_version.is_none() {
        return Err("mpv is required; run the bootstrap script for this platform".into());
    }

    // A laptop profile repeats the whole pair unplugged. Hybrid machines park the discrete GPU on
    // battery, which halves the frame rate without reporting a single late frame — invisible to a
    // mains-only run.
    if profile.test_on_battery {
        match env.on_battery {
            Some(true) => println!("  Running on BATTERY. Repeat on mains for the comparison.\n"),
            Some(false) => println!(
                "  Running on MAINS. When this pass finishes, unplug and run it again — that \
                 comparison is the point of the laptop profile.\n"
            ),
            None => println!("  Power source could not be detected; record it by hand.\n"),
        }
    }

    println!("stage 1/2: baseline (mpv alone), {} s ...", profile.run_seconds);
    let baseline = measure_stage("baseline", &clip, &profile, None)?;
    println!("{}\n", report::render_stage(&baseline));

    let composited = match &shell_cmd {
        Some(cmd) => {
            println!("stage 2/2: composited (shell + HTML OSD), {} s ...", profile.run_seconds);
            Some(measure_stage("composited", &clip, &profile, Some(cmd))?)
        }
        None => {
            println!(
                "stage 2/2: SKIPPED — no --shell given. The baseline alone cannot answer S1; \
                 build the shell in ui/ and re-run with --shell.\n"
            );
            None
        }
    };

    let verdict = composited.as_ref().map(|c| {
        let v = pacing::judge(&baseline, c, &profile.thresholds, osd_latency_ms);
        println!("{}", report::render_stage(c));
        println!("\n{}", report::render_verdict(&v));
        v
    });

    let json = report::render_json(
        &env,
        &profile,
        &baseline,
        composited.as_ref(),
        verdict.as_ref(),
        &warnings,
    );
    std::fs::write(&out_path, json).map_err(|e| format!("cannot write {out_path}: {e}"))?;
    println!("\nreport written to {out_path}");

    if profile.thermal_soak_minutes > 0 {
        println!(
            "\nNEXT: this profile asks for a {}-minute sustained pass. Re-run with \
             `run_seconds = {}` in a copy of the profile — a laptop that passes for two minutes and \
             throttles at ten has not passed.",
            profile.thermal_soak_minutes,
            profile.thermal_soak_minutes * 60
        );
    }

    Ok(match verdict.map(|v| v.outcome) {
        Some(pacing::Outcome::Pass | pacing::Outcome::PassWithNotes) => {
            std::process::ExitCode::SUCCESS
        }
        Some(pacing::Outcome::Fail) => std::process::ExitCode::from(1),
        // Inconclusive and skipped are both "no answer yet", which is not a pass.
        _ => std::process::ExitCode::from(3),
    })
}

/// Launch a stage, poll counters, and summarise.
fn measure_stage(
    label: &str,
    clip: &str,
    profile: &Profile,
    shell_cmd: Option<&str>,
) -> Result<StageStats, String> {
    let ipc = mpv_ipc::default_ipc_path(label);
    let _ = std::fs::remove_file(&ipc); // a stale socket blocks the new one

    let mut child = spawn_stage(clip, &ipc, profile, shell_cmd)?;

    let connect = mpv_ipc::MpvIpc::connect(&ipc, Duration::from_secs(20));
    let mut conn = match connect {
        Ok(c) => c,
        Err(e) => {
            let _ = child.kill();
            return Err(format!(
                "could not reach mpv's IPC socket at {ipc}: {e}. If this is the composited stage, \
                 the shell must pass LUMEN_S1_IPC through to libmpv's `input-ipc-server` option."
            ));
        }
    };

    let start = Instant::now();
    let deadline = Duration::from_secs(profile.run_seconds);
    let mut samples: Vec<Sample> = Vec::new();
    while start.elapsed() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = status;
            break; // the player exited early; summarise what was gathered
        }
        let at_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match conn.sample(at_ms) {
            Ok(s) => samples.push(s),
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(profile.sample_interval_ms));
    }

    conn.quit();
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&ipc);

    if samples.is_empty() {
        return Err(format!("stage `{label}` produced no samples"));
    }
    Ok(pacing::summarize(label, &samples, profile.warmup_ms))
}

fn spawn_stage(
    clip: &str,
    ipc: &str,
    profile: &Profile,
    shell_cmd: Option<&str>,
) -> Result<Child, String> {
    match shell_cmd {
        // The shell owns the window and embeds libmpv itself; the clip and socket arrive by
        // environment so the shell needs no bespoke argument parsing.
        Some(cmd) => {
            let mut parts = cmd.split_whitespace();
            let program = parts.next().ok_or("--shell was empty")?;
            Command::new(program)
                .args(parts)
                .env("LUMEN_S1_IPC", ipc)
                .env("LUMEN_S1_CLIP", clip)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| format!("cannot launch shell `{cmd}`: {e}"))
        }
        None => Command::new("mpv")
            .args(mpv_ipc::common_mpv_args(clip, ipc, profile.run_seconds + 30))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot launch mpv: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_read_by_name_regardless_of_order() {
        let args: Vec<String> =
            ["--clip", "a.mkv", "--profile", "p.toml"].iter().map(|s| (*s).to_string()).collect();
        assert_eq!(flag_value(&args, "--profile"), Some("p.toml".into()));
        assert_eq!(flag_value(&args, "--clip"), Some("a.mkv".into()));
        assert_eq!(flag_value(&args, "--missing"), None);
    }

    #[test]
    fn a_flag_with_no_value_is_none_rather_than_a_panic() {
        let args: Vec<String> = vec!["--clip".to_string()];
        assert_eq!(flag_value(&args, "--clip"), None);
    }
}
