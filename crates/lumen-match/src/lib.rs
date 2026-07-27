//! Filename parsing and metadata candidate ranking — `docs/05` §4.4, research item **R8**.
//!
//! This is where every product in this space is mediocre, and it is cheap to be better because the
//! Probe stage has already read the file's exact duration. Runtime proximity resolves remakes and
//! title collisions that title-and-year scoring cannot, and nothing else uses it.
//!
//! ```
//! use lumen_match::{parse, match_candidates, Candidate, MatchQuery, MatchOutcome, IdProvider, ExternalId};
//!
//! let parsed = parse("Blade.Runner.2049.2017.2160p.UHD.BluRay.REMUX.HDR.TrueHD.7.1-GROUP.mkv");
//! assert_eq!(parsed.title, "Blade Runner 2049");
//! assert_eq!(parsed.year, Some(2017));
//! assert_eq!(parsed.release_group.as_deref(), Some("GROUP"));
//!
//! let candidate = Candidate::new(
//!     ExternalId { provider: IdProvider::Tmdb, value: "335984".into() },
//!     "Blade Runner 2049",
//! ).with_year(2017).with_runtime(164 * 60);
//!
//! let query = MatchQuery { parsed: &parsed, probed_runtime_seconds: Some(164 * 60) };
//! assert!(matches!(match_candidates(&query, &[candidate]), MatchOutcome::Confident(_)));
//! ```

#![forbid(unsafe_code)]

pub mod normalize;
pub mod parse;
pub mod score;
pub mod tokens;

pub use normalize::{best_similarity, normalize_title, similarity};
pub use parse::{EpisodeSpec, ExternalId, IdProvider, ParsedName, merge_with_folder, parse};
pub use score::{
    Candidate, MatchOutcome, MatchQuery, ScoreBreakdown, Scored, episode_matches, match_candidates,
    score,
};
pub use tokens::{Edition, HdrTag, Resolution, Source, TokenClass};
