//! Subtitle acquisition — `docs/14` §3.
//!
//! The request is always the same shape: *give me subtitles I can read for this file*. The ladder here
//! spends the least effort that satisfies it, and every rung is labelled so a viewer always knows
//! whether they are reading a human translation or a machine one.
//!
//! The load-bearing ordering decision is that **translating an existing subtitle beats transcribing the
//! audio**. If a film has Spanish subtitles but no English ones, translating the Spanish text removes
//! an entire error stage:
//!
//! - Translate: `human transcription → machine translation` — one lossy step, and it inherits human
//!   timing, which is worth as much as the wording.
//! - Transcribe: `machine transcription → machine translation` — two lossy steps, where the second
//!   compounds the first. Whisper large-v3 runs around 2.7% WER on clean benchmarks but 8–12% in
//!   real-world conditions, and film audio — music beds, overlapping dialogue, whispering — is firmly
//!   real-world. A 10% word error rate feeding a translator produces confidently wrong sentences, and
//!   a viewer cannot tell a mistranslation from the script.

#![forbid(unsafe_code)]

pub mod generate;
pub mod readability;
pub mod sync;

pub use generate::{AsrPlan, GenerationIssue, GenerationVerdict, gate_generated};
pub use readability::{Cue, Issue, QualityReport, ReadabilityProfile, check_readability};
pub use sync::{SyncCorrection, detect_correction};

/// A language tag, re-exported so callers need only this crate.
pub use lumen_meta_lang::LangTag;

/// Minimal local mirror of the language type, kept deliberately separate from both `lumen_meta::LangTag`
/// (this crate should not depend on `lumen-meta` purely for a tag type) and `lumen_model::Language`
/// (which looks like the obvious shared type, but is not a safe substitute here: it normalises casing
/// only and leaves the raw subtag alone — `Language::new("jpn").as_str() == "jpn"` — where this type
/// additionally folds ISO 639-2 codes onto their ISO 639-1 equivalent, `LangTag::new("jpn").as_str() ==
/// "ja"`. That fold is load-bearing: an embedded Matroska audio track tagged `jpn` (639-2, what
/// Matroska's own `Language` element carries) has to compare equal to a provider or request tag written
/// `ja` (BCP-47), or `acquisition_step`'s forced-track and dub/sub matching silently fails on every file
/// whose muxer used the three-letter form). Unifying the two types would mean either giving every other
/// `lumen_model::Language` consumer this crate's mapping table, or duplicating it there instead of here
/// — this file is the smaller, more contained place for it to live.
mod lumen_meta_lang {
    /// BCP-47-ish tag, normalised for comparison. Mirrors `lumen_meta::LangTag`.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct LangTag(String);

    impl LangTag {
        pub fn new(tag: &str) -> Self {
            let lower = tag.trim().to_ascii_lowercase().replace('_', "-");
            if lower.is_empty() || matches!(lower.as_str(), "und" | "mis" | "zxx" | "mul") {
                return Self("und".into());
            }
            let mut parts = lower.splitn(2, '-');
            let primary = match parts.next().unwrap_or("") {
                "eng" => "en",
                "jpn" => "ja",
                "fra" | "fre" => "fr",
                "deu" | "ger" => "de",
                "spa" => "es",
                "ita" => "it",
                "por" => "pt",
                "rus" => "ru",
                "zho" | "chi" => "zh",
                "kor" => "ko",
                other => other,
            };
            match parts.next() {
                Some(r) if !r.is_empty() => Self(format!("{primary}-{r}")),
                _ => Self(primary.to_string()),
            }
        }

        pub fn as_str(&self) -> &str {
            &self.0
        }

        pub fn base(&self) -> &str {
            self.0.split('-').next().unwrap_or(&self.0)
        }

        pub fn is_undetermined(&self) -> bool {
            self.0 == "und"
        }

        pub fn exact_eq(&self, other: &LangTag) -> bool {
            !self.is_undetermined() && self.0 == other.0
        }

        pub fn base_eq(&self, other: &LangTag) -> bool {
            !self.is_undetermined() && !other.is_undetermined() && self.base() == other.base()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubtitleFormat {
    SubRip,
    Ass,
    WebVtt,
    Ttml,
    /// Bitmap. Cannot be restyled and cannot enter MP4 (`docs/13` §1.1); OCR is the route to text.
    Pgs,
    VobSub,
    /// Carried inside the video elementary stream.
    Cea608,
    Cea708,
}

impl SubtitleFormat {
    pub fn is_bitmap(self) -> bool {
        matches!(self, Self::Pgs | Self::VobSub)
    }

    pub fn is_in_video(self) -> bool {
        matches!(self, Self::Cea608 | Self::Cea708)
    }
}

/// Where a subtitle came from. Determines both its ranking and — non-negotiably — its label.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubtitleOrigin {
    /// A track inside the container.
    Embedded,
    /// A sidecar file beside the media.
    Sidecar,
    /// Downloaded from a subtitle provider.
    Provider { id: String },
    /// CEA-608/708 lifted out of the video stream. Often the *only* captions on a broadcast recording.
    ExtractedCaptions,
    /// OCR of a bitmap track: human wording, machine reading, so OCR errors are possible.
    Ocr { engine: String },
    /// Machine translation of a human-authored subtitle in another language. Rung 6.
    MachineTranslated { from: LangTag, model: String },
    /// ASR of the audio, same language as the speech. Rung 7a.
    MachineTranscribed { model: String },
    /// ASR then machine translation. Rung 7b, and the weakest rung — two compounding lossy stages.
    MachineTranscribedAndTranslated { from: LangTag, asr_model: String, mt_model: String },
}

impl SubtitleOrigin {
    /// True when a machine produced the *words*. OCR is excluded: the wording is human, only the
    /// reading of it is mechanical.
    pub fn is_machine_generated(&self) -> bool {
        matches!(
            self,
            Self::MachineTranslated { .. }
                | Self::MachineTranscribed { .. }
                | Self::MachineTranscribedAndTranslated { .. }
        )
    }

    /// Fidelity rank, higher is better. This is what makes a human track always beat a generated one
    /// in the same language, and what lets a later human subtitle supersede a generated one
    /// automatically when it appears (`docs/14` §3.3).
    pub fn fidelity(&self) -> u8 {
        match self {
            Self::Embedded => 100,
            Self::Sidecar => 95,
            Self::Provider { .. } => 90,
            Self::ExtractedCaptions => 85,
            Self::Ocr { .. } => 60,
            Self::MachineTranslated { .. } => 40,
            Self::MachineTranscribed { .. } => 30,
            Self::MachineTranscribedAndTranslated { .. } => 20,
        }
    }

    /// Suffix appended to the track title. Machine-generated tracks are labelled permanently, because
    /// confidently wrong subtitles are worse than none — a viewer cannot tell a mistranslation from
    /// the script and will believe it.
    pub fn label_suffix(&self) -> Option<&'static str> {
        match self {
            Self::MachineTranslated { .. } => Some("machine translated"),
            Self::MachineTranscribed { .. } => Some("auto-transcribed"),
            Self::MachineTranscribedAndTranslated { .. } => Some("auto-transcribed & translated"),
            Self::Ocr { .. } => Some("OCR"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleTrack {
    pub language: LangTag,
    pub origin: SubtitleOrigin,
    pub format: SubtitleFormat,
    /// Foreign-dialogue-only. Not a substitute for a full track when the audio is foreign.
    pub forced: bool,
    /// Includes speaker IDs and non-speech sound. A closed caption, not a subtitle (`docs/14` §6).
    pub sdh: bool,
    pub rating: Option<f32>,
    pub download_count: u32,
}

impl SubtitleTrack {
    pub fn new(language: &str, origin: SubtitleOrigin, format: SubtitleFormat) -> Self {
        Self {
            language: LangTag::new(language),
            origin,
            format,
            forced: false,
            sdh: false,
            rating: None,
            download_count: 0,
        }
    }

    /// The user-facing track name, with the machine-generated label baked in.
    pub fn display_label(&self, language_name: &str) -> String {
        let mut parts = Vec::new();
        if self.forced {
            parts.push("forced".to_string());
        }
        if self.sdh {
            parts.push("SDH".to_string());
        }
        if let Some(s) = self.origin.label_suffix() {
            parts.push(s.to_string());
        }
        if parts.is_empty() {
            language_name.to_string()
        } else {
            format!("{language_name} ({})", parts.join(", "))
        }
    }
}

/// What the user wants.
#[derive(Debug, Clone)]
pub struct SubtitleRequest {
    /// Ordered preference chain.
    pub wanted: Vec<LangTag>,
    /// Language of the audio actually selected, which decides whether a forced track suffices.
    pub audio_language: LangTag,
    pub prefer_sdh: bool,
    /// Permit rungs 6 and 7. Off by default: generating subtitles costs GPU-minutes and produces
    /// output a user may not want at all.
    pub allow_generation: bool,
    /// Permit OCR of bitmap tracks.
    pub allow_ocr: bool,
}

impl SubtitleRequest {
    pub fn new(wanted: &[&str], audio_language: &str) -> Self {
        Self {
            wanted: wanted.iter().map(|s| LangTag::new(s)).collect(),
            audio_language: LangTag::new(audio_language),
            prefer_sdh: false,
            allow_generation: false,
            allow_ocr: false,
        }
    }

    pub fn allowing_generation(mut self) -> Self {
        self.allow_generation = true;
        self
    }

    pub fn allowing_ocr(mut self) -> Self {
        self.allow_ocr = true;
        self
    }

    /// True when the audio is already in a language the user reads, so only foreign-dialogue subtitles
    /// are wanted (`docs/12` §4 forced-subtitle rule).
    pub fn audio_is_understood(&self) -> bool {
        self.wanted.iter().any(|w| w.base_eq(&self.audio_language))
    }
}

/// One rung of the ladder, as an action to take.
#[derive(Debug, Clone, PartialEq)]
pub enum AcquisitionStep {
    /// Rungs 0–1: a track we already have. Index into the `available` slice.
    UseExisting(usize),
    /// Rungs 2–3: ask providers. `allow_dialect` distinguishes the exact-tag pass from the fallback.
    SearchProviders { language: LangTag, allow_dialect: bool },
    /// Rung 4: lift CEA-608/708 out of the video stream.
    ExtractCaptions,
    /// Rung 5: OCR a bitmap track to text.
    OcrBitmap { index: usize },
    /// Rung 6: translate an existing human-authored subtitle. Always attempted before rung 7.
    TranslateExisting { index: usize, from: LangTag, to: LangTag },
    /// Rung 7: transcribe the audio, then translate if needed.
    TranscribeAndTranslate { audio_language: LangTag, to: LangTag },
    /// Nothing is available. Said plainly rather than left as an empty list.
    Unavailable,
}

impl AcquisitionStep {
    /// Rung number from `docs/14` §3, for the diagnostics view and for the tests.
    pub fn rung(&self) -> u8 {
        match self {
            Self::UseExisting(_) => 0,
            Self::SearchProviders { allow_dialect: false, .. } => 2,
            Self::SearchProviders { allow_dialect: true, .. } => 3,
            Self::ExtractCaptions => 4,
            Self::OcrBitmap { .. } => 5,
            Self::TranslateExisting { .. } => 6,
            Self::TranscribeAndTranslate { .. } => 7,
            Self::Unavailable => 255,
        }
    }

    pub fn is_generative(&self) -> bool {
        matches!(self, Self::TranslateExisting { .. } | Self::TranscribeAndTranslate { .. })
    }
}

/// The ordered plan. The caller walks it until one step yields a usable subtitle.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionPlan {
    pub steps: Vec<AcquisitionStep>,
}

impl AcquisitionPlan {
    pub fn first(&self) -> &AcquisitionStep {
        self.steps.first().unwrap_or(&AcquisitionStep::Unavailable)
    }

    pub fn is_unavailable(&self) -> bool {
        self.steps.iter().all(|s| *s == AcquisitionStep::Unavailable)
    }
}

/// Rank an existing track for a request. Higher is better; `None` means unusable.
fn rank_existing(
    track: &SubtitleTrack,
    req: &SubtitleRequest,
    index: usize,
) -> Option<(u32, usize)> {
    // Language must be one the user asked for. `und` never satisfies a preference.
    let lang_rank =
        req.wanted.iter().position(|w| track.language.exact_eq(w)).map(|p| (p, 2u32)).or_else(
            || req.wanted.iter().position(|w| track.language.base_eq(w)).map(|p| (p, 1u32)),
        )?;
    let (chain_pos, exactness) = lang_rank;

    // A forced track only suffices when the audio is already understood; when the audio is foreign the
    // user needs the whole dialogue, and a signs-only track is not a substitute.
    if track.forced && !req.audio_is_understood() {
        return None;
    }
    // Conversely, when the audio *is* understood a full track is noise the user must turn off, so the
    // forced one is preferred.
    let forced_bonus = u32::from(track.forced == req.audio_is_understood());
    let sdh_bonus = u32::from(track.sdh == req.prefer_sdh);
    let chain_score = u32::try_from(req.wanted.len().saturating_sub(chain_pos)).unwrap_or(0);

    let score = chain_score * 1_000_000
        + exactness * 100_000
        + forced_bonus * 50_000
        + sdh_bonus * 20_000
        + u32::from(track.origin.fidelity()) * 100
        + u32::from(!track.format.is_bitmap()) * 10;
    Some((score, index))
}

/// Build the acquisition plan.
///
/// `available` is everything already on hand: embedded tracks, sidecars, and previously-downloaded
/// files. `has_in_video_captions` comes from the probe.
pub fn plan_acquisition(
    available: &[SubtitleTrack],
    req: &SubtitleRequest,
    has_in_video_captions: bool,
) -> AcquisitionPlan {
    let mut steps = Vec::new();

    // Rungs 0–1: usable tracks we already have, best first.
    let mut existing: Vec<(u32, usize)> = available
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.format.is_bitmap() || req.allow_ocr)
        .filter_map(|(i, t)| rank_existing(t, req, i))
        .collect();
    existing.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in &existing {
        if available[*i].format.is_bitmap() {
            steps.push(AcquisitionStep::OcrBitmap { index: *i });
        } else {
            steps.push(AcquisitionStep::UseExisting(*i));
        }
    }

    // Rungs 2–3: providers, exact tag before dialect, in preference order.
    for want in &req.wanted {
        steps.push(AcquisitionStep::SearchProviders {
            language: want.clone(),
            allow_dialect: false,
        });
    }
    for want in &req.wanted {
        steps
            .push(AcquisitionStep::SearchProviders { language: want.clone(), allow_dialect: true });
    }

    // Rung 4: in-video captions. Cheap, and frequently the only captions a broadcast recording has.
    if has_in_video_captions {
        steps.push(AcquisitionStep::ExtractCaptions);
    }

    if req.allow_generation {
        let target = req.wanted.first().cloned().unwrap_or_else(|| LangTag::new("en"));

        // Rung 6 before rung 7. The whole point: translating human-authored text removes an error
        // stage and inherits human timing. Prefer the highest-fidelity, non-bitmap source available.
        let mut sources: Vec<(u8, usize)> = available
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.format.is_bitmap() && !t.forced)
            .filter(|(_, t)| !t.origin.is_machine_generated())
            .filter(|(_, t)| !t.language.is_undetermined() && !t.language.base_eq(&target))
            .map(|(i, t)| (t.origin.fidelity(), i))
            .collect();
        sources.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, i) in sources {
            steps.push(AcquisitionStep::TranslateExisting {
                index: i,
                from: available[i].language.clone(),
                to: target.clone(),
            });
        }

        // Rung 7: last resort.
        steps.push(AcquisitionStep::TranscribeAndTranslate {
            audio_language: req.audio_language.clone(),
            to: target,
        });
    }

    if steps.is_empty() {
        steps.push(AcquisitionStep::Unavailable);
    }
    AcquisitionPlan { steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SubtitleFormat as F;
    use SubtitleOrigin as O;

    fn track(lang: &str, origin: O, format: F) -> SubtitleTrack {
        SubtitleTrack::new(lang, origin, format)
    }

    #[test]
    fn an_existing_embedded_track_is_used_before_anything_else() {
        let available = vec![track("en", O::Embedded, F::Ass)];
        let plan = plan_acquisition(&available, &SubtitleRequest::new(&["en"], "ja"), false);
        assert_eq!(*plan.first(), AcquisitionStep::UseExisting(0));
        assert_eq!(plan.first().rung(), 0);
    }

    #[test]
    fn translating_an_existing_subtitle_comes_before_transcribing_audio() {
        // The load-bearing ordering decision from docs/14 §3.1. Translating human text is one lossy
        // step; transcribing then translating is two, and the second compounds the first.
        let available = vec![track("es", O::Embedded, F::SubRip)];
        let req = SubtitleRequest::new(&["en"], "ja").allowing_generation();
        let plan = plan_acquisition(&available, &req, false);

        let translate =
            plan.steps.iter().position(|s| matches!(s, AcquisitionStep::TranslateExisting { .. }));
        let transcribe = plan
            .steps
            .iter()
            .position(|s| matches!(s, AcquisitionStep::TranscribeAndTranslate { .. }));
        assert!(translate.is_some(), "a Spanish track should be a translation source");
        assert!(translate < transcribe, "translate must precede transcribe: {:?}", plan.steps);
    }

    #[test]
    fn generation_is_off_unless_asked_for() {
        // Generating costs GPU-minutes and produces output a user may not want at all.
        let plan = plan_acquisition(&[], &SubtitleRequest::new(&["en"], "ja"), false);
        assert!(!plan.steps.iter().any(AcquisitionStep::is_generative));
    }

    #[test]
    fn a_machine_generated_track_is_never_a_translation_source() {
        // Translating a machine translation stacks three lossy stages for no benefit.
        let available = vec![track(
            "es",
            O::MachineTranscribedAndTranslated {
                from: LangTag::new("ja"),
                asr_model: "large-v3".into(),
                mt_model: "nllb".into(),
            },
            F::SubRip,
        )];
        let req = SubtitleRequest::new(&["en"], "ja").allowing_generation();
        let plan = plan_acquisition(&available, &req, false);
        assert!(!plan.steps.iter().any(|s| matches!(s, AcquisitionStep::TranslateExisting { .. })));
    }

    #[test]
    fn a_higher_fidelity_translation_source_is_preferred() {
        let available = vec![
            track("es", O::Ocr { engine: "tesseract".into() }, F::SubRip),
            track("fr", O::Embedded, F::SubRip),
        ];
        let req = SubtitleRequest::new(&["en"], "ja").allowing_generation();
        let plan = plan_acquisition(&available, &req, false);
        let first_translate = plan
            .steps
            .iter()
            .find_map(|s| match s {
                AcquisitionStep::TranslateExisting { from, .. } => Some(from.clone()),
                _ => None,
            })
            .expect("a translation source");
        assert_eq!(first_translate.as_str(), "fr", "embedded beats OCR as a source");
    }

    #[test]
    fn a_forced_track_does_not_satisfy_a_foreign_audio_request() {
        // Japanese audio, English forced-only track: the user needs the whole dialogue, and a
        // signs-only track would leave them unable to follow the film.
        let mut forced = track("en", O::Embedded, F::SubRip);
        forced.forced = true;
        let plan = plan_acquisition(&[forced], &SubtitleRequest::new(&["en"], "ja"), false);
        assert!(
            !matches!(plan.first(), AcquisitionStep::UseExisting(_)),
            "forced-only must not satisfy foreign audio: {:?}",
            plan.first()
        );
    }

    #[test]
    fn a_forced_track_is_preferred_when_the_audio_is_understood() {
        // The inverse: English audio, so only the foreign-dialogue subtitles are wanted.
        let mut forced = track("en", O::Embedded, F::SubRip);
        forced.forced = true;
        let full = track("en", O::Embedded, F::SubRip);
        let available = vec![full, forced];
        let plan = plan_acquisition(&available, &SubtitleRequest::new(&["en"], "en"), false);
        assert_eq!(*plan.first(), AcquisitionStep::UseExisting(1), "forced track wins");
    }

    #[test]
    fn bitmap_tracks_need_ocr_permission() {
        let available = vec![track("en", O::Embedded, F::Pgs)];
        let without = plan_acquisition(&available, &SubtitleRequest::new(&["en"], "ja"), false);
        assert!(!without.steps.iter().any(|s| matches!(s, AcquisitionStep::OcrBitmap { .. })));

        let with = plan_acquisition(
            &available,
            &SubtitleRequest::new(&["en"], "ja").allowing_ocr(),
            false,
        );
        assert_eq!(*with.first(), AcquisitionStep::OcrBitmap { index: 0 });
    }

    #[test]
    fn in_video_captions_are_offered_when_present() {
        // Frequently the only captions on a US broadcast recording (docs/11 §5).
        let plan = plan_acquisition(&[], &SubtitleRequest::new(&["en"], "en"), true);
        assert!(plan.steps.contains(&AcquisitionStep::ExtractCaptions));
    }

    #[test]
    fn exact_provider_search_precedes_dialect_search() {
        let plan = plan_acquisition(&[], &SubtitleRequest::new(&["pt-BR"], "ja"), false);
        let exact = plan.steps.iter().position(|s| s.rung() == 2);
        let dialect = plan.steps.iter().position(|s| s.rung() == 3);
        assert!(exact < dialect, "{:?}", plan.steps);
    }

    #[test]
    fn nothing_available_says_so_rather_than_returning_an_empty_plan() {
        let plan = plan_acquisition(
            &[],
            &SubtitleRequest { wanted: vec![], ..SubtitleRequest::new(&[], "ja") },
            false,
        );
        assert!(plan.is_unavailable());
        assert_eq!(*plan.first(), AcquisitionStep::Unavailable);
    }

    #[test]
    fn machine_generated_origins_are_labelled_and_human_ones_are_not() {
        // docs/14 §3.3: permanent labelling. Confidently wrong subtitles are worse than none.
        for origin in [
            O::MachineTranslated { from: LangTag::new("es"), model: "nllb".into() },
            O::MachineTranscribed { model: "large-v3".into() },
            O::MachineTranscribedAndTranslated {
                from: LangTag::new("ja"),
                asr_model: "large-v3".into(),
                mt_model: "nllb".into(),
            },
        ] {
            assert!(origin.is_machine_generated(), "{origin:?}");
            assert!(origin.label_suffix().is_some(), "{origin:?} must be labelled");
        }
        for origin in
            [O::Embedded, O::Sidecar, O::Provider { id: "os".into() }, O::ExtractedCaptions]
        {
            assert!(!origin.is_machine_generated(), "{origin:?}");
            assert_eq!(origin.label_suffix(), None, "{origin:?} needs no caveat");
        }
    }

    #[test]
    fn ocr_is_not_machine_generated_but_is_still_flagged() {
        // The wording is human; only the reading of it is mechanical. Worth a caveat, not a warning.
        let ocr = O::Ocr { engine: "tesseract".into() };
        assert!(!ocr.is_machine_generated());
        assert_eq!(ocr.label_suffix(), Some("OCR"));
    }

    #[test]
    fn a_human_track_always_outranks_a_generated_one_in_the_same_language() {
        // And this is what lets a later human subtitle supersede a generated one automatically.
        assert!(
            O::Provider { id: "os".into() }.fidelity()
                > O::MachineTranslated { from: LangTag::new("es"), model: "m".into() }.fidelity()
        );
        assert!(
            O::MachineTranslated { from: LangTag::new("es"), model: "m".into() }.fidelity()
                > O::MachineTranscribedAndTranslated {
                    from: LangTag::new("ja"),
                    asr_model: "a".into(),
                    mt_model: "m".into()
                }
                .fidelity(),
            "one lossy stage beats two"
        );
    }

    #[test]
    fn display_labels_carry_every_caveat() {
        let mut t = track("en", O::MachineTranscribed { model: "large-v3".into() }, F::SubRip);
        t.sdh = true;
        let label = t.display_label("English");
        assert!(label.contains("English"));
        assert!(label.contains("SDH"));
        assert!(label.contains("auto-transcribed"), "{label}");

        let plain = track("en", O::Embedded, F::SubRip);
        assert_eq!(plain.display_label("English"), "English");
    }

    #[test]
    fn undetermined_language_never_satisfies_a_preference() {
        let plan = plan_acquisition(
            &[track("und", O::Embedded, F::SubRip)],
            &SubtitleRequest::new(&["en"], "ja"),
            false,
        );
        assert!(!matches!(plan.first(), AcquisitionStep::UseExisting(_)));
    }
}
