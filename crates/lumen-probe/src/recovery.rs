//! The universal recovery ladder — `docs/12` §5.
//!
//! Guarantee **G2** (`docs/11` §1) says the player never shows "unsupported format". This is the
//! machinery that makes that true: an ordered escalation applied on open and continuously during
//! playback, where each rung trades a little more work for a little more tolerance.
//!
//! Two invariants, asserted by the tests:
//!
//! 1. **Escalation is monotonic and finite.** The ladder always moves forward and always terminates.
//!    A ladder that could loop would hang on a corrupt file, which is worse than refusing it.
//! 2. **Only the causes in `docs/11` §7 may exhaust it.** Reaching the end without playing must be
//!    attributable to DRM, an absent decoder, insufficient hardware, or genuinely no decodable data —
//!    never to a container quirk.
//!
//! The rung reached is surfaced as [`crate::ebml`]-style provenance in the Playback Report, and the
//! conformance corpus asserts on it via `recovery_rung`. That matters because a file that still plays
//! but only via rung 4 has silently lost its fast path, and without the assertion nobody notices.

/// Rungs, in escalation order. The discriminants are the `recovery_rung` values used in
/// `conformance/corpus.yaml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Rung {
    /// Normal probe and open.
    Normal = 0,
    /// Re-probe with a much larger `probesize`/`analyzeduration`.
    EscalatedProbe = 1,
    /// Tolerant demux flags: generate timestamps, ignore DTS, discard corrupt packets, ignore index.
    TolerantFlags = 2,
    /// Try the ranked alternative demuxers for the detected magic bytes.
    ForcedDemuxer = 3,
    /// Rebuild the index: scan `mdat` for codec sync patterns, or synthesise `Cues` from clusters.
    IndexReconstruction = 4,
    /// Probe the payload as a headerless elementary stream with generated timestamps.
    RawElementaryStream = 5,
    /// Derive codec parameters from the first frames when extradata is absent.
    ParameterInference = 6,
    /// Conceal per-packet decode errors and continue; skip to the next keyframe on repeats.
    ErrorConcealment = 7,
    /// Recreate the decoder, then fall back from hardware to software at the next keyframe.
    DecoderEscalation = 8,
    /// Drop an unrecoverable track and keep the rest. Audio-only beats nothing.
    StreamIsolation = 9,
    /// Last resort: pipe through a repair pipeline to a temporary playable stream.
    RepairTranscode = 10,
}

impl Rung {
    pub const ALL: [Rung; 11] = [
        Rung::Normal,
        Rung::EscalatedProbe,
        Rung::TolerantFlags,
        Rung::ForcedDemuxer,
        Rung::IndexReconstruction,
        Rung::RawElementaryStream,
        Rung::ParameterInference,
        Rung::ErrorConcealment,
        Rung::DecoderEscalation,
        Rung::StreamIsolation,
        Rung::RepairTranscode,
    ];

    pub fn level(self) -> u8 {
        self as u8
    }

    fn next(self) -> Option<Rung> {
        Rung::ALL.get(self.level() as usize + 1).copied()
    }

    /// Rungs above `TolerantFlags` mean the fast path was lost. Worth surfacing, because a file that
    /// plays only via index reconstruction will be slow to open and slow to seek.
    pub fn is_degraded_open(self) -> bool {
        self > Rung::TolerantFlags
    }

    /// True when reaching this rung implies content may be missing or unverified, which caps the
    /// playback tier at T4 (`docs/11` §1.1).
    pub fn implies_content_loss(self) -> bool {
        matches!(
            self,
            Rung::IndexReconstruction
                | Rung::RawElementaryStream
                | Rung::ErrorConcealment
                | Rung::StreamIsolation
                | Rung::RepairTranscode
        )
    }

    pub fn describe(self) -> &'static str {
        match self {
            Rung::Normal => "opened normally",
            Rung::EscalatedProbe => "needed a deeper probe to identify its streams",
            Rung::TolerantFlags => {
                "needed tolerant parsing; its index or timestamps are unreliable"
            }
            Rung::ForcedDemuxer => "was mislabelled; opened with a different reader",
            Rung::IndexReconstruction => {
                "has no usable index; one was rebuilt by scanning the file"
            }
            Rung::RawElementaryStream => "has no container structure; played as a raw stream",
            Rung::ParameterInference => {
                "declares no codec parameters; they were read from the picture"
            }
            Rung::ErrorConcealment => "contains corrupt data; damaged frames were concealed",
            Rung::DecoderEscalation => "broke the hardware decoder; switched to software",
            Rung::StreamIsolation => "has an unrecoverable track; the rest is playing",
            Rung::RepairTranscode => "needed repairing before it would play",
        }
    }
}

/// Why an open or a playback attempt failed. Determines which rung to try next: escalating to a
/// probe rung on a decoder error would waste time, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenFailure {
    /// No demuxer claimed the input.
    ContainerUnrecognised,
    /// A demuxer claimed it but found no streams.
    NoStreamsFound,
    /// Streams found but codec parameters are missing or unusable.
    CodecParametersMissing,
    /// The index is absent, incomplete, or points to the wrong offsets.
    IndexUnusableOrMissing,
    /// The declared structure disagrees with the bytes.
    StructureCorrupt,
    /// Timestamps are absent, non-monotonic, or wildly discontinuous.
    TimestampsUnusable,
    /// The file ends mid-structure.
    Truncated,
    /// A decoder returned an error on a packet.
    DecodeError,
    /// The hardware decoder failed and a software path exists.
    HardwareDecoderFailed,
    /// One track cannot be decoded at all; others can.
    TrackUnrecoverable,
    // ── Terminal causes: the only ones permitted to exhaust the ladder (docs/11 §7) ───────────────
    /// Content protection with no key available.
    DrmProtected,
    /// No decoder exists for the format, in any build.
    NoDecoderExists,
    /// Nothing decodable was found in the buffer at all.
    NoDecodableData,
    /// The device cannot decode it fast enough, or the link cannot deliver it.
    HardwareInsufficient,
}

impl OpenFailure {
    /// Terminal failures cannot be escalated past. Everything else is the player's problem to solve.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::DrmProtected
                | Self::NoDecoderExists
                | Self::NoDecodableData
                | Self::HardwareInsufficient
        )
    }

    /// The lowest rung that could plausibly address this failure. Skipping straight to it avoids
    /// spending seconds on rungs that cannot possibly help — a deeper probe will not fix a decoder
    /// error, and error concealment will not fix a missing index.
    fn first_useful_rung(self) -> Rung {
        match self {
            Self::ContainerUnrecognised => Rung::ForcedDemuxer,
            Self::NoStreamsFound => Rung::EscalatedProbe,
            Self::CodecParametersMissing => Rung::ParameterInference,
            Self::IndexUnusableOrMissing => Rung::TolerantFlags,
            Self::StructureCorrupt => Rung::TolerantFlags,
            Self::TimestampsUnusable => Rung::TolerantFlags,
            Self::Truncated => Rung::TolerantFlags,
            Self::DecodeError => Rung::ErrorConcealment,
            Self::HardwareDecoderFailed => Rung::DecoderEscalation,
            Self::TrackUnrecoverable => Rung::StreamIsolation,
            // Terminal failures have no useful rung; `advance` rejects them before this is consulted.
            Self::DrmProtected
            | Self::NoDecoderExists
            | Self::NoDecodableData
            | Self::HardwareInsufficient => Rung::RepairTranscode,
        }
    }
}

/// Why the ladder stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// A terminal cause from `docs/11` §7. The only legitimate way to reach T5.
    Terminal(OpenFailure),
    /// Every rung was tried. Reaching here on a non-terminal failure is a bug in the ladder, and the
    /// diagnostics bundle plus a one-click structural-sample report exist for exactly this case.
    AllRungsTried,
}

/// Escalation state for one open attempt.
///
/// Tracks which rungs have been tried so escalation is monotonic and finite regardless of how the
/// failures arrive — a decoder that reports `DecodeError` a thousand times must not restart the
/// ladder a thousand times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLadder {
    current: Rung,
    history: Vec<(Rung, OpenFailure)>,
}

impl Default for RecoveryLadder {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryLadder {
    pub fn new() -> Self {
        Self { current: Rung::Normal, history: Vec::new() }
    }

    pub fn current(&self) -> Rung {
        self.current
    }

    /// Every (rung, failure) pair attempted, in order. This is what the Playback Report renders and
    /// what the diagnostics bundle carries.
    pub fn history(&self) -> &[(Rung, OpenFailure)] {
        &self.history
    }

    /// The highest rung reached. Conformance asserts `reached <= recovery_rung`, so an improvement in
    /// the fast path never fails the build.
    pub fn reached(&self) -> Rung {
        self.history.iter().map(|(r, _)| *r).max().unwrap_or(Rung::Normal).max(self.current)
    }

    /// True when any rung implying content loss was used, which caps the playback tier at T4.
    pub fn implies_content_loss(&self) -> bool {
        self.current.implies_content_loss()
            || self.history.iter().any(|(r, _)| r.implies_content_loss())
    }

    /// Record `failure` at the current rung and choose the next rung to try.
    ///
    /// Returns `Err(Exhausted)` when no further escalation is possible. Escalation is strictly
    /// forward: repeated failures cannot move the ladder backwards or make it revisit a rung.
    pub fn advance(&mut self, failure: OpenFailure) -> Result<Rung, Exhausted> {
        self.history.push((self.current, failure));

        if failure.is_terminal() {
            return Err(Exhausted::Terminal(failure));
        }

        // Jump ahead to the first rung that could address this failure, but never backwards — a
        // late decode error must not send us back to re-probing the container.
        let candidate =
            failure.first_useful_rung().max(self.current.next().unwrap_or(Rung::RepairTranscode));
        let next = if candidate > self.current {
            candidate
        } else {
            self.current.next().ok_or(Exhausted::AllRungsTried)?
        };

        if next <= self.current {
            return Err(Exhausted::AllRungsTried);
        }
        self.current = next;
        Ok(next)
    }

    /// A user-facing sentence for the rung actually used, or `None` when the file opened normally.
    ///
    /// Guarantee **G1**: recovery is a departure from a clean open, so the user is told.
    pub fn explain(&self) -> Option<String> {
        let reached = self.reached();
        if reached == Rung::Normal {
            return None;
        }
        Some(format!("This file {}.", reached.describe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rung_levels_match_the_corpus_recovery_rung_values() {
        // conformance/corpus.yaml asserts on these numbers; they are a contract.
        assert_eq!(Rung::Normal.level(), 0);
        assert_eq!(Rung::TolerantFlags.level(), 2);
        assert_eq!(Rung::IndexReconstruction.level(), 4);
        assert_eq!(Rung::RawElementaryStream.level(), 5);
        assert_eq!(Rung::RepairTranscode.level(), 10);
        for (i, r) in Rung::ALL.iter().enumerate() {
            assert_eq!(r.level() as usize, i, "{r:?} out of order");
        }
    }

    #[test]
    fn escalation_is_strictly_monotonic_and_finite() {
        // Invariant 1. A ladder that could revisit a rung would hang on a corrupt file.
        let mut l = RecoveryLadder::new();
        let mut seen = vec![l.current()];
        let mut steps = 0;
        while let Ok(next) = l.advance(OpenFailure::StructureCorrupt) {
            assert!(next > *seen.last().unwrap(), "went backwards: {seen:?} then {next:?}");
            seen.push(next);
            steps += 1;
            assert!(steps <= Rung::ALL.len(), "did not terminate");
        }
        assert_eq!(l.current(), Rung::RepairTranscode, "ends at the last resort");
    }

    #[test]
    fn repeated_identical_failures_do_not_restart_the_ladder() {
        // A decoder reporting DecodeError a thousand times must not reset progress.
        let mut l = RecoveryLadder::new();
        l.advance(OpenFailure::DecodeError).unwrap();
        let after_first = l.current();
        for _ in 0..1000 {
            if let Ok(next) = l.advance(OpenFailure::DecodeError) {
                assert!(next >= after_first);
            }
        }
        assert_eq!(l.current(), Rung::RepairTranscode);
    }

    #[test]
    fn a_late_failure_never_sends_the_ladder_backwards() {
        // Reaching StreamIsolation then hitting a container error must not return to ForcedDemuxer.
        let mut l = RecoveryLadder::new();
        l.advance(OpenFailure::TrackUnrecoverable).unwrap();
        assert_eq!(l.current(), Rung::StreamIsolation);
        let next = l.advance(OpenFailure::ContainerUnrecognised).unwrap();
        assert!(next > Rung::StreamIsolation, "escalated backwards to {next:?}");
    }

    #[test]
    fn each_failure_jumps_to_a_rung_that_can_actually_address_it() {
        // Spending seconds on a deeper probe will not fix a decoder error.
        let cases = [
            (OpenFailure::ContainerUnrecognised, Rung::ForcedDemuxer),
            (OpenFailure::CodecParametersMissing, Rung::ParameterInference),
            (OpenFailure::DecodeError, Rung::ErrorConcealment),
            (OpenFailure::HardwareDecoderFailed, Rung::DecoderEscalation),
            (OpenFailure::TrackUnrecoverable, Rung::StreamIsolation),
        ];
        for (failure, expected) in cases {
            let mut l = RecoveryLadder::new();
            assert_eq!(l.advance(failure).unwrap(), expected, "{failure:?}");
        }
    }

    #[test]
    fn index_problems_start_at_tolerant_flags_not_at_reconstruction() {
        // Ignoring the index is far cheaper than rebuilding it, and usually enough — a Cues-less MKV
        // reaches rung 2, which is what the corpus asserts for `mkv-damage-no-cues`.
        let mut l = RecoveryLadder::new();
        assert_eq!(l.advance(OpenFailure::IndexUnusableOrMissing).unwrap(), Rung::TolerantFlags);
        assert!(!l.implies_content_loss(), "ignoring an index loses no content");
    }

    #[test]
    fn only_the_documented_terminal_causes_can_exhaust_the_ladder() {
        // Invariant 2, and the executable form of docs/11 §7.
        let terminal = [
            OpenFailure::DrmProtected,
            OpenFailure::NoDecoderExists,
            OpenFailure::NoDecodableData,
            OpenFailure::HardwareInsufficient,
        ];
        for f in terminal {
            assert!(f.is_terminal(), "{f:?}");
            let mut l = RecoveryLadder::new();
            assert_eq!(l.advance(f), Err(Exhausted::Terminal(f)));
        }

        let recoverable = [
            OpenFailure::ContainerUnrecognised,
            OpenFailure::NoStreamsFound,
            OpenFailure::CodecParametersMissing,
            OpenFailure::IndexUnusableOrMissing,
            OpenFailure::StructureCorrupt,
            OpenFailure::TimestampsUnusable,
            OpenFailure::Truncated,
            OpenFailure::DecodeError,
            OpenFailure::HardwareDecoderFailed,
            OpenFailure::TrackUnrecoverable,
        ];
        for f in recoverable {
            assert!(!f.is_terminal(), "{f:?} must be recoverable — it is a container/decode quirk");
            let mut l = RecoveryLadder::new();
            assert!(l.advance(f).is_ok(), "{f:?} exhausted the ladder immediately");
        }
    }

    #[test]
    fn drm_is_terminal_at_any_rung_not_just_the_first() {
        let mut l = RecoveryLadder::new();
        l.advance(OpenFailure::StructureCorrupt).unwrap();
        l.advance(OpenFailure::StructureCorrupt).unwrap();
        assert_eq!(
            l.advance(OpenFailure::DrmProtected),
            Err(Exhausted::Terminal(OpenFailure::DrmProtected))
        );
    }

    #[test]
    fn content_loss_is_tracked_so_the_tier_can_be_capped_at_t4() {
        let mut clean = RecoveryLadder::new();
        clean.advance(OpenFailure::IndexUnusableOrMissing).unwrap();
        assert!(!clean.implies_content_loss(), "index recovery preserves all content");

        let mut lossy = RecoveryLadder::new();
        lossy.advance(OpenFailure::DecodeError).unwrap();
        assert!(lossy.implies_content_loss(), "concealed frames are missing content");

        // History matters, not just the current rung: passing through a lossy rung counts.
        let mut passed_through = RecoveryLadder::new();
        passed_through.advance(OpenFailure::DecodeError).unwrap();
        passed_through.advance(OpenFailure::StructureCorrupt).unwrap();
        assert!(passed_through.implies_content_loss());
    }

    #[test]
    fn reached_never_regresses_and_is_what_conformance_asserts_on() {
        let mut l = RecoveryLadder::new();
        assert_eq!(l.reached(), Rung::Normal);
        l.advance(OpenFailure::TrackUnrecoverable).unwrap();
        let high = l.reached();
        assert_eq!(high, Rung::StreamIsolation);
        let _ = l.advance(OpenFailure::StructureCorrupt);
        assert!(l.reached() >= high);
    }

    #[test]
    fn a_clean_open_says_nothing_and_a_recovered_one_explains_itself() {
        // G1 applies to recovery: a degraded open is a departure the user is owed.
        assert_eq!(RecoveryLadder::new().explain(), None);

        let mut l = RecoveryLadder::new();
        l.advance(OpenFailure::IndexUnusableOrMissing).unwrap();
        let msg = l.explain().expect("recovered opens explain themselves");
        assert!(msg.starts_with("This file "), "{msg}");
        assert!(msg.ends_with('.'), "{msg}");
        assert!(msg.len() > 25, "unhelpfully terse: {msg}");
    }

    #[test]
    fn every_rung_has_a_usable_description() {
        for r in Rung::ALL {
            let d = r.describe();
            assert!(d.len() > 10, "{r:?} description too terse: {d}");
            assert!(!d.ends_with('.'), "{r:?} description must compose into a sentence");
        }
    }

    #[test]
    fn history_records_every_attempt_for_the_diagnostics_bundle() {
        let mut l = RecoveryLadder::new();
        l.advance(OpenFailure::NoStreamsFound).unwrap();
        l.advance(OpenFailure::IndexUnusableOrMissing).unwrap();
        assert_eq!(l.history().len(), 2);
        assert_eq!(l.history()[0], (Rung::Normal, OpenFailure::NoStreamsFound));
        assert_eq!(l.history()[1].0, Rung::EscalatedProbe);
    }

    #[test]
    fn degraded_open_flags_the_loss_of_the_fast_path() {
        // A file that plays only via reconstruction is slow to open and slow to seek. Worth saying.
        assert!(!Rung::Normal.is_degraded_open());
        assert!(!Rung::TolerantFlags.is_degraded_open());
        assert!(Rung::IndexReconstruction.is_degraded_open());
        assert!(Rung::RepairTranscode.is_degraded_open());
    }
}
