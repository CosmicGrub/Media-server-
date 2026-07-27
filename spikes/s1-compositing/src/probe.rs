//! Environment probe.
//!
//! Records what the machine actually is, so a result can be interpreted six months later. A verdict
//! with no record of which GPU drove it, what the display was doing, or whether the machine was on
//! battery is not a result — it is an anecdote.
//!
//! Everything here degrades to `None` rather than failing: a missing detail must never stop the run.

use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    pub os: String,
    pub arch: String,
    pub cpu_model: Option<String>,
    pub cpu_threads: Option<usize>,
    pub memory_gb: Option<f64>,
    /// Every GPU the system reports, in the order it reports them.
    pub gpus: Vec<String>,
    /// True when more than one GPU is present — a hybrid machine, where which one renders is a
    /// question rather than a given.
    pub hybrid_graphics: bool,
    pub display_resolution: Option<String>,
    pub display_refresh_hz: Option<f64>,
    pub on_battery: Option<bool>,
    pub mpv_version: Option<String>,
    /// mpv's video output backends, which decide whether `gpu-next` is even available.
    pub mpv_vo: Vec<String>,
    /// Hardware decoders mpv reports. An empty list on a machine with a GPU is itself the finding.
    pub mpv_hwdec: Vec<String>,
}

/// Run a command and return trimmed stdout, or `None` if it is unavailable or fails.
///
/// Never propagates an error: a probe that cannot answer a question simply does not answer it.
fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Parse `mpv --vo=help` / `--hwdec=help` output into the bare identifiers.
fn parse_mpv_list(text: &str) -> Vec<String> {
    text.lines()
        .skip(1) // the header line
        .filter_map(|l| {
            let t = l.trim();
            let name = t.split_whitespace().next()?;
            (!name.is_empty() && name != "Available" && !name.starts_with('-'))
                .then(|| name.to_string())
        })
        .collect()
}

/// Parse a refresh rate out of a mode string like `3840x2160 @ 143.86Hz` or `2560x1440 60.00*+`.
fn parse_refresh(text: &str) -> Option<f64> {
    let cleaned: String =
        text.chars().map(|c| if c.is_ascii_digit() || c == '.' { c } else { ' ' }).collect();
    cleaned
        .split_whitespace()
        .filter_map(|t| t.parse::<f64>().ok())
        // Refresh rates live in a narrow band; resolutions and other numbers do not. Taking the last
        // match rather than the first is what makes `2560x1440 59.95*+ 74.97` yield the active mode.
        .rfind(|v| (20.0..=500.0).contains(v))
}

pub fn probe() -> Environment {
    let mut env = Environment {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ..Default::default()
    };

    match std::env::consts::OS {
        "linux" => probe_linux(&mut env),
        "macos" => probe_macos(&mut env),
        "windows" => probe_windows(&mut env),
        _ => {}
    }

    env.hybrid_graphics = env.gpus.len() > 1;
    probe_mpv(&mut env);
    env
}

fn probe_linux(env: &mut Environment) {
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        env.cpu_model = cpuinfo
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string());
        env.cpu_threads = Some(cpuinfo.lines().filter(|l| l.starts_with("processor")).count());
    }
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        env.memory_gb = meminfo
            .lines()
            .find(|l| l.starts_with("MemTotal"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<f64>().ok())
            .map(|kb| kb / 1024.0 / 1024.0);
    }
    if let Some(pci) = capture("sh", &["-c", "lspci | grep -Ei 'vga|3d|display'"]) {
        env.gpus = pci
            .lines()
            .map(|l| l.split_once(": ").map_or(l, |(_, v)| v).trim().to_string())
            .collect();
    }
    if let Some(modes) = capture("sh", &["-c", "xrandr --current 2>/dev/null | grep '\\*'"]) {
        env.display_refresh_hz = parse_refresh(&modes);
        env.display_resolution =
            modes.split_whitespace().next().map(std::string::ToString::to_string);
    }
    // `type` is "Battery"; `status` is Discharging when unplugged.
    env.on_battery = std::fs::read_to_string("/sys/class/power_supply/BAT0/status")
        .ok()
        .map(|s| s.trim() == "Discharging");
}

fn probe_macos(env: &mut Environment) {
    env.cpu_model = capture("sysctl", &["-n", "machdep.cpu.brand_string"]);
    env.cpu_threads = capture("sysctl", &["-n", "hw.ncpu"]).and_then(|v| v.parse().ok());
    env.memory_gb = capture("sysctl", &["-n", "hw.memsize"])
        .and_then(|v| v.parse::<f64>().ok())
        .map(|b| b / 1024.0 / 1024.0 / 1024.0);
    if let Some(disp) = capture("system_profiler", &["SPDisplaysDataType"]) {
        env.gpus = disp
            .lines()
            .filter(|l| l.trim_start().starts_with("Chipset Model:"))
            .filter_map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
            .collect();
        env.display_refresh_hz = parse_refresh(&disp);
        env.display_resolution = disp
            .lines()
            .find(|l| l.trim_start().starts_with("Resolution:"))
            .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));
    }
    env.on_battery = capture("pmset", &["-g", "batt"]).map(|s| s.contains("Battery Power"));
}

fn probe_windows(env: &mut Environment) {
    // PowerShell rather than WMIC: WMIC is deprecated and absent from recent Windows builds.
    let ps = |script: &str| capture("powershell", &["-NoProfile", "-Command", script]);
    env.cpu_model = ps("(Get-CimInstance Win32_Processor).Name").map(|s| first_line(&s));
    env.cpu_threads = ps("(Get-CimInstance Win32_Processor).NumberOfLogicalProcessors")
        .and_then(|s| first_line(&s).parse().ok());
    env.memory_gb = ps("(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory")
        .and_then(|s| first_line(&s).parse::<f64>().ok())
        .map(|b| b / 1024.0 / 1024.0 / 1024.0);
    if let Some(gpu) =
        ps("Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name")
    {
        env.gpus = gpu.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    }
    if let Some(mode) = ps("Get-CimInstance Win32_VideoController | \
         Select-Object -First 1 CurrentHorizontalResolution,CurrentVerticalResolution,\
         CurrentRefreshRate | Format-List | Out-String")
    {
        env.display_refresh_hz = parse_refresh(&mode);
    }
    // BatteryStatus 2 means running on AC.
    env.on_battery = ps("(Get-CimInstance Win32_Battery).BatteryStatus")
        .map(|s| first_line(&s) != "2" && !first_line(&s).is_empty());
}

fn probe_mpv(env: &mut Environment) {
    env.mpv_version = capture("mpv", &["--version"]).map(|s| first_line(&s));
    if let Some(vo) = capture("mpv", &["--vo=help"]) {
        env.mpv_vo = parse_mpv_list(&vo);
    }
    if let Some(hw) = capture("mpv", &["--hwdec=help"]) {
        env.mpv_hwdec = parse_mpv_list(&hw);
    }
}

/// Does this adapter name look like a discrete GPU?
///
/// A heuristic on marketing names, and openly so — there is no portable way to ask an adapter whether
/// it has its own memory. It errs toward saying "yes": the consequence of a false positive is a
/// missing warning, while a false negative nags the user on a correctly-configured machine, and a
/// warning that cries wolf is one that gets ignored when it is right.
fn looks_discrete(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // Integrated parts that would otherwise match a vendor keyword below. Checked first because
    // "AMD Radeon Graphics" is an iGPU while "AMD Radeon RX 7900" is not.
    const INTEGRATED: &[&str] =
        &["uhd graphics", "hd graphics", "iris", "vega 8", "vega 7", "radeon graphics", "apple m"];
    if INTEGRATED.iter().any(|m| n.contains(m)) {
        return false;
    }
    const DISCRETE: &[&str] = &[
        "geforce",
        "rtx",
        "gtx",
        "quadro",
        "tesla",
        "nvidia",
        "radeon rx",
        "radeon pro",
        "firepro",
        "arc a",
        "arc b",
        "intel arc",
    ];
    DISCRETE.iter().any(|m| n.contains(m))
}

/// Warnings about the environment that would make a result misleading if unrecorded.
///
/// Separate from the pass/fail verdict: these do not decide whether compositing works, they decide
/// whether the number can be believed.
pub fn environment_warnings(
    env: &Environment,
    check_refresh_match: bool,
    expect_discrete: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();

    if env.mpv_version.is_none() {
        warnings.push(
            "mpv was not found on PATH. Run the bootstrap script for this platform first — the \
             harness cannot measure anything without it."
                .into(),
        );
    }
    if !env.mpv_vo.is_empty() && !env.mpv_vo.iter().any(|v| v == "gpu-next") {
        warnings.push(format!(
            "This mpv has no `gpu-next` video output (found: {}). gpu-next is the libplacebo \
             renderer the product is built on, so a result from `gpu` alone measures a different \
             pipeline than the one that will ship.",
            env.mpv_vo.join(", ")
        ));
    }
    if env.mpv_version.is_some() && env.mpv_hwdec.iter().all(|h| h == "no" || h == "auto") {
        warnings.push(
            "mpv reports no specific hardware decoders. If the baseline stage struggles, software \
             decoding is the likely cause rather than anything to do with compositing."
                .into(),
        );
    }

    if check_refresh_match && let Some(hz) = env.display_refresh_hz {
        // 23.976 fps content on a 60 Hz panel judders regardless of compositing, and that judder is
        // easily misread as a compositing failure.
        let multiples = [23.976_f64, 24.0, 25.0, 30.0, 50.0, 60.0];
        // The tolerance is on the refresh rate itself, not on the ratio. A ratio tolerance loosens as
        // the multiple grows — 0.01 of a ratio is 0.24 Hz at 6:1, which accumulates a repeated frame
        // every few seconds — so a 144 Hz panel would be judged far more leniently than a 60 Hz one
        // for no reason. 0.1% of the refresh rate is roughly one slipped frame per thousand, which is
        // below what anyone perceives as judder.
        let clean = multiples.iter().any(|f| {
            let ratio = hz / f;
            ratio >= 1.0 && (hz - f * ratio.round()).abs() <= hz * 0.001
        });
        if !clean {
            warnings.push(format!(
                "Display is at {hz:.2} Hz, which is not an integer multiple of any common frame \
                 rate. Expect judder from the rate mismatch alone; it is not a compositing fault. \
                 Set the display to 60 Hz or to the clip's rate before drawing conclusions."
            ));
        }
    }

    if expect_discrete {
        // Counting adapters would be wrong here. A desktop with a discrete card and a CPU that has no
        // integrated graphics reports exactly one adapter, and that one *is* the discrete GPU — the
        // most common desktop there is. So look at what the adapter claims to be instead.
        if env.gpus.is_empty() {
            warnings.push(
                "This profile expects a discrete GPU, but no display adapter could be detected at \
                 all. That is a gap in the record rather than a finding — note the hardware by hand."
                    .into(),
            );
        } else if !env.gpus.iter().any(|g| looks_discrete(g)) {
            warnings.push(format!(
                "This profile expects a discrete GPU, but no adapter looks like one ({}). Either the \
                 laptop profile is the right one for this machine, or the naming is unfamiliar — \
                 check before treating the result as a desktop reference.",
                env.gpus.join(" / ")
            ));
        }
    }
    if env.hybrid_graphics {
        warnings.push(format!(
            "Hybrid graphics detected ({}). Which adapter actually drove the render is not visible \
             from here — confirm it before trusting the result, since a machine that renders on the \
             iGPU is measuring a different question.",
            env.gpus.join(" / ")
        ));
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_rates_are_parsed_out_of_real_mode_strings() {
        assert_eq!(parse_refresh("3840x2160 @ 143.86Hz"), Some(143.86));
        assert_eq!(parse_refresh("2560x1440     59.95*+  74.97"), Some(74.97));
        assert_eq!(parse_refresh("CurrentRefreshRate : 60"), Some(60.0));
        // A resolution alone carries no refresh rate, and must not be read as one.
        assert_eq!(parse_refresh("1920x1080"), None);
        assert_eq!(parse_refresh(""), None);
    }

    #[test]
    fn mpv_help_output_is_parsed_into_bare_identifiers() {
        let vo = "Available video outputs:\n  gpu-next\n  gpu\n  libmpv\n  null\n";
        let parsed = parse_mpv_list(vo);
        assert!(parsed.contains(&"gpu-next".to_string()), "{parsed:?}");
        assert!(parsed.contains(&"gpu".to_string()));
        assert!(!parsed.iter().any(|s| s.contains("Available")));
    }

    #[test]
    fn a_missing_gpu_next_is_warned_about_because_it_is_a_different_pipeline() {
        let env = Environment {
            mpv_version: Some("mpv 0.38.0".into()),
            mpv_vo: vec!["gpu".into(), "null".into()],
            mpv_hwdec: vec!["vaapi".into()],
            ..Default::default()
        };
        let w = environment_warnings(&env, false, false);
        assert!(w.iter().any(|m| m.contains("gpu-next")), "{w:?}");
    }

    #[test]
    fn a_missing_mpv_is_the_first_thing_reported() {
        let w = environment_warnings(&Environment::default(), false, false);
        assert!(w[0].contains("mpv was not found"), "{w:?}");
    }

    #[test]
    fn a_refresh_mismatch_is_flagged_as_not_a_compositing_fault() {
        // 165 Hz is a common gaming-panel rate and is a multiple of nothing: 6.88× film, 2.75× 60.
        // Film content judders on such a panel with no compositor involved at all.
        let env = Environment {
            mpv_version: Some("mpv 0.38.0".into()),
            mpv_vo: vec!["gpu-next".into()],
            mpv_hwdec: vec!["vulkan".into()],
            display_refresh_hz: Some(165.0),
            ..Default::default()
        };
        let w = environment_warnings(&env, true, false);
        let msg = w.iter().find(|m| m.contains("Hz")).expect("a refresh warning");
        assert!(msg.contains("not a compositing fault"), "{msg}");
    }

    #[test]
    fn clean_refresh_multiples_are_not_flagged() {
        // 143.86 is in the list deliberately: it is what a "144 Hz" panel usually reports, and it is
        // exactly 6× 23.976. Warning about it would train the user to ignore the warning.
        for hz in [60.0, 120.0, 144.0, 143.86, 48.0, 50.0, 24.0] {
            let env = Environment {
                mpv_version: Some("mpv".into()),
                mpv_vo: vec!["gpu-next".into()],
                mpv_hwdec: vec!["vulkan".into()],
                display_refresh_hz: Some(hz),
                ..Default::default()
            };
            let w = environment_warnings(&env, true, false);
            assert!(!w.iter().any(|m| m.contains("judder")), "{hz} Hz flagged: {w:?}");
        }
    }

    #[test]
    fn hybrid_graphics_are_always_reported_because_the_renderer_is_ambiguous() {
        let env = Environment {
            mpv_version: Some("mpv".into()),
            mpv_vo: vec!["gpu-next".into()],
            mpv_hwdec: vec!["vulkan".into()],
            gpus: vec!["Intel Iris Xe".into(), "NVIDIA RTX 4060 Laptop".into()],
            hybrid_graphics: true,
            ..Default::default()
        };
        let w = environment_warnings(&env, false, false);
        assert!(w.iter().any(|m| m.contains("Hybrid graphics")), "{w:?}");
    }

    #[test]
    fn a_desktop_profile_on_an_integrated_only_machine_warns_about_the_mismatch() {
        let env = Environment {
            mpv_version: Some("mpv".into()),
            mpv_vo: vec!["gpu-next".into()],
            mpv_hwdec: vec!["vulkan".into()],
            gpus: vec!["Intel UHD Graphics 770".into()],
            ..Default::default()
        };
        let w = environment_warnings(&env, false, true);
        assert!(w.iter().any(|m| m.contains("expects a discrete GPU")), "{w:?}");
    }

    #[test]
    fn a_desktop_with_one_discrete_card_is_not_warned_about() {
        // The most common desktop there is: a discrete card and a CPU with no integrated graphics,
        // so exactly one adapter is reported — and it is the right one. Counting adapters instead of
        // reading them would nag on every correctly-configured desktop, and a warning that cries wolf
        // is one that gets ignored when it is right.
        for gpu in ["NVIDIA GeForce RTX 4070", "AMD Radeon RX 7900 XTX", "Intel Arc A770"] {
            let env = Environment {
                mpv_version: Some("mpv".into()),
                mpv_vo: vec!["gpu-next".into()],
                mpv_hwdec: vec!["vulkan".into()],
                gpus: vec![gpu.into()],
                ..Default::default()
            };
            assert!(
                environment_warnings(&env, false, true).is_empty(),
                "{gpu} was flagged: {:?}",
                environment_warnings(&env, false, true)
            );
        }
    }

    #[test]
    fn integrated_names_that_contain_a_vendor_keyword_are_not_read_as_discrete() {
        // "AMD Radeon Graphics" is an iGPU; "AMD Radeon RX 7900" is not. Matching the vendor alone
        // would call both discrete.
        assert!(!looks_discrete("AMD Radeon Graphics"));
        assert!(!looks_discrete("Intel(R) Iris(R) Xe Graphics"));
        assert!(!looks_discrete("Apple M3 Pro"));
        assert!(looks_discrete("AMD Radeon RX 7900 XTX"));
        assert!(looks_discrete("NVIDIA GeForce RTX 4090"));
    }

    #[test]
    fn an_undetectable_gpu_is_reported_as_a_gap_not_as_a_finding() {
        let env = Environment {
            mpv_version: Some("mpv".into()),
            mpv_vo: vec!["gpu-next".into()],
            mpv_hwdec: vec!["vulkan".into()],
            ..Default::default()
        };
        let w = environment_warnings(&env, false, true);
        let msg = w.iter().find(|m| m.contains("discrete GPU")).expect("a warning");
        assert!(msg.contains("gap in the record"), "{msg}");
    }

    #[test]
    fn a_fully_healthy_environment_produces_no_warnings() {
        let env = Environment {
            mpv_version: Some("mpv 0.38.0".into()),
            mpv_vo: vec!["gpu-next".into(), "gpu".into()],
            mpv_hwdec: vec!["vulkan".into(), "vaapi".into()],
            display_refresh_hz: Some(60.0),
            gpus: vec!["NVIDIA RTX 4070".into()],
            ..Default::default()
        };
        assert!(environment_warnings(&env, true, false).is_empty());
    }

    #[test]
    fn probing_this_machine_does_not_panic_or_hang() {
        // The probe shells out to tools that may not exist. Every one of those paths must degrade to
        // `None` rather than failing the run.
        let env = probe();
        assert!(!env.os.is_empty());
        assert!(!env.arch.is_empty());
    }
}
