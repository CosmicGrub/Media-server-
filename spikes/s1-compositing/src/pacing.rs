//! Frame-pacing statistics and the S1 verdict.
//!
//! This is the part of the harness that decides whether the spike passed, so it is pure logic and
//! fully tested — the parts that touch a GPU cannot be tested in CI, but the reasoning about their
//! output can be.
//!
//! **The measurement is a paired comparison, not an absolute.** Running only the composited stage
//! conflates two different findings: "compositing a WebView over video costs frames" and "this
//! machine cannot decode 4K HDR at all". Those have opposite consequences — the first invalidates
//! Tauri, the second invalidates nothing about the architecture — so the harness runs mpv bare as a
//! baseline and reports the *delta*.

use std::fmt;

/// One sample of mpv's counters, polled over the IPC socket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Monotonic milliseconds since the stage started.
    pub at_ms: u64,
    /// `frame-drop-count`: frames the decoder dropped, cumulative.
    pub decoder_drops: u64,
    /// `vo-delayed-frame-count`: frames the video output presented late, cumulative. This is the
    /// number that matters for compositing — a late present is what a viewer sees as a stutter.
    pub vo_delayed: u64,
    /// `avsync`: audio-video desync in seconds, signed.
    pub avsync_s: f64,
    /// `estimated-vf-fps`: measured output frame rate.
    pub fps: f64,
    /// Milliseconds the renderer spent on the last frame, from `vo-passes`. `None` when the build
    /// does not expose it.
    pub render_ms: Option<f64>,
    /// Process CPU utilisation as a fraction of one core, if the platform probe could read it.
    pub cpu_frac: Option<f64>,
}

impl Sample {
    pub fn new(at_ms: u64, decoder_drops: u64, vo_delayed: u64, fps: f64) -> Self {
        Self {
            at_ms,
            decoder_drops,
            vo_delayed,
            avsync_s: 0.0,
            fps,
            render_ms: None,
            cpu_frac: None,
        }
    }
}

/// Statistics for one stage (bare mpv, or mpv inside the shell).
#[derive(Debug, Clone, PartialEq)]
pub struct StageStats {
    pub label: String,
    pub duration_s: f64,
    /// Total frames the video output presented late over the run.
    pub delayed_frames: u64,
    pub dropped_frames: u64,
    /// Late presents per minute — the comparable figure, since stages may differ slightly in length.
    pub delayed_per_min: f64,
    pub mean_fps: f64,
    /// Worst A/V desync seen, absolute.
    pub max_avsync_ms: f64,
    pub p99_render_ms: Option<f64>,
    pub mean_cpu_frac: Option<f64>,
    pub sample_count: usize,
}

/// Counters are cumulative, so a decrease means mpv restarted them — a new file, or a seek that reset
/// the video output. Treated as a fresh baseline rather than a negative delta.
fn monotonic_delta(first: u64, last: u64) -> u64 {
    last.saturating_sub(first)
}

/// Summarise a stage's samples.
///
/// Discards the first `warmup_ms` of samples: the first seconds of playback include decoder
/// initialisation, shader compilation, and window-manager settling, none of which represent steady
/// state and all of which would dominate a short run.
pub fn summarize(label: &str, samples: &[Sample], warmup_ms: u64) -> StageStats {
    let steady: Vec<&Sample> = samples.iter().filter(|s| s.at_ms >= warmup_ms).collect();

    let empty = StageStats {
        label: label.to_string(),
        duration_s: 0.0,
        delayed_frames: 0,
        dropped_frames: 0,
        delayed_per_min: 0.0,
        mean_fps: 0.0,
        max_avsync_ms: 0.0,
        p99_render_ms: None,
        mean_cpu_frac: None,
        sample_count: 0,
    };
    let (Some(first), Some(last)) = (steady.first(), steady.last()) else {
        return empty;
    };
    let duration_s = (last.at_ms.saturating_sub(first.at_ms)) as f64 / 1000.0;
    if duration_s <= 0.0 {
        return StageStats { sample_count: steady.len(), ..empty };
    }

    let delayed = monotonic_delta(first.vo_delayed, last.vo_delayed);
    let dropped = monotonic_delta(first.decoder_drops, last.decoder_drops);

    let fps: Vec<f64> =
        steady.iter().map(|s| s.fps).filter(|f| f.is_finite() && *f > 0.0).collect();
    let mean_fps = if fps.is_empty() { 0.0 } else { fps.iter().sum::<f64>() / fps.len() as f64 };

    let max_avsync_ms = steady
        .iter()
        .map(|s| s.avsync_s.abs() * 1000.0)
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max);

    let mut renders: Vec<f64> =
        steady.iter().filter_map(|s| s.render_ms).filter(|v| v.is_finite()).collect();
    let p99_render_ms = if renders.is_empty() {
        None
    } else {
        renders.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(renders[((renders.len() as f64 * 0.99) as usize).min(renders.len() - 1)])
    };

    let cpus: Vec<f64> =
        steady.iter().filter_map(|s| s.cpu_frac).filter(|v| v.is_finite()).collect();
    let mean_cpu_frac = (!cpus.is_empty()).then(|| cpus.iter().sum::<f64>() / cpus.len() as f64);

    StageStats {
        label: label.to_string(),
        duration_s,
        delayed_frames: delayed,
        dropped_frames: dropped,
        delayed_per_min: delayed as f64 * 60.0 / duration_s,
        mean_fps,
        max_avsync_ms,
        p99_render_ms,
        mean_cpu_frac,
        sample_count: steady.len(),
    }
}

/// Pass/fail thresholds. Loaded from a profile so a laptop and a desktop can be judged differently
/// without changing the code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Late presents per minute the *baseline* may have before the machine is considered unable to
    /// play the clip at all. Above this, the spike says nothing about compositing.
    pub baseline_max_delayed_per_min: f64,
    /// Extra late presents per minute compositing may add. This is the actual S1 question.
    pub max_added_delayed_per_min: f64,
    /// Fraction of the baseline frame rate the composited stage must sustain.
    pub min_fps_ratio: f64,
    /// Worst permitted A/V desync in the composited stage.
    pub max_avsync_ms: f64,
    /// Extra CPU, in fractions of one core, compositing may cost.
    pub max_added_cpu_frac: Option<f64>,
    /// Milliseconds an OSD interaction may take to appear on screen. A responsive overlay is half the
    /// point of using a WebView.
    pub max_osd_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Compositing is viable on this machine.
    Pass,
    /// Viable, but with a caveat worth recording.
    PassWithNotes,
    /// Compositing costs too much here.
    Fail,
    /// The baseline itself could not sustain playback, so the composited result is uninterpretable.
    /// **Not a failure of the architecture** — it is a failure to measure it.
    Inconclusive,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "PASS",
            Self::PassWithNotes => "PASS (with notes)",
            Self::Fail => "FAIL",
            Self::Inconclusive => "INCONCLUSIVE",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub outcome: Outcome,
    /// One line per finding, in the order they were checked.
    pub findings: Vec<String>,
    pub added_delayed_per_min: f64,
    pub fps_ratio: f64,
    pub added_cpu_frac: Option<f64>,
}

/// Judge a paired baseline/composited run.
///
/// `osd_latency_ms` is measured separately by the shell — it has no meaning for the bare-mpv stage.
pub fn judge(
    baseline: &StageStats,
    composited: &StageStats,
    thresholds: &Thresholds,
    osd_latency_ms: Option<f64>,
) -> Verdict {
    let mut findings = Vec::new();
    let added = composited.delayed_per_min - baseline.delayed_per_min;
    let fps_ratio =
        if baseline.mean_fps > 0.0 { composited.mean_fps / baseline.mean_fps } else { 0.0 };
    let added_cpu = match (baseline.mean_cpu_frac, composited.mean_cpu_frac) {
        (Some(b), Some(c)) => Some(c - b),
        _ => None,
    };

    let base = Verdict {
        outcome: Outcome::Pass,
        findings: Vec::new(),
        added_delayed_per_min: added,
        fps_ratio,
        added_cpu_frac: added_cpu,
    };

    // Refuse to draw a conclusion from an unusable baseline. A machine that cannot play the clip bare
    // tells us nothing about whether compositing is expensive, and reporting that as a FAIL would
    // wrongly indict the architecture.
    if baseline.sample_count == 0 || composited.sample_count == 0 {
        findings.push("One or both stages produced no steady-state samples.".into());
        return Verdict { outcome: Outcome::Inconclusive, findings, ..base };
    }
    if baseline.delayed_per_min > thresholds.baseline_max_delayed_per_min {
        findings.push(format!(
            "Baseline mpv already dropped {:.1} frames/min (limit {:.1}). This machine cannot play \
             the test clip even without a UI, so the compositing result is uninterpretable. Try a \
             lower-bitrate clip, or check hardware decoding is actually engaged.",
            baseline.delayed_per_min, thresholds.baseline_max_delayed_per_min
        ));
        return Verdict { outcome: Outcome::Inconclusive, findings, ..base };
    }

    let mut failed = false;
    let mut noted = false;

    if added > thresholds.max_added_delayed_per_min {
        failed = true;
        findings.push(format!(
            "Compositing added {added:.1} late presents/min (limit {:.1}). Baseline {:.1}, \
             composited {:.1}.",
            thresholds.max_added_delayed_per_min,
            baseline.delayed_per_min,
            composited.delayed_per_min
        ));
    } else {
        findings.push(format!(
            "Compositing added {added:.1} late presents/min, within the {:.1} budget.",
            thresholds.max_added_delayed_per_min
        ));
    }

    if fps_ratio < thresholds.min_fps_ratio {
        failed = true;
        findings.push(format!(
            "Composited frame rate is {:.1}% of baseline ({:.2} vs {:.2} fps), below the {:.0}% floor.",
            fps_ratio * 100.0,
            composited.mean_fps,
            baseline.mean_fps,
            thresholds.min_fps_ratio * 100.0
        ));
    }

    if composited.max_avsync_ms > thresholds.max_avsync_ms {
        failed = true;
        findings.push(format!(
            "A/V desync reached {:.0} ms (limit {:.0} ms).",
            composited.max_avsync_ms, thresholds.max_avsync_ms
        ));
    }

    if let (Some(limit), Some(actual)) = (thresholds.max_added_cpu_frac, added_cpu) {
        if actual > limit {
            noted = true;
            findings.push(format!(
                "Compositing cost {:.2} extra CPU cores (soft limit {limit:.2}). Not fatal, but it \
                 is battery and thermal headroom.",
                actual
            ));
        }
    }

    if let (Some(limit), Some(actual)) = (thresholds.max_osd_latency_ms, osd_latency_ms) {
        if actual > limit {
            failed = true;
            findings.push(format!(
                "OSD interactions took {actual:.0} ms to appear (limit {limit:.0} ms). A responsive \
                 overlay is half the reason to use a WebView at all."
            ));
        } else {
            findings.push(format!("OSD responded in {actual:.0} ms."));
        }
    } else if thresholds.max_osd_latency_ms.is_some() {
        noted = true;
        findings.push(
            "OSD latency was not measured; the shell must report it for a complete result.".into(),
        );
    }

    let outcome = if failed {
        Outcome::Fail
    } else if noted {
        Outcome::PassWithNotes
    } else {
        Outcome::Pass
    };
    Verdict { outcome, findings, ..base }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> Thresholds {
        Thresholds {
            baseline_max_delayed_per_min: 6.0,
            max_added_delayed_per_min: 3.0,
            min_fps_ratio: 0.98,
            max_avsync_ms: 40.0,
            max_added_cpu_frac: Some(0.5),
            max_osd_latency_ms: Some(100.0),
        }
    }

    /// A stage with `delayed` late presents accumulating linearly over `secs`.
    fn stage(label: &str, secs: u64, delayed: u64, fps: f64) -> StageStats {
        let samples: Vec<Sample> = (0..=secs)
            .map(|i| {
                let d = delayed * i / secs.max(1);
                Sample { avsync_s: 0.001, ..Sample::new(i * 1000, 0, d, fps) }
            })
            .collect();
        summarize(label, &samples, 0)
    }

    #[test]
    fn warmup_samples_are_excluded() {
        // Decoder init and shader compilation dominate the first seconds and are not steady state.
        let samples = vec![
            Sample::new(0, 0, 0, 24.0),
            Sample::new(1000, 0, 40, 12.0), // warmup stutter
            Sample::new(3000, 0, 40, 23.9),
            Sample::new(9000, 0, 41, 23.9),
        ];
        let with_warmup = summarize("x", &samples, 0);
        let without = summarize("x", &samples, 3000);
        assert!(without.delayed_per_min < with_warmup.delayed_per_min);
        assert_eq!(without.delayed_frames, 1, "only the steady-state delta counts");
    }

    #[test]
    fn counters_resetting_are_not_read_as_negative_deltas() {
        // A seek or a new file resets mpv's cumulative counters.
        let samples = vec![
            Sample::new(0, 0, 500, 24.0),
            Sample::new(1000, 0, 3, 24.0), // reset
            Sample::new(2000, 0, 5, 24.0),
        ];
        let s = summarize("x", &samples, 0);
        assert_eq!(s.delayed_frames, 0, "a reset must not produce a huge or negative count");
    }

    #[test]
    fn a_clean_pair_passes() {
        let base = stage("baseline", 60, 1, 23.976);
        let comp = stage("composited", 60, 2, 23.976);
        let v = judge(&base, &comp, &thresholds(), Some(35.0));
        assert_eq!(v.outcome, Outcome::Pass, "{:?}", v.findings);
    }

    #[test]
    fn compositing_that_costs_frames_fails() {
        // The finding that would send the desktop shell to Qt 6.
        let base = stage("baseline", 60, 1, 23.976);
        let comp = stage("composited", 60, 40, 23.9);
        let v = judge(&base, &comp, &thresholds(), Some(30.0));
        assert_eq!(v.outcome, Outcome::Fail);
        assert!(v.added_delayed_per_min > 3.0);
        assert!(v.findings.iter().any(|f| f.contains("late presents")), "{:?}", v.findings);
    }

    #[test]
    fn an_unusable_baseline_is_inconclusive_not_a_failure() {
        // The distinction the whole paired design exists for: a machine that cannot play the clip
        // bare tells us nothing about compositing, and calling that a FAIL would wrongly indict Tauri.
        let base = stage("baseline", 60, 600, 14.0);
        let comp = stage("composited", 60, 900, 12.0);
        let v = judge(&base, &comp, &thresholds(), Some(40.0));
        assert_eq!(v.outcome, Outcome::Inconclusive);
        assert!(
            v.findings[0].contains("cannot play the test clip"),
            "the message must point at the machine, not the architecture: {:?}",
            v.findings
        );
    }

    #[test]
    fn empty_stages_are_inconclusive() {
        let empty = summarize("baseline", &[], 0);
        let comp = stage("composited", 60, 1, 24.0);
        assert_eq!(judge(&empty, &comp, &thresholds(), None).outcome, Outcome::Inconclusive);
        assert_eq!(judge(&comp, &empty, &thresholds(), None).outcome, Outcome::Inconclusive);
    }

    #[test]
    fn a_frame_rate_collapse_fails_even_with_few_late_presents() {
        // Half rate with no "late" frames is what a compositor doing vsync-locked half-rate looks
        // like, and it is a failure a delayed-frame count alone would miss.
        let base = stage("baseline", 60, 1, 23.976);
        let comp = stage("composited", 60, 1, 12.0);
        let v = judge(&base, &comp, &thresholds(), Some(20.0));
        assert_eq!(v.outcome, Outcome::Fail);
        assert!(v.findings.iter().any(|f| f.contains("frame rate")), "{:?}", v.findings);
    }

    #[test]
    fn av_desync_fails_independently() {
        let base = stage("baseline", 60, 1, 23.976);
        let mut comp = stage("composited", 60, 1, 23.976);
        comp.max_avsync_ms = 120.0;
        let v = judge(&base, &comp, &thresholds(), Some(20.0));
        assert_eq!(v.outcome, Outcome::Fail);
        assert!(v.findings.iter().any(|f| f.contains("desync")));
    }

    #[test]
    fn slow_osd_fails_because_a_responsive_overlay_is_the_point() {
        let base = stage("baseline", 60, 1, 23.976);
        let comp = stage("composited", 60, 1, 23.976);
        let v = judge(&base, &comp, &thresholds(), Some(400.0));
        assert_eq!(v.outcome, Outcome::Fail);
        assert!(v.findings.iter().any(|f| f.contains("OSD")));
    }

    #[test]
    fn unmeasured_osd_latency_is_a_note_not_a_pass() {
        // Silently passing an incomplete run would let the spike be declared done on partial data.
        let base = stage("baseline", 60, 1, 23.976);
        let comp = stage("composited", 60, 1, 23.976);
        let v = judge(&base, &comp, &thresholds(), None);
        assert_eq!(v.outcome, Outcome::PassWithNotes);
        assert!(v.findings.iter().any(|f| f.contains("not measured")));
    }

    #[test]
    fn extra_cpu_is_a_note_rather_than_a_failure() {
        // It matters on a laptop — battery and thermals — but it does not invalidate the approach.
        let mut base = stage("baseline", 60, 1, 23.976);
        base.mean_cpu_frac = Some(0.4);
        let mut comp = stage("composited", 60, 1, 23.976);
        comp.mean_cpu_frac = Some(1.6);
        let v = judge(&base, &comp, &thresholds(), Some(30.0));
        assert_eq!(v.outcome, Outcome::PassWithNotes);
        let added = v.added_cpu_frac.expect("both stages reported CPU, so the delta exists");
        assert!((added - 1.2).abs() < 1e-9, "{added}");
        assert!(v.findings.iter().any(|f| f.contains("CPU")));
    }

    #[test]
    fn stages_of_different_lengths_are_compared_fairly() {
        // Runs rarely end at exactly the same second, so the comparable figure is per-minute.
        let short = stage("baseline", 30, 5, 24.0);
        let long = stage("composited", 120, 20, 24.0);
        assert!((short.delayed_per_min - 10.0).abs() < 0.5, "{}", short.delayed_per_min);
        assert!((long.delayed_per_min - 10.0).abs() < 0.5, "{}", long.delayed_per_min);
        let v = judge(&short, &long, &thresholds(), Some(20.0));
        assert!((v.added_delayed_per_min).abs() < 1.0, "{}", v.added_delayed_per_min);
    }

    #[test]
    fn p99_render_time_is_reported_when_available() {
        let samples: Vec<Sample> = (0..100)
            .map(|i| Sample {
                render_ms: Some(if i == 99 { 40.0 } else { 4.0 }),
                ..Sample::new(i * 100, 0, 0, 24.0)
            })
            .collect();
        let s = summarize("x", &samples, 0);
        assert!(s.p99_render_ms.is_some_and(|v| v >= 4.0), "{:?}", s.p99_render_ms);
    }

    #[test]
    fn missing_optional_counters_do_not_fabricate_values() {
        let samples: Vec<Sample> = (0..10).map(|i| Sample::new(i * 1000, 0, 0, 24.0)).collect();
        let s = summarize("x", &samples, 0);
        assert_eq!(s.p99_render_ms, None);
        assert_eq!(s.mean_cpu_frac, None);
        let v = judge(&s, &s, &thresholds(), Some(20.0));
        assert_eq!(v.added_cpu_frac, None, "a missing counter is not a zero delta");
    }
}
