//! Rendering: a human-readable console report and a machine-readable JSON one.
//!
//! Both are produced from the same values. The console text is what the person at the machine reads
//! while the run is happening; the JSON is what gets attached to the spike's conclusion in
//! `docs/09-roadmap.md`, months after the machine it came from has been reinstalled.
//!
//! JSON is emitted by hand rather than with `serde` — the harness has zero dependencies on purpose,
//! so it builds on a machine with nothing installed but a Rust toolchain. The shapes here are fixed
//! and shallow, which is exactly the case where hand-emitting is safe. Every string still goes
//! through [`escape`]; a GPU model containing a quote or a backslash (Windows adapter names do
//! contain backslashes) would otherwise produce a file no parser can read.

use crate::pacing::{StageStats, Verdict};
use crate::probe::Environment;
use crate::profile::Profile;

/// Escape a string for a JSON string literal, per RFC 8259 §7.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below 0x20 must be escaped; \u form covers the ones without a short escape.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_string(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

/// A JSON value for an optional string: the string, or `null`.
fn json_opt_str(v: Option<&str>) -> String {
    v.map_or_else(|| "null".to_string(), json_string)
}

/// A JSON number, or `null`.
///
/// `NaN` and the infinities have no JSON representation, and emitting them anyway is the classic way
/// to produce a file that every parser rejects. They become `null` — an absent measurement, which is
/// what they mean here.
fn json_num(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.4}"),
        _ => "null".to_string(),
    }
}

fn json_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| json_string(s)).collect();
    format!("[{}]", inner.join(","))
}

/// Wrap prose to `width` columns, indenting continuation lines.
///
/// Findings are full sentences meant to be read at a terminal, and an unwrapped 300-character
/// explanation is one nobody reads.
fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.join(&format!("\n{indent}"))
}

fn or_unknown(v: Option<&String>) -> &str {
    v.map_or("unknown", String::as_str)
}

/// An environment warning, wrapped to the same width as a verdict finding.
///
/// These are the lines that decide whether a number can be believed at all, so they must be as
/// readable as the verdict itself — an unwrapped 200-character warning scrolls past unread, and the
/// one it hides is usually the one that mattered.
pub fn render_warning(text: &str) -> String {
    format!("  ! {}", wrap(text, 88, "    "))
}

/// The environment block: what this machine is, in the terms that make a result interpretable.
pub fn render_environment(env: &Environment) -> String {
    let mut s = String::from("machine\n");
    s.push_str(&format!("  os           {} / {}\n", env.os, env.arch));
    s.push_str(&format!(
        "  cpu          {}{}\n",
        or_unknown(env.cpu_model.as_ref()),
        env.cpu_threads.map_or(String::new(), |t| format!(" ({t} threads)"))
    ));
    if let Some(gb) = env.memory_gb {
        s.push_str(&format!("  memory       {gb:.1} GiB\n"));
    }
    s.push_str(&format!(
        "  gpu          {}{}\n",
        if env.gpus.is_empty() { "none detected".to_string() } else { env.gpus.join(" / ") },
        if env.hybrid_graphics { "   [hybrid]" } else { "" }
    ));
    s.push_str(&format!(
        "  display      {}{}\n",
        or_unknown(env.display_resolution.as_ref()),
        env.display_refresh_hz.map_or(String::new(), |hz| format!(" @ {hz:.2} Hz"))
    ));
    s.push_str(&format!(
        "  power        {}\n",
        match env.on_battery {
            Some(true) => "battery",
            Some(false) => "mains",
            None => "unknown",
        }
    ));
    s.push_str(&format!("  mpv          {}\n", or_unknown(env.mpv_version.as_ref())));
    s.push_str(&format!(
        "  mpv vo       {}\n",
        if env.mpv_vo.is_empty() { "unknown".to_string() } else { env.mpv_vo.join(", ") }
    ));
    s.push_str(&format!(
        "  mpv hwdec    {}",
        if env.mpv_hwdec.is_empty() { "unknown".to_string() } else { env.mpv_hwdec.join(", ") }
    ));
    s
}

/// One stage's numbers.
pub fn render_stage(st: &StageStats) -> String {
    let mut s = format!("  {} ({:.0} s, {} samples)\n", st.label, st.duration_s, st.sample_count);
    s.push_str(&format!(
        "    late presents  {} ({:.2}/min)\n",
        st.delayed_frames, st.delayed_per_min
    ));
    s.push_str(&format!("    decoder drops  {}\n", st.dropped_frames));
    s.push_str(&format!("    mean fps       {:.3}\n", st.mean_fps));
    s.push_str(&format!("    max avsync     {:.1} ms", st.max_avsync_ms));
    if let Some(p99) = st.p99_render_ms {
        s.push_str(&format!("\n    p99 render     {p99:.2} ms"));
    }
    if let Some(cpu) = st.mean_cpu_frac {
        s.push_str(&format!("\n    mean cpu       {cpu:.2} cores"));
    }
    s
}

/// The verdict block, with every finding spelled out.
///
/// An `INCONCLUSIVE` result gets an explicit line saying it is not a failure of the architecture,
/// because the whole reason the harness is a paired comparison is that those two are easy to confuse
/// and have opposite consequences for the desktop shell.
pub fn render_verdict(v: &Verdict) -> String {
    let mut s = format!("VERDICT: {}\n", v.outcome);
    s.push_str(&format!(
        "  added late presents  {:+.2}/min\n  frame rate ratio     {:.1}%\n",
        v.added_delayed_per_min,
        v.fps_ratio * 100.0
    ));
    if let Some(cpu) = v.added_cpu_frac {
        s.push_str(&format!("  added cpu            {cpu:+.2} cores\n"));
    }
    s.push('\n');
    for f in &v.findings {
        s.push_str(&format!("  - {}\n", wrap(f, 88, "    ")));
    }
    match v.outcome {
        crate::pacing::Outcome::Inconclusive => s.push_str(
            "\n  This is not a failure of the architecture — it is a failure to measure it. Fix the\n  \
             baseline (lower-bitrate clip, or hardware decoding actually engaged) and run again.\n",
        ),
        crate::pacing::Outcome::Fail => s.push_str(
            "\n  Per ADR-0001 this is the trigger to reconsider Tauri for the desktop shell. Before\n  \
             acting on it, confirm the environment warnings above are clear — a refresh-rate\n  \
             mismatch or a render on the wrong GPU produces the same numbers.\n",
        ),
        _ => {}
    }
    s
}

/// The full machine-readable record.
#[allow(clippy::too_many_arguments)] // every argument is a distinct part of the record
pub fn render_json(
    env: &Environment,
    profile: &Profile,
    baseline: &StageStats,
    composited: Option<&StageStats>,
    verdict: Option<&Verdict>,
    warnings: &[String],
) -> String {
    let stage_json = |st: &StageStats| {
        format!(
            "{{\"label\":{},\"duration_s\":{},\"delayed_frames\":{},\"dropped_frames\":{},\
             \"delayed_per_min\":{},\"mean_fps\":{},\"max_avsync_ms\":{},\"p99_render_ms\":{},\
             \"mean_cpu_frac\":{},\"sample_count\":{}}}",
            json_string(&st.label),
            json_num(Some(st.duration_s)),
            st.delayed_frames,
            st.dropped_frames,
            json_num(Some(st.delayed_per_min)),
            json_num(Some(st.mean_fps)),
            json_num(Some(st.max_avsync_ms)),
            json_num(st.p99_render_ms),
            json_num(st.mean_cpu_frac),
            st.sample_count
        )
    };

    let t = &profile.thresholds;
    format!(
        "{{\n\
         \x20 \"spike\": \"S1-compositing\",\n\
         \x20 \"schema\": 1,\n\
         \x20 \"environment\": {{\"os\":{},\"arch\":{},\"cpu_model\":{},\"cpu_threads\":{},\
         \"memory_gb\":{},\"gpus\":{},\"hybrid_graphics\":{},\"display_resolution\":{},\
         \"display_refresh_hz\":{},\"on_battery\":{},\"mpv_version\":{},\"mpv_vo\":{},\
         \"mpv_hwdec\":{}}},\n\
         \x20 \"profile\": {{\"name\":{},\"description\":{},\"run_seconds\":{},\"warmup_ms\":{},\
         \"test_on_battery\":{},\"thermal_soak_minutes\":{},\"thresholds\":{{\
         \"baseline_max_delayed_per_min\":{},\"max_added_delayed_per_min\":{},\"min_fps_ratio\":{},\
         \"max_avsync_ms\":{},\"max_added_cpu_frac\":{},\"max_osd_latency_ms\":{}}}}},\n\
         \x20 \"warnings\": {},\n\
         \x20 \"baseline\": {},\n\
         \x20 \"composited\": {},\n\
         \x20 \"verdict\": {}\n\
         }}\n",
        json_string(&env.os),
        json_string(&env.arch),
        json_opt_str(env.cpu_model.as_deref()),
        env.cpu_threads.map_or("null".into(), |v| v.to_string()),
        json_num(env.memory_gb),
        json_list(&env.gpus),
        env.hybrid_graphics,
        json_opt_str(env.display_resolution.as_deref()),
        json_num(env.display_refresh_hz),
        env.on_battery.map_or("null".into(), |v| v.to_string()),
        json_opt_str(env.mpv_version.as_deref()),
        json_list(&env.mpv_vo),
        json_list(&env.mpv_hwdec),
        json_string(&profile.name),
        json_string(&profile.description),
        profile.run_seconds,
        profile.warmup_ms,
        profile.test_on_battery,
        profile.thermal_soak_minutes,
        json_num(Some(t.baseline_max_delayed_per_min)),
        json_num(Some(t.max_added_delayed_per_min)),
        json_num(Some(t.min_fps_ratio)),
        json_num(Some(t.max_avsync_ms)),
        json_num(t.max_added_cpu_frac),
        json_num(t.max_osd_latency_ms),
        json_list(warnings),
        stage_json(baseline),
        composited.map_or("null".to_string(), stage_json),
        verdict.map_or("null".to_string(), |v| format!(
            "{{\"outcome\":{},\"added_delayed_per_min\":{},\"fps_ratio\":{},\"added_cpu_frac\":{},\
             \"findings\":{}}}",
            json_string(&v.outcome.to_string()),
            json_num(Some(v.added_delayed_per_min)),
            json_num(Some(v.fps_ratio)),
            json_num(v.added_cpu_frac),
            json_list(&v.findings)
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacing::{Outcome, Sample, Thresholds, judge, summarize};

    fn profile() -> Profile {
        Profile::parse(include_str!("../profiles/desktop.toml")).unwrap()
    }

    fn stage(label: &str, delayed: u64, fps: f64) -> StageStats {
        let samples: Vec<Sample> =
            (0..=60).map(|i| Sample::new(i * 1000, 0, delayed * i / 60, fps)).collect();
        summarize(label, &samples, 0)
    }

    /// A structural check that stands in for a JSON parser, which this crate deliberately does not
    /// have. Unbalanced braces or brackets are exactly the failure hand-emitted JSON produces.
    fn is_balanced(json: &str) -> bool {
        let (mut braces, mut brackets, mut in_string, mut escaped) = (0i32, 0i32, false, false);
        for c in json.chars() {
            if in_string {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => braces += 1,
                '}' => braces -= 1,
                '[' => brackets += 1,
                ']' => brackets -= 1,
                _ => {}
            }
            if braces < 0 || brackets < 0 {
                return false;
            }
        }
        braces == 0 && brackets == 0 && !in_string
    }

    #[test]
    fn json_strings_are_escaped() {
        // Windows adapter names contain backslashes and device paths; an unescaped one produces a
        // file no parser will read.
        assert_eq!(escape(r#"NVIDIA "RTX" \ 4070"#), r#"NVIDIA \"RTX\" \\ 4070"#);
        assert_eq!(escape("line\nbreak\ttab"), "line\\nbreak\\ttab");
        assert_eq!(escape("bell\u{7}"), "bell\\u0007");
    }

    #[test]
    fn non_finite_numbers_become_null_rather_than_invalid_json() {
        // A zero-length stage divides by zero somewhere upstream; NaN in the output would make the
        // whole report unparseable instead of one field unknown.
        assert_eq!(json_num(Some(f64::NAN)), "null");
        assert_eq!(json_num(Some(f64::INFINITY)), "null");
        assert_eq!(json_num(None), "null");
        assert_eq!(json_num(Some(1.5)), "1.5000");
    }

    #[test]
    fn a_complete_report_is_structurally_valid_json() {
        let env = Environment {
            os: "windows".into(),
            arch: "x86_64".into(),
            cpu_model: Some("AMD Ryzen 9 7950X".into()),
            gpus: vec![r#"NVIDIA "RTX" 4090"#.into()],
            mpv_version: Some("mpv 0.38.0".into()),
            ..Default::default()
        };
        let base = stage("baseline", 1, 23.976);
        let comp = stage("composited", 2, 23.976);
        let v = judge(&base, &comp, &profile().thresholds, Some(30.0));
        let json =
            render_json(&env, &profile(), &base, Some(&comp), Some(&v), &["a \"note\"".into()]);
        assert!(is_balanced(&json), "{json}");
        assert!(json.contains("\"spike\": \"S1-compositing\""));
        assert!(json.contains(r#"\"RTX\""#), "the GPU name must be escaped: {json}");
    }

    #[test]
    fn a_skipped_composited_stage_is_null_rather_than_a_fabricated_zero() {
        // Reporting zeros for a stage that never ran would read as a perfect result.
        let json = render_json(
            &Environment::default(),
            &profile(),
            &stage("baseline", 1, 24.0),
            None,
            None,
            &[],
        );
        assert!(is_balanced(&json), "{json}");
        assert!(json.contains("\"composited\": null"), "{json}");
        assert!(json.contains("\"verdict\": null"), "{json}");
    }

    #[test]
    fn an_inconclusive_verdict_says_it_is_not_an_architecture_failure() {
        // The single most important line in the whole report: an unusable baseline must never be
        // read as evidence against Tauri.
        let base = stage("baseline", 600, 14.0);
        let comp = stage("composited", 900, 12.0);
        let v = judge(&base, &comp, &profile().thresholds, Some(30.0));
        assert_eq!(v.outcome, Outcome::Inconclusive);
        let text = render_verdict(&v);
        assert!(text.contains("INCONCLUSIVE"), "{text}");
        assert!(text.contains("not a failure of the architecture"), "{text}");
    }

    #[test]
    fn a_failure_points_at_the_adr_and_at_the_confounders() {
        let t = Thresholds { max_osd_latency_ms: Some(100.0), ..profile().thresholds };
        let base = stage("baseline", 0, 23.976);
        let comp = stage("composited", 0, 11.0);
        let v = judge(&base, &comp, &t, Some(30.0));
        assert_eq!(v.outcome, Outcome::Fail);
        let text = render_verdict(&v);
        assert!(text.contains("ADR-0001"), "{text}");
        assert!(text.contains("refresh-rate"), "a confounder check must precede acting: {text}");
    }

    #[test]
    fn stage_output_names_the_numbers_the_verdict_uses() {
        let text = render_stage(&stage("baseline", 12, 23.976));
        for needed in ["late presents", "mean fps", "avsync"] {
            assert!(text.contains(needed), "{needed} missing from:\n{text}");
        }
    }

    #[test]
    fn an_environment_with_nothing_detected_still_renders() {
        // The probe degrades to `None` everywhere; the report must not panic or print an empty block.
        let text = render_environment(&Environment::default());
        assert!(text.contains("unknown"), "{text}");
        assert!(text.contains("none detected"), "{text}");
    }

    #[test]
    fn warnings_wrap_to_the_same_width_as_findings() {
        // An unwrapped warning scrolls past unread, and the one it hides is usually the one that
        // decided whether the number could be believed.
        let env = Environment::default();
        let w = crate::probe::environment_warnings(&env, false, false);
        let text = render_warning(&w[0]);
        assert!(text.starts_with("  ! "), "{text}");
        assert!(text.lines().count() > 1, "a long warning must wrap:\n{text}");
        assert!(text.lines().all(|l| l.chars().count() <= 92), "{text}");
    }

    #[test]
    fn long_findings_are_wrapped_for_a_terminal() {
        let long = "word ".repeat(60);
        let wrapped = wrap(&long, 40, "    ");
        assert!(wrapped.lines().count() > 1);
        assert!(wrapped.lines().all(|l| l.chars().count() <= 44), "{wrapped}");
    }

    #[test]
    fn wrapping_preserves_every_word() {
        let text = "Compositing added 4.2 late presents/min (limit 3.0).";
        let wrapped = wrap(text, 20, "  ");
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }
}
