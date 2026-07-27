//! Subtitle sync correction — `docs/14` §5.
//!
//! An out-of-sync subtitle is experienced as a broken subtitle, and providers routinely return files
//! timed for a different cut or a different frame rate. Two failure shapes cover almost all of it:
//!
//! - **Frame-rate mismatch** — a file authored for 25 fps played against 23.976 fps content drifts
//!   linearly, ending roughly 4% out. Over two hours that is nearly five minutes. The ratio comes from
//!   a small known set, so it can be *identified* rather than fitted.
//! - **Constant offset** — a different intro length or a missing distributor logo shifts everything by
//!   a fixed amount.
//!
//! Both are detected by comparing cue starts against voice activity, and both are recorded so the
//! correction can be undone.

/// Frame rates subtitles are authored against. Any mismatch between two of these produces one of a
/// small set of drift ratios, which is what makes identification possible.
const KNOWN_RATES: [f64; 6] = [
    23.976_023_976_023_978, // 24000/1001
    24.0,
    25.0,
    29.97,
    30.0,
    50.0,
];

/// What to do to bring a subtitle into sync.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncCorrection {
    /// Already in sync within tolerance.
    None,
    /// Shift every cue by a constant. Positive delays the subtitles.
    Offset { ms: i64 },
    /// Rescale timings by `from_fps / to_fps`, then shift.
    ///
    /// Rescaling alone is rarely enough: a rate-converted file usually also starts at a different
    /// point, so the two corrections travel together.
    Rescale { from_fps: f64, to_fps: f64, then_offset_ms: i64 },
}

impl SyncCorrection {
    /// Apply the correction to a timestamp.
    pub fn apply(self, ms: i64) -> i64 {
        match self {
            Self::None => ms,
            Self::Offset { ms: delta } => ms + delta,
            Self::Rescale { from_fps, to_fps, then_offset_ms } => {
                if to_fps <= 0.0 {
                    return ms;
                }
                ((ms as f64) * (from_fps / to_fps)).round() as i64 + then_offset_ms
            }
        }
    }

    pub fn is_noop(self) -> bool {
        match self {
            Self::None => true,
            Self::Offset { ms } => ms == 0,
            Self::Rescale { from_fps, to_fps, then_offset_ms } => {
                (from_fps - to_fps).abs() < f64::EPSILON && then_offset_ms == 0
            }
        }
    }

    /// A user-facing description. Sync corrections are applied automatically, so the user is told what
    /// changed and can undo it.
    pub fn explain(self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Offset { ms: 0 } => None,
            Self::Offset { ms } => Some(format!(
                "Shifted the subtitles by {:+.3} s to match the audio.",
                ms as f64 / 1000.0
            )),
            Self::Rescale { from_fps, to_fps, then_offset_ms } => Some(format!(
                "These subtitles were timed for {from_fps:.3} fps but the video is {to_fps:.3} fps. \
                 Rescaled and shifted by {:+.3} s.",
                then_offset_ms as f64 / 1000.0
            )),
        }
    }
}

/// Tolerance below which a subtitle counts as in sync. Human authoring varies by more than this, and
/// "correcting" a fifty-millisecond difference would churn timings for no perceptible gain.
pub const SYNC_TOLERANCE_MS: i64 = 150;

/// Widest offset worth considering when pairing a cue with a speech onset.
///
/// Generous, because a subtitle timed for a different cut can be minutes out before any rescaling.
const MAX_PAIR_WINDOW_MS: i64 = 600_000;

/// Bucket width for the offset histogram. Wide enough to absorb per-cue authoring variation, narrow
/// enough that two genuinely different offsets do not merge.
const OFFSET_BUCKET_MS: i64 = 50;

/// Bound on pairwise comparisons, so a pathological input cannot make this quadratic in practice.
const MAX_PAIRS: usize = 4_000_000;

/// Nearest speech onset to `target`, within `window`.
fn nearest(speech_starts: &[i64], target: i64, window: i64) -> Option<i64> {
    speech_starts
        .iter()
        .copied()
        .min_by_key(|s| (s - target).abs())
        .filter(|s| (s - target).abs() <= window)
}

/// Estimate a constant offset between cue starts and detected speech starts.
///
/// Uses a **histogram of all plausible pairwise differences**, taking the modal bucket and then the
/// median within it. Nearest-neighbour matching looks simpler but is wrong: when the true offset is
/// near half the cue spacing, roughly half the cues pair with the *preceding* speech onset and the
/// estimate collapses to the wrong sign entirely. A histogram is indifferent to that, and it also
/// ignores cues with no corresponding speech — translated signs, on-screen captions — which would drag
/// a mean badly.
pub fn estimate_offset(cue_starts: &[i64], speech_starts: &[i64]) -> Option<i64> {
    if cue_starts.is_empty() || speech_starts.is_empty() {
        return None;
    }
    if cue_starts.len().saturating_mul(speech_starts.len()) > MAX_PAIRS {
        // Fall back to sampling rather than refusing: a very long file still deserves an answer.
        let step = cue_starts.len().div_ceil(2000).max(1);
        let sampled: Vec<i64> = cue_starts.iter().copied().step_by(step).collect();
        return estimate_offset(&sampled, speech_starts);
    }

    let mut buckets: std::collections::BTreeMap<i64, Vec<i64>> = std::collections::BTreeMap::new();
    for c in cue_starts {
        for s in speech_starts {
            let diff = s - c;
            if diff.abs() > MAX_PAIR_WINDOW_MS {
                continue;
            }
            buckets.entry(diff.div_euclid(OFFSET_BUCKET_MS)).or_default().push(diff);
        }
    }
    // Modal bucket; ties break toward the smaller absolute offset, because the smallest correction
    // that explains the data is the one most likely to be real.
    let (_, best) = buckets
        .iter()
        .max_by_key(|(bucket, diffs)| (diffs.len(), std::cmp::Reverse(bucket.abs())))?;
    let mut diffs = best.clone();
    diffs.sort_unstable();
    Some(diffs[diffs.len() / 2])
}

/// Identify a frame-rate mismatch from how the drift grows across the file.
///
/// Compares the span between the first and last cue against the span between the corresponding speech
/// onsets: a constant offset leaves the span unchanged, while a rate mismatch scales it.
pub fn detect_framerate_mismatch(
    first_cue_ms: i64,
    first_speech_ms: i64,
    last_cue_ms: i64,
    last_speech_ms: i64,
) -> Option<(f64, f64)> {
    if last_cue_ms <= first_cue_ms || first_cue_ms < 0 {
        return None;
    }
    let span_cue = (last_cue_ms - first_cue_ms) as f64;
    let span_speech = (last_speech_ms - first_speech_ms) as f64;
    if span_cue <= 0.0 || span_speech <= 0.0 {
        return None;
    }
    let observed = span_speech / span_cue;
    // A ratio within tolerance of 1 is a constant offset, not a rate mismatch.
    if (observed - 1.0).abs() < 0.002 {
        return None;
    }

    // Match against the ratios the known rate pairs can produce, rather than fitting an arbitrary
    // scale factor: an arbitrary factor would happily "correct" a subtitle for the wrong cut.
    let mut best: Option<(f64, f64, f64)> = None;
    for from in KNOWN_RATES {
        for to in KNOWN_RATES {
            if (from - to).abs() < f64::EPSILON {
                continue;
            }
            let ratio = from / to;
            let error = (ratio - observed).abs() / observed;
            if error < 0.004 && best.is_none_or(|(_, _, e)| error < e) {
                best = Some((from, to, error));
            }
        }
    }
    best.map(|(from, to, _)| (from, to))
}

/// Decide the correction for a subtitle, given its cue starts and detected speech onsets.
pub fn detect_correction(cue_starts: &[i64], speech_starts: &[i64]) -> SyncCorrection {
    let Some(offset) = estimate_offset(cue_starts, speech_starts) else {
        return SyncCorrection::None;
    };

    // Anchor the rate check on the outermost cues, paired without the offset histogram's assumption
    // of a single constant shift — under a rate mismatch the end of a two-hour film can be minutes
    // out, which is exactly the signal being looked for.
    if let (Some(&first_cue), Some(&last_cue)) = (cue_starts.first(), cue_starts.last())
        && cue_starts.len() >= 4
        && let (Some(first_speech), Some(last_speech)) = (
            nearest(speech_starts, first_cue, MAX_PAIR_WINDOW_MS),
            nearest(speech_starts, last_cue, MAX_PAIR_WINDOW_MS),
        )
    {
        if let Some((from, to)) =
            detect_framerate_mismatch(first_cue, first_speech, last_cue, last_speech)
        {
            // After rescaling, whatever constant offset remains is applied on top.
            let scaled_first = ((first_cue as f64) * (from / to)).round() as i64;
            return SyncCorrection::Rescale {
                from_fps: from,
                to_fps: to,
                then_offset_ms: first_speech - scaled_first,
            };
        }
    }

    if offset.abs() <= SYNC_TOLERANCE_MS {
        SyncCorrection::None
    } else {
        SyncCorrection::Offset { ms: offset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_in_sync_subtitle_is_left_alone() {
        let cues = vec![1000, 5000, 9000, 13_000, 17_000];
        let speech = vec![1020, 5010, 8990, 13_030, 17_000];
        assert_eq!(detect_correction(&cues, &speech), SyncCorrection::None);
    }

    #[test]
    fn a_constant_offset_is_detected_and_applied() {
        // A different intro length: everything shifted by the same amount.
        let cues = vec![1000, 5000, 9000, 13_000, 17_000];
        let speech: Vec<i64> = cues.iter().map(|c| c + 2500).collect();
        let correction = detect_correction(&cues, &speech);
        assert_eq!(correction, SyncCorrection::Offset { ms: 2500 });
        assert_eq!(correction.apply(1000), 3500);
        assert!(correction.explain().is_some_and(|e| e.contains("2.500")));
    }

    #[test]
    fn a_negative_offset_works_too() {
        let cues = vec![10_000, 14_000, 18_000, 22_000];
        let speech: Vec<i64> = cues.iter().map(|c| c - 3000).collect();
        assert_eq!(detect_correction(&cues, &speech), SyncCorrection::Offset { ms: -3000 });
    }

    #[test]
    fn the_25_to_23_976_mismatch_is_identified_by_rate_not_fitted() {
        // The classic PAL-vs-film case: a subtitle authored for 25 fps against 23.976 fps content ends
        // about 4% out — nearly five minutes over a two-hour film.
        let from = 25.0;
        let to = 23.976_023_976_023_978;
        let cues: Vec<i64> = (0..12).map(|i| 60_000 + i * 600_000).collect();
        let speech: Vec<i64> =
            cues.iter().map(|c| ((*c as f64) * (from / to)).round() as i64).collect();

        match detect_correction(&cues, &speech) {
            SyncCorrection::Rescale { from_fps, to_fps, .. } => {
                assert!((from_fps - from).abs() < 0.01, "got from={from_fps}");
                assert!((to_fps - to).abs() < 0.01, "got to={to_fps}");
            }
            other => panic!("expected a rescale, got {other:?}"),
        }
    }

    #[test]
    fn a_rescale_correction_actually_realigns_the_timings() {
        let from = 25.0;
        let to = 23.976_023_976_023_978;
        let cues: Vec<i64> = (0..12).map(|i| 60_000 + i * 600_000).collect();
        let speech: Vec<i64> =
            cues.iter().map(|c| ((*c as f64) * (from / to)).round() as i64).collect();
        let correction = detect_correction(&cues, &speech);

        for (cue, want) in cues.iter().zip(&speech) {
            let got = correction.apply(*cue);
            assert!(
                (got - want).abs() <= SYNC_TOLERANCE_MS,
                "cue {cue}: corrected to {got}, wanted {want}"
            );
        }
    }

    #[test]
    fn a_rescale_explains_itself_in_terms_a_user_can_act_on() {
        let c = SyncCorrection::Rescale { from_fps: 25.0, to_fps: 23.976, then_offset_ms: -500 };
        let msg = c.explain().expect("a rescale is worth explaining");
        assert!(msg.contains("25"), "{msg}");
        assert!(msg.contains("23.976"), "{msg}");
    }

    #[test]
    fn a_constant_offset_is_not_mistaken_for_a_rate_mismatch() {
        // Both produce a nonzero error; only a rate mismatch makes it *grow*.
        let cues: Vec<i64> = (0..12).map(|i| 60_000 + i * 600_000).collect();
        let speech: Vec<i64> = cues.iter().map(|c| c + 4000).collect();
        assert_eq!(detect_correction(&cues, &speech), SyncCorrection::Offset { ms: 4000 });
    }

    #[test]
    fn an_arbitrary_scale_factor_is_not_accepted_as_a_rate_mismatch() {
        // Fitting any factor would happily "correct" a subtitle for an entirely different cut. Only
        // ratios the known rate pairs produce are allowed.
        assert_eq!(
            detect_framerate_mismatch(1000, 1370, 100_000, 137_000),
            None,
            "1.37x is no rate pair"
        );
    }

    #[test]
    fn median_estimation_survives_cues_with_no_matching_speech() {
        // Translated signs and on-screen captions have no speech under them; a mean would be dragged
        // badly by them.
        let cues = vec![1000, 5000, 9000, 13_000, 17_000, 21_000];
        let mut speech: Vec<i64> = cues.iter().map(|c| c + 2000).collect();
        speech.push(500_000); // a distant unrelated onset
        assert_eq!(estimate_offset(&cues, &speech), Some(2000));
    }

    #[test]
    fn wildly_distant_speech_is_not_paired_with_a_cue() {
        // The window is generous — a subtitle for a different cut can be minutes out — but not
        // unlimited, or unrelated files would "sync" to each other.
        assert_eq!(estimate_offset(&[1000], &[9_000_000]), None);
    }

    #[test]
    fn a_half_spacing_offset_does_not_flip_the_estimate_sign() {
        // Nearest-neighbour matching fails exactly here: with the true offset near half the cue
        // spacing, about half the cues pair with the *preceding* onset and the estimate inverts.
        let cues: Vec<i64> = (0..10).map(|i| 1000 + i * 4000).collect();
        let speech: Vec<i64> = cues.iter().map(|c| c + 2000).collect();
        assert_eq!(estimate_offset(&cues, &speech), Some(2000));
    }

    #[test]
    fn empty_input_yields_no_correction_rather_than_a_panic() {
        assert_eq!(estimate_offset(&[], &[1000]), None);
        assert_eq!(estimate_offset(&[1000], &[]), None);
        assert_eq!(detect_correction(&[], &[]), SyncCorrection::None);
        assert_eq!(detect_framerate_mismatch(0, 0, 0, 0), None);
    }

    #[test]
    fn corrections_report_whether_they_change_anything() {
        assert!(SyncCorrection::None.is_noop());
        assert!(SyncCorrection::Offset { ms: 0 }.is_noop());
        assert!(!SyncCorrection::Offset { ms: 500 }.is_noop());
        assert!(SyncCorrection::None.explain().is_none());
        assert!(SyncCorrection::Offset { ms: 0 }.explain().is_none());
    }

    #[test]
    fn apply_is_safe_against_a_zero_target_rate() {
        let bad = SyncCorrection::Rescale { from_fps: 25.0, to_fps: 0.0, then_offset_ms: 0 };
        assert_eq!(bad.apply(1234), 1234, "must not divide by zero");
    }
}
