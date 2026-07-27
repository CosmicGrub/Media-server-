//! Readability gating — `docs/14` §4.
//!
//! A generated subtitle is checked before it ships, against **published broadcast standards** rather
//! than invented thresholds. Machine output tends to inherit the shape of speech rather than the shape
//! of readable text: one long cue per utterance, no line breaks, and a reading speed nobody can keep up
//! with.
//!
//! Sources for the numbers: Netflix's Timed Text Style Guide caps lines at 42 characters, two lines,
//! and reading speed at 20 CPS for adult content and 17 for children's; the BBC recommends staying
//! under about 15 CPS because its audience includes elderly viewers and people with reading
//! difficulties; CEA-608 closed captions are limited to 32 characters across at most 4 lines.

/// One subtitle cue.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl Cue {
    pub fn new(start_ms: i64, end_ms: i64, text: impl Into<String>) -> Self {
        Self { start_ms, end_ms, text: text.into() }
    }

    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }

    pub fn lines(&self) -> Vec<&str> {
        self.text.lines().filter(|l| !l.trim().is_empty()).collect()
    }

    /// Characters that count toward reading speed: the visible text, excluding line breaks and markup.
    ///
    /// Counted in `char`s rather than bytes so CJK and accented text are not penalised — a 40-character
    /// Japanese line is not three times as hard to read as a 40-character English one.
    pub fn visible_chars(&self) -> usize {
        strip_markup(&self.text).chars().filter(|c| !c.is_control()).count()
    }

    /// Reading speed in characters per second.
    pub fn cps(&self) -> f32 {
        let secs = self.duration_ms() as f32 / 1000.0;
        if secs <= 0.0 {
            return f32::INFINITY;
        }
        self.visible_chars() as f32 / secs
    }
}

/// Remove ASS override blocks and simple HTML tags so they do not count toward length.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth_brace = 0usize;
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '{' => depth_brace += 1,
            '}' => depth_brace = depth_brace.saturating_sub(1),
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if depth_brace > 0 || in_tag => {}
            _ => out.push(c),
        }
    }
    out
}

/// A named set of thresholds. Different audiences need different numbers, and "accessibility profile"
/// is a real user setting rather than a nicety.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadabilityProfile {
    pub name: &'static str,
    pub max_cps: f32,
    pub max_chars_per_line: usize,
    pub max_lines: usize,
    pub min_duration_ms: i64,
    pub max_duration_ms: i64,
    pub min_gap_ms: i64,
}

impl ReadabilityProfile {
    /// Netflix Timed Text Style Guide, adult content: 42 chars/line, 2 lines, 20 CPS.
    pub const NETFLIX_ADULT: Self = Self {
        name: "Netflix (adult)",
        max_cps: 20.0,
        max_chars_per_line: 42,
        max_lines: 2,
        // 5/6 of a second.
        min_duration_ms: 833,
        max_duration_ms: 7000,
        min_gap_ms: 84, // ~2 frames at 24 fps
    };

    /// Netflix children's content: the same layout at a slower reading speed.
    pub const NETFLIX_CHILDREN: Self =
        Self { name: "Netflix (children)", max_cps: 17.0, ..Self::NETFLIX_ADULT };

    /// BBC guidance, aimed at an audience including elderly viewers and people with reading
    /// difficulties. The right default for an accessibility profile.
    pub const BBC: Self = Self { name: "BBC", max_cps: 15.0, ..Self::NETFLIX_ADULT };

    /// CEA-608 closed captions: 32 characters across up to 4 lines.
    pub const CEA608: Self = Self {
        name: "CEA-608",
        max_cps: 20.0,
        max_chars_per_line: 32,
        max_lines: 4,
        min_duration_ms: 833,
        max_duration_ms: 7000,
        min_gap_ms: 84,
    };

    /// The default: comfortable rather than maximal. 17 CPS is the widely-used industry benchmark and
    /// sits between Netflix's cap and the BBC's recommendation.
    pub const DEFAULT: Self = Self { name: "default", max_cps: 17.0, ..Self::NETFLIX_ADULT };
}

impl Default for ReadabilityProfile {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A specific problem with a specific cue.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Issue {
    /// Reading speed above the profile's cap. The most common machine-output failure: ASR emits one
    /// long utterance per cue with no regard for how fast it must be read.
    TooFast {
        cue: usize,
        cps: f32,
        max: f32,
    },
    LineTooLong {
        cue: usize,
        line: usize,
        chars: usize,
        max: usize,
    },
    TooManyLines {
        cue: usize,
        lines: usize,
        max: usize,
    },
    /// On screen too briefly to register, even if the text is short.
    TooShort {
        cue: usize,
        duration_ms: i64,
        min: i64,
    },
    TooLong {
        cue: usize,
        duration_ms: i64,
        max: i64,
    },
    /// Two cues visible at once. Never permitted.
    Overlap {
        cue: usize,
        previous: usize,
    },
    /// A gap too small to perceive as a change; the two should be merged.
    GapTooSmall {
        cue: usize,
        gap_ms: i64,
        min: i64,
    },
    /// Zero or negative duration, or empty text after markup removal.
    Malformed {
        cue: usize,
    },
    /// Cues out of chronological order.
    OutOfOrder {
        cue: usize,
    },
}

impl Issue {
    pub fn cue_index(&self) -> usize {
        match self {
            Self::TooFast { cue, .. }
            | Self::LineTooLong { cue, .. }
            | Self::TooManyLines { cue, .. }
            | Self::TooShort { cue, .. }
            | Self::TooLong { cue, .. }
            | Self::Overlap { cue, .. }
            | Self::GapTooSmall { cue, .. }
            | Self::Malformed { cue }
            | Self::OutOfOrder { cue } => *cue,
        }
    }

    /// Issues that make a track unusable rather than merely imperfect.
    ///
    /// Overlap and malformed cues break rendering; a slightly-fast cue is a quality note. Rejecting a
    /// whole track for one fast line would throw away usable subtitles.
    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Overlap { .. } | Self::Malformed { .. } | Self::OutOfOrder { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityReport {
    pub profile_name: &'static str,
    pub cue_count: usize,
    pub issues: Vec<Issue>,
    /// Mean reading speed across well-formed cues, for the diagnostics view.
    pub mean_cps: f32,
    pub p95_cps: f32,
}

impl QualityReport {
    /// Fraction of cues with at least one issue.
    pub fn issue_rate(&self) -> f32 {
        if self.cue_count == 0 {
            return 0.0;
        }
        let mut affected: Vec<usize> = self.issues.iter().map(Issue::cue_index).collect();
        affected.sort_unstable();
        affected.dedup();
        affected.len() as f32 / self.cue_count as f32
    }

    pub fn has_blocking(&self) -> bool {
        self.issues.iter().any(Issue::is_blocking)
    }

    /// Usable as-is: nothing blocking, and issues confined to a small minority of cues.
    ///
    /// The 10% tolerance exists because human-authored subtitles routinely exceed these thresholds too
    /// — the standards are targets for new work, and a gate that rejected every real-world subtitle
    /// would simply be turned off.
    pub fn is_acceptable(&self) -> bool {
        !self.has_blocking() && self.issue_rate() <= 0.10
    }
}

/// Check a cue list against a profile.
pub fn check_readability(cues: &[Cue], profile: &ReadabilityProfile) -> QualityReport {
    let mut issues = Vec::new();
    let mut speeds: Vec<f32> = Vec::new();

    for (i, cue) in cues.iter().enumerate() {
        let visible = cue.visible_chars();
        if cue.duration_ms() <= 0 || visible == 0 {
            issues.push(Issue::Malformed { cue: i });
            continue;
        }

        let cps = cue.cps();
        speeds.push(cps);
        if cps > profile.max_cps {
            issues.push(Issue::TooFast { cue: i, cps, max: profile.max_cps });
        }

        let lines = cue.lines();
        if lines.len() > profile.max_lines {
            issues.push(Issue::TooManyLines { cue: i, lines: lines.len(), max: profile.max_lines });
        }
        for (li, line) in lines.iter().enumerate() {
            let n = strip_markup(line).chars().count();
            if n > profile.max_chars_per_line {
                issues.push(Issue::LineTooLong {
                    cue: i,
                    line: li,
                    chars: n,
                    max: profile.max_chars_per_line,
                });
            }
        }

        if cue.duration_ms() < profile.min_duration_ms {
            issues.push(Issue::TooShort {
                cue: i,
                duration_ms: cue.duration_ms(),
                min: profile.min_duration_ms,
            });
        }
        if cue.duration_ms() > profile.max_duration_ms {
            issues.push(Issue::TooLong {
                cue: i,
                duration_ms: cue.duration_ms(),
                max: profile.max_duration_ms,
            });
        }

        if i > 0 {
            let prev = &cues[i - 1];
            if cue.start_ms < prev.start_ms {
                issues.push(Issue::OutOfOrder { cue: i });
            } else if cue.start_ms < prev.end_ms {
                issues.push(Issue::Overlap { cue: i, previous: i - 1 });
            } else {
                let gap = cue.start_ms - prev.end_ms;
                if gap > 0 && gap < profile.min_gap_ms {
                    issues.push(Issue::GapTooSmall {
                        cue: i,
                        gap_ms: gap,
                        min: profile.min_gap_ms,
                    });
                }
            }
        }
    }

    speeds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean_cps =
        if speeds.is_empty() { 0.0 } else { speeds.iter().sum::<f32>() / speeds.len() as f32 };
    let p95_cps = if speeds.is_empty() {
        0.0
    } else {
        speeds[((speeds.len() as f32 * 0.95) as usize).min(speeds.len() - 1)]
    };

    QualityReport { profile_name: profile.name, cue_count: cues.len(), issues, mean_cps, p95_cps }
}

/// Split a cue's text into lines that fit the profile, breaking at word boundaries.
///
/// ASR output arrives as one unbroken run per utterance, so re-wrapping is not optional — it is the
/// difference between a readable subtitle and a wall of text. Splits prefer punctuation, because a
/// break at a clause boundary reads far better than one at an arbitrary word.
pub fn wrap_text(text: &str, profile: &ReadabilityProfile) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= profile.max_chars_per_line {
        return flat;
    }

    let words: Vec<&str> = flat.split(' ').collect();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    // Aim for balanced lines rather than filling greedily: two lines of 30 read better than one of 42
    // and one of 18.
    let total = flat.chars().count();
    let target_lines = total.div_ceil(profile.max_chars_per_line).max(1);
    let target = total.div_ceil(target_lines);

    for word in words {
        let candidate_len =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        let ends_clause = current.ends_with([',', '.', '!', '?', ';', ':', '—']);
        let should_break = !current.is_empty()
            && (candidate_len > profile.max_chars_per_line
                || (current.chars().count() >= target && ends_clause));
        if should_break {
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
    lines.truncate(profile.max_lines.max(1));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_thresholds_are_recorded_faithfully() {
        // These are external standards, not preferences. Getting them wrong makes the gate meaningless.
        assert_eq!(ReadabilityProfile::NETFLIX_ADULT.max_cps, 20.0);
        assert_eq!(ReadabilityProfile::NETFLIX_ADULT.max_chars_per_line, 42);
        assert_eq!(ReadabilityProfile::NETFLIX_ADULT.max_lines, 2);
        assert_eq!(ReadabilityProfile::NETFLIX_CHILDREN.max_cps, 17.0);
        assert_eq!(ReadabilityProfile::BBC.max_cps, 15.0);
        assert_eq!(ReadabilityProfile::CEA608.max_chars_per_line, 32);
        assert_eq!(ReadabilityProfile::CEA608.max_lines, 4);
        // 5/6 of a second.
        assert_eq!(ReadabilityProfile::NETFLIX_ADULT.min_duration_ms, 833);
    }

    #[test]
    fn a_clean_subtitle_passes() {
        let cues = vec![
            Cue::new(1000, 3000, "This is a perfectly readable line."),
            Cue::new(3500, 5500, "And so is this one."),
        ];
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(r.is_acceptable());
    }

    #[test]
    fn the_classic_asr_failure_is_caught() {
        // One long utterance crammed into a short cue: unreadable, and exactly what raw ASR emits.
        let cues = vec![Cue::new(
            0,
            1500,
            "This is an extremely long line of dialogue that no human being could possibly read in \
             one and a half seconds no matter how motivated they are.",
        )];
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(r.issues.iter().any(|i| matches!(i, Issue::TooFast { .. })), "{:?}", r.issues);
        assert!(r.issues.iter().any(|i| matches!(i, Issue::LineTooLong { .. })));
        assert!(!r.is_acceptable());
    }

    #[test]
    fn overlap_is_blocking_but_a_fast_cue_is_not() {
        // Overlap breaks rendering; a fast cue is a quality note. Rejecting a track for one fast line
        // would throw away usable subtitles.
        let overlapping = vec![Cue::new(0, 3000, "First."), Cue::new(2000, 4000, "Second.")];
        let r = check_readability(&overlapping, &ReadabilityProfile::DEFAULT);
        assert!(r.has_blocking());
        assert!(!r.is_acceptable());

        let fast = vec![Cue::new(0, 900, "Quite a lot of words for under a second here.")];
        let r2 = check_readability(&fast, &ReadabilityProfile::DEFAULT);
        assert!(!r2.has_blocking(), "a fast cue must not block: {:?}", r2.issues);
    }

    #[test]
    fn out_of_order_cues_are_blocking() {
        let cues = vec![Cue::new(5000, 7000, "Later."), Cue::new(1000, 3000, "Earlier.")];
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(r.issues.iter().any(|i| matches!(i, Issue::OutOfOrder { .. })));
        assert!(r.has_blocking());
    }

    #[test]
    fn zero_and_negative_durations_are_malformed_not_infinite_cps() {
        let cues = vec![Cue::new(1000, 1000, "Instant."), Cue::new(5000, 4000, "Backwards.")];
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert_eq!(r.issues.iter().filter(|i| matches!(i, Issue::Malformed { .. })).count(), 2);
        assert!(r.mean_cps.is_finite(), "malformed cues must not poison the statistics");
    }

    #[test]
    fn markup_does_not_count_toward_length_or_speed() {
        // An ASS override block is invisible; charging the reader for it would reject styled subtitles.
        let styled = Cue::new(0, 2000, "{\\pos(960,1000)\\c&HFFFFFF&}Hello.");
        let plain = Cue::new(0, 2000, "Hello.");
        assert_eq!(styled.visible_chars(), plain.visible_chars());
        let html = Cue::new(0, 2000, "<i>Hello.</i>");
        assert_eq!(html.visible_chars(), plain.visible_chars());
    }

    #[test]
    fn cjk_is_counted_in_characters_not_bytes() {
        // Counting bytes would make every Japanese subtitle fail the line-length check.
        let ja = Cue::new(0, 2000, "こんにちは世界");
        assert_eq!(ja.visible_chars(), 7, "7 characters, 21 bytes");
        let r = check_readability(&[ja], &ReadabilityProfile::DEFAULT);
        assert!(!r.issues.iter().any(|i| matches!(i, Issue::LineTooLong { .. })));
    }

    #[test]
    fn cps_is_computed_at_the_boundary_correctly() {
        // 34 characters over 2 seconds is exactly 17 CPS: at the cap, so acceptable.
        let text = "a".repeat(34);
        let at_cap = Cue::new(0, 2000, text.clone());
        assert!((at_cap.cps() - 17.0).abs() < 0.01);
        let r = check_readability(
            &[at_cap],
            &ReadabilityProfile { max_chars_per_line: 100, ..ReadabilityProfile::DEFAULT },
        );
        assert!(
            !r.issues.iter().any(|i| matches!(i, Issue::TooFast { .. })),
            "at the cap is allowed"
        );

        let over = Cue::new(0, 1999, text);
        let r2 = check_readability(
            &[over],
            &ReadabilityProfile { max_chars_per_line: 100, ..ReadabilityProfile::DEFAULT },
        );
        assert!(r2.issues.iter().any(|i| matches!(i, Issue::TooFast { .. })));
    }

    #[test]
    fn tiny_gaps_are_flagged_for_merging() {
        let cues = vec![Cue::new(0, 2000, "First."), Cue::new(2020, 4000, "Second.")];
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(r.issues.iter().any(|i| matches!(i, Issue::GapTooSmall { .. })));
        assert!(!r.has_blocking(), "a small gap is a merge hint, not a failure");
    }

    #[test]
    fn a_few_bad_cues_in_a_long_track_is_still_acceptable() {
        // Real human subtitles exceed these thresholds too. A gate that rejected everything would be
        // switched off, which is worse than a tolerant one.
        let mut cues: Vec<Cue> = (0..100)
            .map(|i| Cue::new(i * 3000, i * 3000 + 2000, "A short readable line."))
            .collect();
        cues[5] =
            Cue::new(15_000, 15_600, "Rather a lot of characters for six hundred milliseconds.");
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(!r.issues.is_empty());
        assert!(r.is_acceptable(), "issue rate {} should be tolerable", r.issue_rate());
    }

    #[test]
    fn many_bad_cues_are_not_acceptable() {
        let cues: Vec<Cue> = (0..20)
            .map(|i| {
                Cue::new(i * 3000, i * 3000 + 400, "Far too many characters for four hundred ms.")
            })
            .collect();
        let r = check_readability(&cues, &ReadabilityProfile::DEFAULT);
        assert!(!r.is_acceptable(), "issue rate {}", r.issue_rate());
    }

    #[test]
    fn wrapping_respects_the_line_limit_and_balances_lines() {
        let long =
            "This is a fairly long single line of dialogue that needs wrapping to two lines.";
        let wrapped = wrap_text(long, &ReadabilityProfile::DEFAULT);
        let lines: Vec<&str> = wrapped.lines().collect();
        assert!(lines.len() >= 2, "{wrapped:?}");
        for l in &lines {
            assert!(
                l.chars().count() <= ReadabilityProfile::DEFAULT.max_chars_per_line,
                "line too long: {l:?}"
            );
        }
        // No word was cut in half.
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            long.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(wrap_text("Short.", &ReadabilityProfile::DEFAULT), "Short.");
    }

    #[test]
    fn wrapping_never_exceeds_the_line_count() {
        let very_long = "word ".repeat(200);
        let wrapped = wrap_text(&very_long, &ReadabilityProfile::DEFAULT);
        assert!(wrapped.lines().count() <= ReadabilityProfile::DEFAULT.max_lines);
    }

    #[test]
    fn an_empty_track_reports_cleanly() {
        let r = check_readability(&[], &ReadabilityProfile::DEFAULT);
        assert_eq!(r.cue_count, 0);
        assert_eq!(r.issue_rate(), 0.0);
        assert_eq!(r.mean_cps, 0.0);
        assert!(r.is_acceptable());
    }
}
