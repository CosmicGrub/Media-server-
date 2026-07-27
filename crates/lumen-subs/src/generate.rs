//! ASR and translation quality gating — `docs/14` §3.2 and §4.
//!
//! Whisper's failure modes are specific and well known, and each one produces output that *looks*
//! plausible. They are worth detecting individually rather than hoping a general quality score catches
//! them:
//!
//! - **Hallucination on silence.** Given music or room tone, Whisper emits confident text — commonly a
//!   caption from its training data. The tell is a cue with no speech under it.
//! - **Repetition loops.** A decode gets stuck and repeats a phrase for many cues.
//! - **Runaway translation.** A translation several times longer than its source is a decode failure,
//!   not a verbose language.
//! - **Language misdetection.** The container's language tag is wrong often enough to matter, and so is
//!   Whisper's own detection; disagreement between them is worth surfacing rather than resolving
//!   silently.

use crate::LangTag;
use crate::readability::Cue;

/// How the audio should be prepared and which models to use.
#[derive(Debug, Clone, PartialEq)]
pub struct AsrPlan {
    /// Whisper checkpoint. `large-v3` for quality; `turbo`/`distil` trade about 1% WER for roughly 6×
    /// speed. No v4 exists as of mid-2026, so large-v3 remains the production-safe choice.
    pub asr_model: String,
    /// Translation model, if the transcript needs translating. A purpose-built translator (NLLB-200,
    /// MADLAD-400) beats Whisper's own `translate` task.
    pub mt_model: Option<String>,
    /// Feed the centre channel where a 5.1 track exists: dialogue lives there, and excluding music and
    /// effects measurably improves word error rate. Downmixing is the fallback.
    pub use_center_channel: bool,
    /// Recover word-level timing with forced phoneme alignment (the WhisperX approach: faster-whisper
    /// plus wav2vec2). Whisper's own segment timestamps are too coarse for subtitles.
    pub forced_alignment: bool,
    /// Split overlapping dialogue into separate cues, and supply speaker labels for an SDH variant.
    pub diarize: bool,
    /// Detect the spoken language from audio rather than trusting the container tag.
    pub detect_language: bool,
}

impl AsrPlan {
    /// Quality-first defaults. Everything that improves accuracy is on, because the alternative to a
    /// good generated subtitle is usually no subtitle at all.
    pub fn quality(target: &LangTag, spoken: &LangTag) -> Self {
        Self {
            asr_model: "large-v3".into(),
            mt_model: (!spoken.base_eq(target)).then(|| "nllb-200".to_string()),
            use_center_channel: true,
            forced_alignment: true,
            diarize: true,
            detect_language: true,
        }
    }

    /// Faster, for bulk work over a large library.
    pub fn fast(target: &LangTag, spoken: &LangTag) -> Self {
        Self { asr_model: "large-v3-turbo".into(), diarize: false, ..Self::quality(target, spoken) }
    }

    /// True when this plan produces a translation as well as a transcription — two lossy stages, and
    /// the weakest rung of the ladder.
    pub fn is_translating(&self) -> bool {
        self.mt_model.is_some()
    }
}

/// Per-cue evidence from the generator, used for gating.
#[derive(Debug, Clone, PartialEq)]
pub struct CueEvidence {
    /// Mean token confidence in `0.0..=1.0`, where the model reports one.
    pub confidence: Option<f32>,
    /// Whether voice activity detection found speech overlapping this cue. The single best
    /// hallucination detector.
    pub has_speech: bool,
    /// Character count of the source text, when this cue is a translation.
    pub source_chars: Option<usize>,
}

impl CueEvidence {
    pub fn with_speech(confidence: f32) -> Self {
        Self { confidence: Some(confidence), has_speech: true, source_chars: None }
    }

    pub fn without_speech(confidence: f32) -> Self {
        Self { confidence: Some(confidence), has_speech: false, source_chars: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GenerationIssue {
    /// Text over a stretch with no detected speech. Whisper's characteristic failure on music and
    /// silence, and it produces confident, plausible, entirely invented lines.
    HallucinationOnSilence { cue: usize },
    /// The same text repeated across consecutive cues beyond any plausible dialogue.
    RepetitionLoop { first_cue: usize, repeats: usize },
    /// A translation far longer than its source.
    RunawayLength { cue: usize, ratio: f32 },
    /// Below the per-cue confidence floor.
    LowConfidence { cue: usize, confidence: f32, floor: f32 },
    /// Mean confidence across the track is below the floor.
    LowMeanConfidence { mean: f32, floor: f32 },
    /// Audio-detected language disagrees with the container's tag. Not resolved silently: one of them
    /// is wrong and the user is better placed to say which.
    LanguageMismatch { detected: LangTag, tagged: LangTag },
}

impl GenerationIssue {
    /// Issues that make the output untrustworthy rather than merely imperfect.
    pub fn is_disqualifying(&self) -> bool {
        matches!(self, Self::RepetitionLoop { .. } | Self::LowMeanConfidence { .. })
    }
}

/// What to do with generated output.
#[derive(Debug, Clone, PartialEq)]
pub enum GenerationVerdict {
    /// Good enough to attach as a normal (still labelled) track.
    Accept { issues: Vec<GenerationIssue> },
    /// Attach, but as a draft the user is asked to confirm. Better than nothing and honest about it.
    Draft { issues: Vec<GenerationIssue> },
    /// Do not attach. Wrong subtitles are worse than none, because a viewer cannot tell.
    Reject { issues: Vec<GenerationIssue> },
}

impl GenerationVerdict {
    pub fn issues(&self) -> &[GenerationIssue] {
        match self {
            Self::Accept { issues } | Self::Draft { issues } | Self::Reject { issues } => issues,
        }
    }

    pub fn is_attachable(&self) -> bool {
        !matches!(self, Self::Reject { .. })
    }
}

/// Per-cue confidence floor.
pub const CONFIDENCE_FLOOR: f32 = 0.45;
/// Mean-confidence floor across a track.
pub const MEAN_CONFIDENCE_FLOOR: f32 = 0.60;
/// Consecutive identical cues that constitute a decode loop rather than dialogue. Three is deliberate:
/// two repeats can be genuine emphasis ("No. No."), four is never dialogue.
pub const REPETITION_THRESHOLD: usize = 4;
/// Translation-to-source length ratio above which the output is a runaway.
pub const RUNAWAY_RATIO: f32 = 3.0;
/// Fraction of cues that may be hallucinations before the whole track is untrustworthy.
pub const MAX_HALLUCINATION_RATE: f32 = 0.05;

/// Gate generated cues.
///
/// `evidence` is parallel to `cues`; a shorter slice means the tail carries no evidence, which is
/// treated as unknown rather than bad.
pub fn gate_generated(
    cues: &[Cue],
    evidence: &[CueEvidence],
    detected_language: Option<&LangTag>,
    tagged_language: Option<&LangTag>,
) -> GenerationVerdict {
    let mut issues = Vec::new();

    if let (Some(detected), Some(tagged)) = (detected_language, tagged_language)
        && !detected.is_undetermined()
        && !tagged.is_undetermined()
        && !detected.base_eq(tagged)
    {
        issues.push(GenerationIssue::LanguageMismatch {
            detected: detected.clone(),
            tagged: tagged.clone(),
        });
    }

    let mut confidences: Vec<f32> = Vec::new();
    let mut hallucinations = 0usize;

    for (i, cue) in cues.iter().enumerate() {
        let Some(ev) = evidence.get(i) else { continue };

        if !ev.has_speech && !cue.text.trim().is_empty() {
            issues.push(GenerationIssue::HallucinationOnSilence { cue: i });
            hallucinations += 1;
        }
        if let Some(c) = ev.confidence {
            confidences.push(c);
            if c < CONFIDENCE_FLOOR {
                issues.push(GenerationIssue::LowConfidence {
                    cue: i,
                    confidence: c,
                    floor: CONFIDENCE_FLOOR,
                });
            }
        }
        if let Some(src) = ev.source_chars
            && src > 0
        {
            let ratio = cue.visible_chars() as f32 / src as f32;
            if ratio > RUNAWAY_RATIO {
                issues.push(GenerationIssue::RunawayLength { cue: i, ratio });
            }
        }
    }

    // Repetition loops: consecutive cues with identical normalised text.
    let mut run_start = 0usize;
    let mut run_len = 1usize;
    for i in 1..cues.len() {
        let same = normalized(&cues[i].text) == normalized(&cues[i - 1].text)
            && !cues[i].text.trim().is_empty();
        if same {
            run_len += 1;
        } else {
            if run_len >= REPETITION_THRESHOLD {
                issues.push(GenerationIssue::RepetitionLoop {
                    first_cue: run_start,
                    repeats: run_len,
                });
            }
            run_start = i;
            run_len = 1;
        }
    }
    if run_len >= REPETITION_THRESHOLD {
        issues.push(GenerationIssue::RepetitionLoop { first_cue: run_start, repeats: run_len });
    }

    if !confidences.is_empty() {
        let mean = confidences.iter().sum::<f32>() / confidences.len() as f32;
        if mean < MEAN_CONFIDENCE_FLOOR {
            issues.push(GenerationIssue::LowMeanConfidence { mean, floor: MEAN_CONFIDENCE_FLOOR });
        }
    }

    let hallucination_rate =
        if cues.is_empty() { 0.0 } else { hallucinations as f32 / cues.len() as f32 };

    if issues.iter().any(GenerationIssue::is_disqualifying)
        || hallucination_rate > MAX_HALLUCINATION_RATE
    {
        return GenerationVerdict::Reject { issues };
    }
    // Anything less than clean output is offered as a draft: attached and usable, but flagged so the
    // user knows to check it rather than trusting it.
    if issues.is_empty() {
        GenerationVerdict::Accept { issues }
    } else {
        GenerationVerdict::Draft { issues }
    }
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Header lines written into a generated subtitle file, recording exactly how it was produced.
///
/// `docs/14` §3.3: the provenance travels with the file, so a subtitle copied out of the library still
/// says what it is. A user who finds it years later should not have to guess.
pub fn provenance_header(plan: &AsrPlan, source: &str, generated_at: &str) -> Vec<String> {
    let mut lines = vec![
        "NOTE This subtitle was generated automatically and has not been reviewed by a human."
            .into(),
        format!("NOTE Source: {source}"),
        format!("NOTE ASR model: {}", plan.asr_model),
    ];
    if let Some(mt) = &plan.mt_model {
        lines.push(format!("NOTE Translation model: {mt}"));
    }
    lines.push(format!(
        "NOTE Pipeline: centre-channel={} forced-alignment={} diarization={}",
        plan.use_center_channel, plan.forced_alignment, plan.diarize
    ));
    lines.push(format!("NOTE Generated: {generated_at}"));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(i: i64, text: &str) -> Cue {
        Cue::new(i * 3000, i * 3000 + 2000, text)
    }

    fn good(n: usize) -> (Vec<Cue>, Vec<CueEvidence>) {
        let cues: Vec<Cue> = (0..n).map(|i| cue(i as i64, &format!("Line number {i}."))).collect();
        let ev = vec![CueEvidence::with_speech(0.9); n];
        (cues, ev)
    }

    #[test]
    fn clean_output_is_accepted() {
        let (cues, ev) = good(20);
        let v = gate_generated(&cues, &ev, None, None);
        assert!(matches!(v, GenerationVerdict::Accept { .. }), "{v:?}");
        assert!(v.is_attachable());
    }

    #[test]
    fn hallucination_on_silence_is_detected() {
        // Whisper's signature failure: confident, plausible text over music or room tone.
        let (mut cues, mut ev) = good(20);
        cues.push(cue(20, "Subtitles by the community."));
        ev.push(CueEvidence::without_speech(0.95));
        let v = gate_generated(&cues, &ev, None, None);
        assert!(
            v.issues().iter().any(|i| matches!(i, GenerationIssue::HallucinationOnSilence { .. })),
            "{:?}",
            v.issues()
        );
        assert!(v.is_attachable(), "one hallucination in 21 cues is a draft, not a rejection");
    }

    #[test]
    fn many_hallucinations_reject_the_whole_track() {
        let mut cues = Vec::new();
        let mut ev = Vec::new();
        for i in 0..20 {
            cues.push(cue(i, "Thanks for watching."));
            ev.push(CueEvidence::without_speech(0.9));
        }
        let v = gate_generated(&cues, &ev, None, None);
        assert!(matches!(v, GenerationVerdict::Reject { .. }), "{v:?}");
        assert!(!v.is_attachable());
    }

    #[test]
    fn repetition_loops_are_disqualifying() {
        // A stuck decode. The output looks like dialogue but is a decoder artefact.
        let mut cues = Vec::new();
        let mut ev = Vec::new();
        for i in 0..6 {
            cues.push(cue(i, "I don't know what to do."));
            ev.push(CueEvidence::with_speech(0.85));
        }
        let v = gate_generated(&cues, &ev, None, None);
        assert!(
            v.issues().iter().any(|i| matches!(i, GenerationIssue::RepetitionLoop { .. })),
            "{:?}",
            v.issues()
        );
        assert!(matches!(v, GenerationVerdict::Reject { .. }));
    }

    #[test]
    fn genuine_short_repetition_is_not_a_loop() {
        // "No. No." is real dialogue. The threshold of 4 exists to allow emphasis.
        let cues = vec![cue(0, "No."), cue(1, "No."), cue(2, "Please."), cue(3, "Stop.")];
        let ev = vec![CueEvidence::with_speech(0.9); 4];
        let v = gate_generated(&cues, &ev, None, None);
        assert!(!v.issues().iter().any(|i| matches!(i, GenerationIssue::RepetitionLoop { .. })));
    }

    #[test]
    fn repetition_detection_ignores_punctuation_and_case() {
        let cues = vec![
            cue(0, "Hello there"),
            cue(1, "hello, there!"),
            cue(2, "HELLO THERE."),
            cue(3, "Hello  there"),
        ];
        let ev = vec![CueEvidence::with_speech(0.9); 4];
        let v = gate_generated(&cues, &ev, None, None);
        assert!(v.issues().iter().any(|i| matches!(i, GenerationIssue::RepetitionLoop { .. })));
    }

    #[test]
    fn runaway_translations_are_flagged() {
        let cues = vec![cue(0, &"very long output ".repeat(20))];
        let ev = vec![CueEvidence { source_chars: Some(10), ..CueEvidence::with_speech(0.9) }];
        let v = gate_generated(&cues, &ev, None, None);
        assert!(
            v.issues().iter().any(|i| matches!(i, GenerationIssue::RunawayLength { .. })),
            "{:?}",
            v.issues()
        );
    }

    #[test]
    fn a_normal_length_difference_between_languages_is_not_a_runaway() {
        // German is legitimately longer than English. The threshold must not punish that.
        let cues = vec![cue(0, "Das ist ein ganz normaler deutscher Satz.")];
        let ev = vec![CueEvidence { source_chars: Some(28), ..CueEvidence::with_speech(0.9) }];
        let v = gate_generated(&cues, &ev, None, None);
        assert!(!v.issues().iter().any(|i| matches!(i, GenerationIssue::RunawayLength { .. })));
    }

    #[test]
    fn low_mean_confidence_rejects_but_one_weak_cue_does_not() {
        let (cues, _) = good(10);
        let weak_track = vec![CueEvidence::with_speech(0.3); 10];
        let v = gate_generated(&cues, &weak_track, None, None);
        assert!(matches!(v, GenerationVerdict::Reject { .. }), "{v:?}");

        let mut mostly_good = vec![CueEvidence::with_speech(0.95); 10];
        mostly_good[3] = CueEvidence::with_speech(0.2);
        let v2 = gate_generated(&cues, &mostly_good, None, None);
        assert!(v2.is_attachable(), "one weak cue is a note, not a rejection");
        assert!(v2.issues().iter().any(|i| matches!(i, GenerationIssue::LowConfidence { .. })));
    }

    #[test]
    fn language_disagreement_is_surfaced_not_resolved() {
        // Container tags are wrong often enough to matter, and so is audio detection. The user is
        // better placed to say which one is lying.
        let (cues, ev) = good(5);
        let v = gate_generated(&cues, &ev, Some(&LangTag::new("ko")), Some(&LangTag::new("ja")));
        assert!(
            v.issues().iter().any(|i| matches!(i, GenerationIssue::LanguageMismatch { .. })),
            "{:?}",
            v.issues()
        );
    }

    #[test]
    fn dialect_differences_are_not_a_language_mismatch() {
        let (cues, ev) = good(5);
        let v =
            gate_generated(&cues, &ev, Some(&LangTag::new("pt-BR")), Some(&LangTag::new("pt-PT")));
        assert!(!v.issues().iter().any(|i| matches!(i, GenerationIssue::LanguageMismatch { .. })));
    }

    #[test]
    fn missing_evidence_is_treated_as_unknown_not_as_bad() {
        // A generator that reports no confidence must not have its output rejected for it.
        let (cues, _) = good(10);
        let v = gate_generated(&cues, &[], None, None);
        assert!(matches!(v, GenerationVerdict::Accept { .. }), "{v:?}");
    }

    #[test]
    fn an_empty_track_is_not_rejected_by_a_divide_by_zero() {
        let v = gate_generated(&[], &[], None, None);
        assert!(matches!(v, GenerationVerdict::Accept { .. }));
    }

    #[test]
    fn the_plan_only_translates_when_languages_differ() {
        let same = AsrPlan::quality(&LangTag::new("en"), &LangTag::new("eng"));
        assert!(!same.is_translating(), "same language needs no translation stage");
        let cross = AsrPlan::quality(&LangTag::new("en"), &LangTag::new("ja"));
        assert!(cross.is_translating());
        assert_eq!(cross.asr_model, "large-v3");
    }

    #[test]
    fn quality_defaults_enable_everything_that_improves_accuracy() {
        // The alternative to a good generated subtitle is usually no subtitle, so accuracy wins over
        // speed by default.
        let p = AsrPlan::quality(&LangTag::new("en"), &LangTag::new("ja"));
        assert!(p.use_center_channel, "dialogue lives in the centre channel");
        assert!(p.forced_alignment, "Whisper segment timestamps are too coarse for subtitles");
        assert!(p.diarize);
        assert!(p.detect_language, "container tags are unreliable");

        let f = AsrPlan::fast(&LangTag::new("en"), &LangTag::new("ja"));
        assert!(!f.diarize, "the fast path drops the expensive stage");
        assert!(f.use_center_channel, "but keeps the free accuracy win");
    }

    #[test]
    fn the_provenance_header_records_everything_needed_to_reproduce_it() {
        let plan = AsrPlan::quality(&LangTag::new("en"), &LangTag::new("ja"));
        let header = provenance_header(&plan, "audio track 1 (ja)", "2026-07-27T12:00:00Z");
        let joined = header.join("\n");
        assert!(joined.contains("generated automatically"), "{joined}");
        assert!(joined.contains("large-v3"));
        assert!(joined.contains("nllb-200"));
        assert!(joined.contains("2026-07-27"));
        assert!(joined.contains("audio track 1"));
    }
}
