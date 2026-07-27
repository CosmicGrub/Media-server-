//! Candidate ranking — `docs/05` §4.4 steps 4 and 5.
//!
//! Two decisions here are the ones that separate good matching from the mediocre matching every
//! product in this space ships:
//!
//! **Runtime is the strongest disambiguator, and nobody uses it.** By the time matching runs, the
//! Probe stage has already read the file's exact duration (`docs/05` §4.3). Comparing it against a
//! provider's runtime resolves remakes, foreign-title collisions, and same-title-same-year pairs that
//! title-and-year scoring cannot. Research item **R9** exists to quantify how much it helps.
//!
//! **Ambiguity is a first-class outcome.** When the top two candidates are close, the honest answer is
//! "I don't know" — surfaced as a review queue, never resolved by silently taking the first result.
//! Silent wrong matches are worse than no match: the user has to notice before they can fix it.

use crate::normalize;
use crate::parse::{EpisodeSpec, ExternalId, ParsedName};

/// A candidate returned by a metadata provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: ExternalId,
    pub title: String,
    /// Original-language titles and AKAs. A foreign film catalogued under an English release title
    /// only ever matches on one of the two.
    pub alternate_titles: Vec<String>,
    pub year: Option<u16>,
    pub runtime_seconds: Option<u32>,
    /// Provider popularity, any scale. Used only to break otherwise-equal ties, never to outrank a
    /// real signal — popularity bias is how obscure films get matched to blockbusters.
    pub popularity: Option<f32>,
}

impl Candidate {
    pub fn new(id: ExternalId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            alternate_titles: Vec::new(),
            year: None,
            runtime_seconds: None,
            popularity: None,
        }
    }

    pub fn with_year(mut self, year: u16) -> Self {
        self.year = Some(year);
        self
    }

    pub fn with_runtime(mut self, seconds: u32) -> Self {
        self.runtime_seconds = Some(seconds);
        self
    }
}

/// Everything known about the file at match time.
#[derive(Debug, Clone)]
pub struct MatchQuery<'a> {
    pub parsed: &'a ParsedName,
    /// Exact duration from the Probe stage. The strongest signal available and the one the
    /// competition ignores.
    pub probed_runtime_seconds: Option<u32>,
}

/// Per-signal contributions, kept separate so a bad match can be explained rather than just scored.
///
/// The review queue shows these directly: "title 0.94, year exact, runtime off by 41 min" tells a
/// user what went wrong; a bare 0.71 does not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreBreakdown {
    pub title: f32,
    pub year: f32,
    pub runtime: f32,
    pub popularity: f32,
    pub total: f32,
}

/// Signal weights. Title dominates; runtime outranks year because a provider's year can legitimately
/// differ by one (festival vs general release) while a runtime mismatch of 20 minutes cannot be
/// explained away.
const W_TITLE: f32 = 1.00;
const W_RUNTIME: f32 = 0.45;
const W_YEAR: f32 = 0.35;
const W_POPULARITY: f32 = 0.05;

/// Below this, a candidate is not offered at all.
pub const MIN_VIABLE_SCORE: f32 = 0.45;
/// A candidate must reach this to be applied without review.
pub const CONFIDENT_SCORE: f32 = 0.80;
/// If the runner-up is within this of the leader, the choice is genuinely ambiguous.
pub const AMBIGUITY_MARGIN: f32 = 0.08;

/// How well two runtimes agree, in `0.0..=1.0`.
///
/// Tolerance is proportional rather than absolute: 3 minutes out on a 22-minute sitcom is a different
/// claim from 3 minutes out on a 3-hour epic. Deliberately generous, because legitimate differences
/// exist — an extended cut, PAL speedup, or a provider listing the theatrical runtime for a disc that
/// carries the director's cut.
fn runtime_agreement(file_seconds: u32, candidate_seconds: u32) -> f32 {
    if file_seconds == 0 || candidate_seconds == 0 {
        return 0.0;
    }
    let diff = file_seconds.abs_diff(candidate_seconds) as f32;
    let reference = file_seconds.min(candidate_seconds) as f32;
    // Free within 2%, degrading to zero by 25%.
    let ratio = diff / reference;
    if ratio <= 0.02 {
        1.0
    } else if ratio >= 0.25 {
        0.0
    } else {
        1.0 - (ratio - 0.02) / 0.23
    }
}

fn year_agreement(file_year: u16, candidate_year: u16) -> f32 {
    match file_year.abs_diff(candidate_year) {
        0 => 1.0,
        // Festival, limited, and international release years legitimately differ by one.
        1 => 0.7,
        2 => 0.3,
        _ => 0.0,
    }
}

/// Score one candidate against the query.
pub fn score(query: &MatchQuery<'_>, candidate: &Candidate) -> ScoreBreakdown {
    let title = normalize::best_similarity(
        &query.parsed.title,
        &candidate.title,
        &candidate.alternate_titles,
    );

    // Absent signals contribute nothing, rather than counting against a candidate. A provider that
    // does not publish runtimes must not lose to one that does.
    let (year, year_weight) = match (query.parsed.year, candidate.year) {
        (Some(a), Some(b)) => (year_agreement(a, b), W_YEAR),
        _ => (0.0, 0.0),
    };
    let (runtime, runtime_weight) = match (query.probed_runtime_seconds, candidate.runtime_seconds)
    {
        (Some(a), Some(b)) => (runtime_agreement(a, b), W_RUNTIME),
        _ => (0.0, 0.0),
    };
    let popularity = candidate.popularity.map_or(0.0, |p| (p / 100.0).clamp(0.0, 1.0));

    let weighted =
        title * W_TITLE + year * year_weight + runtime * runtime_weight + popularity * W_POPULARITY;
    let max = W_TITLE + year_weight + runtime_weight + W_POPULARITY;
    let total = if max > 0.0 { weighted / max } else { 0.0 };

    ScoreBreakdown { title, year, runtime, popularity, total }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub candidate: Candidate,
    pub breakdown: ScoreBreakdown,
}

/// The result of matching. `NeedsReview` is a real outcome, not a failure.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOutcome {
    /// The name pinned an ID. No scoring ran; the user already told us the answer (§4.4 rule 1).
    Pinned(Vec<ExternalId>),
    /// One clear winner.
    Confident(Scored),
    /// Two or more plausible candidates too close to separate. Goes to the review queue, ranked, and
    /// is the single best job for the optional AI agent (`docs/07` §5.2).
    NeedsReview(Vec<Scored>),
    /// Nothing scored above [`MIN_VIABLE_SCORE`].
    NoMatch,
}

impl MatchOutcome {
    /// The ID to apply without asking, if there is one.
    pub fn decided_id(&self) -> Option<&ExternalId> {
        match self {
            Self::Pinned(ids) => ids.first(),
            Self::Confident(s) => Some(&s.candidate.id),
            _ => None,
        }
    }

    pub fn needs_human(&self) -> bool {
        matches!(self, Self::NeedsReview(_))
    }
}

/// Rank candidates and decide whether the result is safe to apply.
pub fn match_candidates(query: &MatchQuery<'_>, candidates: &[Candidate]) -> MatchOutcome {
    // A pinned ID short-circuits everything. Re-deriving a match the user already specified is how
    // libraries get silently re-matched after an unrelated provider update.
    if !query.parsed.pinned_ids.is_empty() {
        return MatchOutcome::Pinned(query.parsed.pinned_ids.clone());
    }

    let mut scored: Vec<Scored> = candidates
        .iter()
        .map(|c| Scored { candidate: c.clone(), breakdown: score(query, c) })
        .filter(|s| s.breakdown.total >= MIN_VIABLE_SCORE)
        .collect();

    // Sort by score, then by ID for a deterministic order — a client and a server ranking the same
    // candidate list must agree, and float ties are otherwise resolved by input order.
    scored.sort_by(|a, b| {
        b.breakdown
            .total
            .partial_cmp(&a.breakdown.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.id.value.cmp(&b.candidate.id.value))
    });

    match scored.len() {
        0 => MatchOutcome::NoMatch,
        1 => {
            let top = scored.remove(0);
            if top.breakdown.total >= CONFIDENT_SCORE {
                MatchOutcome::Confident(top)
            } else {
                MatchOutcome::NeedsReview(vec![top])
            }
        }
        _ => {
            let margin = scored[0].breakdown.total - scored[1].breakdown.total;
            if scored[0].breakdown.total >= CONFIDENT_SCORE && margin > AMBIGUITY_MARGIN {
                MatchOutcome::Confident(scored.remove(0))
            } else {
                scored.truncate(5);
                MatchOutcome::NeedsReview(scored)
            }
        }
    }
}

/// Does a candidate's episode numbering agree with the parsed file?
///
/// Kept separate from scoring because a season/episode mismatch is disqualifying rather than a
/// penalty: episode 3 is not a worse match for episode 4, it is the wrong file.
pub fn episode_matches(parsed: &EpisodeSpec, season: u16, episode: u16) -> bool {
    match parsed {
        EpisodeSpec::SeasonEpisode { season: s, episodes } => {
            *s == season && episodes.contains(&episode)
        }
        EpisodeSpec::SeasonOnly { season: s } => *s == season,
        // Absolute numbering needs the AniDB↔TVDB mapping tables to compare; without them, refusing
        // to guess is correct.
        EpisodeSpec::Absolute(_) | EpisodeSpec::Date { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{self, IdProvider};

    fn q<'a>(parsed: &'a ParsedName, runtime: Option<u32>) -> MatchQuery<'a> {
        MatchQuery { parsed, probed_runtime_seconds: runtime }
    }

    fn cand(id: &str, title: &str) -> Candidate {
        Candidate::new(ExternalId { provider: IdProvider::Tmdb, value: id.into() }, title)
    }

    #[test]
    fn runtime_resolves_a_remake_that_title_and_year_cannot() {
        // The disambiguator docs/05 §4.4 step 4 calls out and nothing else uses. Both candidates are
        // called "Dune"; only the runtime tells them apart, and the year is absent from the filename.
        let parsed = parse::parse("Dune.2160p.BluRay.mkv");
        let candidates = [
            cand("841", "Dune").with_year(1984).with_runtime(137 * 60),
            cand("438631", "Dune").with_year(2021).with_runtime(155 * 60),
        ];
        let outcome = match_candidates(&q(&parsed, Some(155 * 60)), &candidates);
        match outcome {
            MatchOutcome::Confident(s) => assert_eq!(s.candidate.id.value, "438631"),
            other => panic!("runtime should have decided this: {other:?}"),
        }
    }

    #[test]
    fn without_runtime_the_same_pair_is_correctly_ambiguous() {
        // The honest outcome. Silently picking one is the failure mode this exists to prevent.
        let parsed = parse::parse("Dune.2160p.BluRay.mkv");
        let candidates = [
            cand("841", "Dune").with_year(1984).with_runtime(137 * 60),
            cand("438631", "Dune").with_year(2021).with_runtime(155 * 60),
        ];
        let outcome = match_candidates(&q(&parsed, None), &candidates);
        assert!(outcome.needs_human(), "expected review, got {outcome:?}");
        assert!(outcome.decided_id().is_none(), "must not apply an ambiguous match");
    }

    #[test]
    fn a_pinned_id_short_circuits_scoring_entirely() {
        // Even with a wildly wrong title, the user's explicit ID wins (§4.4 rule 1).
        let parsed = parse::parse("Complete Nonsense {tmdb-603} 1080p.mkv");
        let candidates = [cand("999", "Something Else").with_year(1999)];
        let outcome = match_candidates(&q(&parsed, None), &candidates);
        match outcome {
            MatchOutcome::Pinned(ids) => {
                assert_eq!(ids.len(), 1);
                assert_eq!(ids[0].value, "603");
                assert_eq!(ids[0].provider, IdProvider::Tmdb);
            }
            other => panic!("expected Pinned, got {other:?}"),
        }
    }

    #[test]
    fn year_off_by_one_is_forgiven_but_noticed() {
        // Festival and international release years legitimately differ by one.
        assert_eq!(year_agreement(2019, 2019), 1.0);
        assert!(year_agreement(2019, 2020) > 0.5);
        assert!(year_agreement(2019, 2021) < 0.5);
        assert_eq!(year_agreement(1984, 2021), 0.0);
    }

    #[test]
    fn runtime_agreement_is_proportional_not_absolute() {
        // Three minutes out is nothing on a 3-hour epic and a lot on a 22-minute sitcom.
        let epic = runtime_agreement(180 * 60, 183 * 60);
        let sitcom = runtime_agreement(22 * 60, 25 * 60);
        assert!(epic > sitcom, "epic {epic} should tolerate 3 min better than sitcom {sitcom}");
        assert_eq!(runtime_agreement(7200, 7200), 1.0);
        assert_eq!(runtime_agreement(0, 7200), 0.0, "unknown runtime must not score");
        assert_eq!(runtime_agreement(3600, 7200), 0.0, "double length is no match");
    }

    #[test]
    fn absent_signals_do_not_penalise_a_candidate() {
        // A provider that publishes no runtime must not lose to one that does, on title alone.
        let parsed = parse::parse("Arrival.2016.1080p.BluRay.mkv");
        let rich = cand("329865", "Arrival").with_year(2016).with_runtime(116 * 60);
        let sparse = cand("329865", "Arrival").with_year(2016);
        let bare = cand("329865", "Arrival");

        let rich_score = score(&q(&parsed, Some(116 * 60)), &rich).total;
        let sparse_score = score(&q(&parsed, Some(116 * 60)), &sparse).total;
        let bare_score = score(&q(&parsed, None), &bare).total;

        for (name, s) in [("rich", rich_score), ("sparse", sparse_score), ("bare", bare_score)] {
            assert!(s >= CONFIDENT_SCORE, "{name} scored {s}, below the confidence threshold");
        }
    }

    #[test]
    fn a_wrong_title_cannot_be_rescued_by_year_and_runtime() {
        // Title dominates by design; agreeing metadata on the wrong film is a coincidence.
        let parsed = parse::parse("The.Godfather.1972.1080p.mkv");
        let wrong = cand("1", "Solaris").with_year(1972).with_runtime(175 * 60);
        let s = score(&q(&parsed, Some(175 * 60)), &wrong);
        assert!(s.total < CONFIDENT_SCORE, "wrong title scored {} on metadata alone", s.total);
    }

    #[test]
    fn nothing_plausible_yields_no_match_rather_than_a_bad_one() {
        let parsed = parse::parse("Some.Obscure.Home.Video.mkv");
        let candidates =
            [cand("1", "Avatar").with_year(2009), cand("2", "Titanic").with_year(1997)];
        assert_eq!(match_candidates(&q(&parsed, None), &candidates), MatchOutcome::NoMatch);
        assert_eq!(match_candidates(&q(&parsed, None), &[]), MatchOutcome::NoMatch);
    }

    #[test]
    fn a_single_weak_candidate_goes_to_review_not_straight_in() {
        let parsed = parse::parse("Blade.Runner.1080p.mkv");
        let weak = [cand("1", "Blade").with_year(1998)];
        let outcome = match_candidates(&q(&parsed, None), &weak);
        assert!(
            matches!(outcome, MatchOutcome::NeedsReview(_) | MatchOutcome::NoMatch),
            "a weak lone candidate must not be applied: {outcome:?}"
        );
    }

    #[test]
    fn ranking_is_deterministic_for_equal_scores() {
        // A client and a server ranking the same list must agree, or they disagree about identity.
        let parsed = parse::parse("Twins.2019.1080p.mkv");
        let a = cand("bbb", "Twins").with_year(2019);
        let b = cand("aaa", "Twins").with_year(2019);
        let forward = match_candidates(&q(&parsed, None), &[a.clone(), b.clone()]);
        let reversed = match_candidates(&q(&parsed, None), &[b, a]);
        assert_eq!(forward, reversed, "candidate order changed the outcome");
    }

    #[test]
    fn review_lists_are_capped_and_ordered_best_first() {
        let parsed = parse::parse("Alien.1080p.mkv");
        let candidates: Vec<Candidate> =
            (0..12).map(|i| cand(&format!("{i:02}"), "Alien").with_year(1979 + i)).collect();
        match match_candidates(&q(&parsed, None), &candidates) {
            MatchOutcome::NeedsReview(list) => {
                assert!(list.len() <= 5, "review list should be a shortlist, got {}", list.len());
                for w in list.windows(2) {
                    assert!(w[0].breakdown.total >= w[1].breakdown.total, "not best-first");
                }
            }
            other => panic!("expected review, got {other:?}"),
        }
    }

    #[test]
    fn alternate_titles_carry_foreign_releases() {
        let parsed = parse::parse("Le.fabuleux.destin.d.Amelie.Poulain.2001.1080p.BluRay.mkv");
        let mut c = cand("194", "Amélie").with_year(2001).with_runtime(122 * 60);
        c.alternate_titles = vec!["Le Fabuleux Destin d'Amélie Poulain".into()];
        let s = score(&q(&parsed, Some(122 * 60)), &c);
        assert!(s.total >= CONFIDENT_SCORE, "alternate title should carry it, got {}", s.total);
    }

    #[test]
    fn episode_numbering_mismatch_is_disqualifying_not_a_penalty() {
        let spec = EpisodeSpec::SeasonEpisode { season: 1, episodes: vec![4] };
        assert!(episode_matches(&spec, 1, 4));
        assert!(!episode_matches(&spec, 1, 3), "episode 3 is the wrong file, not a worse match");
        assert!(!episode_matches(&spec, 2, 4));

        // Multi-episode files satisfy every episode they contain.
        let multi = EpisodeSpec::SeasonEpisode { season: 1, episodes: vec![1, 2] };
        assert!(episode_matches(&multi, 1, 1) && episode_matches(&multi, 1, 2));

        // Absolute numbering needs the mapping tables; refusing to guess is correct.
        assert!(!episode_matches(&EpisodeSpec::Absolute(vec![28]), 1, 28));
    }

    #[test]
    fn breakdown_explains_the_decision_for_the_review_queue() {
        // "title 0.94, year exact, runtime off" is actionable; a bare total is not.
        let parsed = parse::parse("Arrival.2016.1080p.mkv");
        let c = cand("329865", "Arrival").with_year(2016).with_runtime(116 * 60);
        let b = score(&q(&parsed, Some(116 * 60)), &c);
        assert_eq!(b.title, 1.0);
        assert_eq!(b.year, 1.0);
        assert_eq!(b.runtime, 1.0);
        assert!(b.total > 0.9);
    }

    #[test]
    fn every_score_stays_in_range() {
        let parsed = parse::parse("Anything.2020.mkv");
        for title in ["", "x", "Anything", "a much longer unrelated title entirely"] {
            for runtime in [None, Some(0), Some(1), Some(100_000)] {
                let mut c = cand("1", title);
                c.runtime_seconds = runtime;
                c.popularity = Some(1e9);
                let s = score(&q(&parsed, runtime), &c);
                assert!((0.0..=1.0).contains(&s.total), "{title:?}/{runtime:?} -> {}", s.total);
            }
        }
    }
}
