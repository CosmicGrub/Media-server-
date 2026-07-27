//! Machine profiles.
//!
//! A desktop and a laptop are judged differently, and pretending otherwise produces useless results.
//! A laptop that adds two late presents per minute while composited is fine; the same laptop dropping
//! to half rate the moment it leaves mains power is a finding the desktop run can never surface.
//!
//! Profiles are flat `key = value` text, parsed here rather than pulled in with a TOML crate — the
//! harness is meant to be built on a machine that may have nothing installed, so it has zero
//! dependencies on purpose.

use std::collections::BTreeMap;

use crate::pacing::Thresholds;

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    /// Human note shown in the report, explaining what this profile assumes.
    pub description: String,
    pub thresholds: Thresholds,
    /// Seconds of playback per stage, after warmup.
    pub run_seconds: u64,
    /// Milliseconds discarded at the start of each stage.
    pub warmup_ms: u64,
    /// Poll interval for mpv's counters.
    pub sample_interval_ms: u64,
    /// Repeat the whole pair on battery as well as mains. Laptop-only, and the single most valuable
    /// laptop-specific measurement: hybrid-graphics machines routinely park the discrete GPU on
    /// battery, which halves the frame rate without dropping a single "late" frame.
    pub test_on_battery: bool,
    /// Run a sustained-load pass to catch thermal throttling. A laptop that passes for ninety seconds
    /// and fails at ten minutes has not passed.
    pub thermal_soak_minutes: u64,
    /// Warn when the display's refresh rate is not an integer multiple of the clip's frame rate.
    /// Judder from a rate mismatch is easily mistaken for a compositing failure.
    pub check_refresh_match: bool,
    /// Expect the render to be driven by a discrete GPU, and warn if it is not.
    pub expect_discrete_gpu: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "profile: {}", self.0)
    }
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

fn need<T: std::str::FromStr>(kv: &BTreeMap<String, String>, key: &str) -> Result<T, ParseError> {
    let raw = kv.get(key).ok_or_else(|| ParseError(format!("missing required key `{key}`")))?;
    raw.parse::<T>().map_err(|_| ParseError(format!("key `{key}` has unparseable value {raw:?}")))
}

fn opt<T: std::str::FromStr>(kv: &BTreeMap<String, String>, key: &str) -> Option<T> {
    kv.get(key).filter(|v| !v.is_empty() && *v != "none").and_then(|v| v.parse().ok())
}

fn flag(kv: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    kv.get(key).map_or(default, |v| matches!(v.as_str(), "true" | "yes" | "1" | "on"))
}

impl Profile {
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let kv = parse_kv(text);
        let thresholds = Thresholds {
            baseline_max_delayed_per_min: need(&kv, "baseline_max_delayed_per_min")?,
            max_added_delayed_per_min: need(&kv, "max_added_delayed_per_min")?,
            min_fps_ratio: need(&kv, "min_fps_ratio")?,
            max_avsync_ms: need(&kv, "max_avsync_ms")?,
            max_added_cpu_frac: opt(&kv, "max_added_cpu_frac"),
            max_osd_latency_ms: opt(&kv, "max_osd_latency_ms"),
        };
        if !(0.0..=1.0).contains(&thresholds.min_fps_ratio) {
            return Err(ParseError("min_fps_ratio must be between 0 and 1".into()));
        }
        Ok(Self {
            name: kv.get("name").cloned().unwrap_or_else(|| "unnamed".into()),
            description: kv.get("description").cloned().unwrap_or_default(),
            thresholds,
            run_seconds: need(&kv, "run_seconds")?,
            warmup_ms: opt(&kv, "warmup_ms").unwrap_or(5000),
            sample_interval_ms: opt(&kv, "sample_interval_ms").unwrap_or(500),
            test_on_battery: flag(&kv, "test_on_battery", false),
            thermal_soak_minutes: opt(&kv, "thermal_soak_minutes").unwrap_or(0),
            check_refresh_match: flag(&kv, "check_refresh_match", true),
            expect_discrete_gpu: flag(&kv, "expect_discrete_gpu", false),
        })
    }

    pub fn load(path: &std::path::Path) -> Result<Self, ParseError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ParseError(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: &str = include_str!("../profiles/desktop.toml");
    const LAPTOP: &str = include_str!("../profiles/laptop.toml");

    #[test]
    fn the_shipped_profiles_parse() {
        // These are the files the user actually runs; a typo in one is a broken spike.
        for (name, text) in [("desktop", DESKTOP), ("laptop", LAPTOP)] {
            Profile::parse(text).unwrap_or_else(|e| panic!("{name} profile: {e}"));
        }
    }

    #[test]
    fn the_laptop_profile_adds_the_checks_a_desktop_cannot_surface() {
        let desktop = Profile::parse(DESKTOP).unwrap();
        let laptop = Profile::parse(LAPTOP).unwrap();

        assert!(laptop.test_on_battery, "the single most valuable laptop-specific measurement");
        assert!(
            !desktop.test_on_battery,
            "a mains-powered desktop has no battery state to compare"
        );
        assert!(laptop.thermal_soak_minutes > 0, "a laptop that passes for 90 s has not passed");
        assert!(
            desktop.expect_discrete_gpu,
            "a desktop spike assumes the dGPU is driving the render"
        );
    }

    #[test]
    fn the_laptop_profile_is_more_forgiving_on_cost_and_stricter_on_sustained_rate() {
        let desktop = Profile::parse(DESKTOP).unwrap();
        let laptop = Profile::parse(LAPTOP).unwrap();
        // An iGPU legitimately has less headroom, so the frame budget is looser...
        assert!(
            laptop.thresholds.max_added_delayed_per_min
                >= desktop.thresholds.max_added_delayed_per_min
        );
        // ...but CPU cost matters more, because it is battery and fan noise.
        assert!(
            laptop.thresholds.max_added_cpu_frac <= desktop.thresholds.max_added_cpu_frac,
            "laptop CPU budget should be tighter: {:?} vs {:?}",
            laptop.thresholds.max_added_cpu_frac,
            desktop.thresholds.max_added_cpu_frac
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let p = Profile::parse(
            "# a comment\n\nname = test  # trailing comment\n\
             baseline_max_delayed_per_min = 6\nmax_added_delayed_per_min = 3\n\
             min_fps_ratio = 0.98\nmax_avsync_ms = 40\nrun_seconds = 90\n",
        )
        .unwrap();
        assert_eq!(p.name, "test");
        assert_eq!(p.run_seconds, 90);
    }

    #[test]
    fn a_missing_required_key_is_an_error_naming_the_key() {
        // The user is editing this file by hand on their own machine; the error has to be actionable.
        let err = Profile::parse("name = x\nrun_seconds = 90\n").unwrap_err();
        assert!(err.0.contains("baseline_max_delayed_per_min"), "{err}");
    }

    #[test]
    fn an_unparseable_value_names_the_key_and_the_value() {
        let err = Profile::parse(
            "baseline_max_delayed_per_min = lots\nmax_added_delayed_per_min = 3\n\
             min_fps_ratio = 0.98\nmax_avsync_ms = 40\nrun_seconds = 90\n",
        )
        .unwrap_err();
        assert!(err.0.contains("baseline_max_delayed_per_min"), "{err}");
        assert!(err.0.contains("lots"), "{err}");
    }

    #[test]
    fn an_out_of_range_fps_ratio_is_rejected() {
        // A ratio above 1 would demand the composited stage beat the baseline, which never passes.
        let err = Profile::parse(
            "baseline_max_delayed_per_min = 6\nmax_added_delayed_per_min = 3\n\
             min_fps_ratio = 1.5\nmax_avsync_ms = 40\nrun_seconds = 90\n",
        )
        .unwrap_err();
        assert!(err.0.contains("min_fps_ratio"), "{err}");
    }

    #[test]
    fn optional_thresholds_may_be_omitted_or_set_to_none() {
        let base = "baseline_max_delayed_per_min = 6\nmax_added_delayed_per_min = 3\n\
                    min_fps_ratio = 0.98\nmax_avsync_ms = 40\nrun_seconds = 90\n";
        assert_eq!(Profile::parse(base).unwrap().thresholds.max_added_cpu_frac, None);
        let explicit = format!("{base}max_added_cpu_frac = none\n");
        assert_eq!(Profile::parse(&explicit).unwrap().thresholds.max_added_cpu_frac, None);
    }
}
