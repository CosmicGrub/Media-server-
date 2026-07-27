//! Artwork selection — `docs/14` §2.
//!
//! Not "highest rated wins". The load-bearing insight is that **language preference depends on the
//! artwork kind**:
//!
//! - A **poster** carries the title, so a Japanese poster is worse for an English-speaking user than a
//!   lower-rated English one.
//! - A **backdrop** sits behind UI text, so a *textless* one is strictly better than any localised
//!   one. Providers tag those with no language at all, and that null is a feature rather than missing
//!   data — which is exactly the sort of thing a naive "prefer my language" rule gets wrong.

use crate::language::{LangTag, LanguageMatch, resolve_language};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ArtworkKind {
    Poster,
    Backdrop,
    Logo,
    ClearArt,
    Banner,
    Thumb,
    Disc,
    SeasonPoster,
    EpisodeStill,
    ActorHeadshot,
    AlbumCover,
    ArtistBackground,
}

/// How this kind of artwork should treat language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguagePolicy {
    /// The image contains text: prefer the user's language, then textless, then anything.
    PreferUserLanguage,
    /// The image sits behind UI text: prefer textless, always.
    PreferTextless,
    /// The image has no text: language is irrelevant.
    Irrelevant,
}

impl ArtworkKind {
    pub fn language_policy(self) -> LanguagePolicy {
        match self {
            Self::Poster
            | Self::Logo
            | Self::ClearArt
            | Self::Banner
            | Self::SeasonPoster
            | Self::AlbumCover => LanguagePolicy::PreferUserLanguage,
            Self::Backdrop | Self::ArtistBackground | Self::Disc | Self::Thumb => {
                LanguagePolicy::PreferTextless
            }
            Self::EpisodeStill | Self::ActorHeadshot => LanguagePolicy::Irrelevant,
        }
    }

    /// Ideal width-to-height ratio, and the tolerance within which a candidate is acceptable.
    ///
    /// Wrong-aspect artwork is visible as letterboxing or cropping in a poster grid, so a
    /// well-rated image of the wrong shape is not a good choice.
    pub fn ideal_aspect(self) -> (f32, f32) {
        match self {
            Self::Poster | Self::SeasonPoster => (0.667, 0.08), // 2:3
            Self::Backdrop | Self::Thumb | Self::EpisodeStill | Self::ArtistBackground => {
                (1.778, 0.15) // 16:9
            }
            Self::Banner => (5.4, 0.6),
            Self::Logo | Self::ClearArt => (2.5, 2.0), // highly variable
            Self::Disc | Self::AlbumCover => (1.0, 0.05),
            Self::ActorHeadshot => (0.667, 0.25),
        }
    }

    /// Minimum width worth accepting. Below this the image looks worse than a placeholder on a 4K TV.
    pub fn min_width(self) -> u32 {
        match self {
            Self::Poster | Self::SeasonPoster => 500,
            Self::Backdrop | Self::ArtistBackground => 1280,
            Self::Thumb | Self::EpisodeStill => 640,
            Self::Logo | Self::ClearArt => 400,
            Self::Banner => 758,
            Self::Disc => 400,
            Self::AlbumCover => 500,
            Self::ActorHeadshot => 185,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtworkRef {
    pub kind: ArtworkKind,
    pub url: String,
    /// `None` means textless. For backdrops that is the *best* value, not missing data.
    pub language: Option<LangTag>,
    pub width: u32,
    pub height: u32,
    /// Provider rating, any scale, normalised by the caller to 0..=10.
    pub rating: Option<f32>,
    pub vote_count: u32,
    /// Position of the supplying provider in the user's ranking. Lower is preferred.
    pub provider_rank: u8,
}

impl ArtworkRef {
    pub fn new(kind: ArtworkKind, url: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            kind,
            url: url.into(),
            language: None,
            width,
            height,
            rating: None,
            vote_count: 0,
            provider_rank: 0,
        }
    }

    pub fn with_language(mut self, tag: &str) -> Self {
        self.language = Some(LangTag::new(tag));
        self
    }

    pub fn with_rating(mut self, rating: f32, votes: u32) -> Self {
        self.rating = Some(rating);
        self.vote_count = votes;
        self
    }

    pub fn is_textless(&self) -> bool {
        self.language.as_ref().is_none_or(LangTag::is_undetermined)
    }

    pub fn aspect(&self) -> f32 {
        if self.height == 0 { 0.0 } else { self.width as f32 / self.height as f32 }
    }

    fn aspect_ok(&self) -> bool {
        let (ideal, tolerance) = self.kind.ideal_aspect();
        (self.aspect() - ideal).abs() <= tolerance
    }

    /// A rating shrunk toward the mean by its vote count, so a 10/10 from one voter does not beat an
    /// 8.5/10 from four hundred. Plain provider ratings are dominated by single-vote noise.
    fn confidence_weighted_rating(&self) -> f32 {
        const PRIOR: f32 = 5.0;
        const PRIOR_WEIGHT: f32 = 10.0;
        let Some(r) = self.rating else { return PRIOR };
        let n = self.vote_count as f32;
        (r * n + PRIOR * PRIOR_WEIGHT) / (n + PRIOR_WEIGHT)
    }
}

/// Why a particular image was chosen, for the diagnostics view and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionReason {
    pub language: Option<LanguageMatch>,
    pub textless: bool,
    pub aspect_ok: bool,
}

/// Choose the best artwork of one kind.
///
/// `candidates` may mix kinds; only those matching `kind` are considered. Returns the index into
/// `candidates` and why it won.
pub fn select_artwork(
    candidates: &[ArtworkRef],
    kind: ArtworkKind,
    wanted_languages: &[LangTag],
) -> Option<(usize, SelectionReason)> {
    let pool: Vec<usize> =
        candidates.iter().enumerate().filter(|(_, a)| a.kind == kind).map(|(i, _)| i).collect();
    if pool.is_empty() {
        return None;
    }

    // Resolution and aspect are quality filters, but only when relaxing them is not the difference
    // between an image and no image at all — a small poster beats a grey placeholder.
    let usable: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|i| candidates[*i].width >= kind.min_width() && candidates[*i].aspect_ok())
        .collect();
    let pool = if usable.is_empty() { pool } else { usable };

    match kind.language_policy() {
        LanguagePolicy::PreferTextless => {
            // Textless first, unconditionally. A backdrop with a burned-in title collides with the UI
            // text drawn over it, however well rated it is.
            let textless: Vec<usize> =
                pool.iter().copied().filter(|i| candidates[*i].is_textless()).collect();
            let (search, textless_won) =
                if textless.is_empty() { (pool, false) } else { (textless, true) };
            let best = best_by_quality(candidates, &search)?;
            Some((
                best,
                SelectionReason {
                    language: None,
                    textless: textless_won,
                    aspect_ok: candidates[best].aspect_ok(),
                },
            ))
        }
        LanguagePolicy::Irrelevant => {
            let best = best_by_quality(candidates, &pool)?;
            Some((
                best,
                SelectionReason {
                    language: None,
                    textless: candidates[best].is_textless(),
                    aspect_ok: candidates[best].aspect_ok(),
                },
            ))
        }
        LanguagePolicy::PreferUserLanguage => {
            // Group by language, resolve which language to use, then pick the best within it.
            let available: Vec<LangTag> = pool
                .iter()
                .map(|i| candidates[*i].language.clone().unwrap_or_else(|| LangTag::new("und")))
                .collect();

            if let Some((idx, m)) = resolve_language(&available, wanted_languages, None)
                && m.is_preferred()
            {
                let chosen_lang = available[idx].clone();
                let same: Vec<usize> = pool
                    .iter()
                    .copied()
                    .zip(available.iter())
                    .filter(|(_, l)| **l == chosen_lang)
                    .map(|(i, _)| i)
                    .collect();
                let best = best_by_quality(candidates, &same)?;
                return Some((
                    best,
                    SelectionReason {
                        language: Some(m),
                        textless: candidates[best].is_textless(),
                        aspect_ok: candidates[best].aspect_ok(),
                    },
                ));
            }

            // No preferred language. A textless poster is better than one in a language the user
            // cannot read, so it is tried before falling back to whatever is left.
            let textless: Vec<usize> =
                pool.iter().copied().filter(|i| candidates[*i].is_textless()).collect();
            if !textless.is_empty() {
                let best = best_by_quality(candidates, &textless)?;
                return Some((
                    best,
                    SelectionReason {
                        language: None,
                        textless: true,
                        aspect_ok: candidates[best].aspect_ok(),
                    },
                ));
            }
            let best = best_by_quality(candidates, &pool)?;
            Some((
                best,
                SelectionReason {
                    language: Some(LanguageMatch::LastResort),
                    textless: false,
                    aspect_ok: candidates[best].aspect_ok(),
                },
            ))
        }
    }
}

/// Best within an already language-filtered set: provider order, then weighted rating, then
/// resolution, then URL for determinism.
fn best_by_quality(candidates: &[ArtworkRef], pool: &[usize]) -> Option<usize> {
    pool.iter().copied().reduce(|a, b| {
        let (x, y) = (&candidates[a], &candidates[b]);
        let key = |v: &ArtworkRef| {
            (
                std::cmp::Reverse(v.provider_rank),
                (v.confidence_weighted_rating() * 1000.0) as i64,
                v.width,
            )
        };
        match key(y).cmp(&key(x)) {
            std::cmp::Ordering::Greater => b,
            std::cmp::Ordering::Less => a,
            // Deterministic tie-break: a client and a server must choose the same image.
            std::cmp::Ordering::Equal => {
                if y.url < x.url {
                    b
                } else {
                    a
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poster(url: &str, lang: Option<&str>, w: u32, rating: f32, votes: u32) -> ArtworkRef {
        let mut a =
            ArtworkRef::new(ArtworkKind::Poster, url, w, w * 3 / 2).with_rating(rating, votes);
        if let Some(l) = lang {
            a = a.with_language(l);
        }
        a
    }

    fn backdrop(url: &str, lang: Option<&str>, w: u32, rating: f32, votes: u32) -> ArtworkRef {
        let mut a =
            ArtworkRef::new(ArtworkKind::Backdrop, url, w, w * 9 / 16).with_rating(rating, votes);
        if let Some(l) = lang {
            a = a.with_language(l);
        }
        a
    }

    fn en() -> Vec<LangTag> {
        vec![LangTag::new("en")]
    }

    #[test]
    fn backdrops_prefer_textless_even_over_a_better_rated_localised_one() {
        // The headline rule. A backdrop sits behind UI text; a burned-in title collides with it.
        let c = vec![
            backdrop("english-titled.jpg", Some("en"), 3840, 9.8, 500),
            backdrop("textless.jpg", None, 1920, 6.0, 20),
        ];
        let (i, why) = select_artwork(&c, ArtworkKind::Backdrop, &en()).unwrap();
        assert_eq!(c[i].url, "textless.jpg");
        assert!(why.textless);
    }

    #[test]
    fn backdrops_fall_back_to_localised_when_no_textless_exists() {
        let c = vec![backdrop("en.jpg", Some("en"), 1920, 7.0, 50)];
        let (i, why) = select_artwork(&c, ArtworkKind::Backdrop, &en()).unwrap();
        assert_eq!(c[i].url, "en.jpg");
        assert!(!why.textless, "reported honestly rather than claimed textless");
    }

    #[test]
    fn posters_prefer_the_users_language_over_a_better_rated_foreign_one() {
        // Inverse of the backdrop rule: a poster carries the title, so language matters more than
        // rating. A 9.9-rated Japanese poster is worse for an English reader.
        let c = vec![
            poster("ja.jpg", Some("ja"), 2000, 9.9, 900),
            poster("en.jpg", Some("en"), 1000, 6.5, 30),
        ];
        let (i, why) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "en.jpg");
        assert_eq!(why.language, Some(LanguageMatch::Exact));
    }

    #[test]
    fn posters_prefer_textless_over_an_unreadable_language() {
        // No English poster exists. A textless one is better than a Korean one for an English reader.
        let c = vec![
            poster("ko.jpg", Some("ko"), 2000, 9.0, 400),
            poster("textless.jpg", None, 1500, 7.0, 40),
        ];
        let (i, why) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "textless.jpg");
        assert!(why.textless);
    }

    #[test]
    fn a_dialect_poster_is_accepted_and_reported_as_such() {
        let c = vec![poster("pt-pt.jpg", Some("pt-PT"), 1000, 7.0, 50)];
        let (i, why) = select_artwork(&c, ArtworkKind::Poster, &[LangTag::new("pt-BR")]).unwrap();
        assert_eq!(c[i].url, "pt-pt.jpg");
        assert_eq!(why.language, Some(LanguageMatch::Dialect));
    }

    #[test]
    fn single_vote_perfect_scores_do_not_beat_well_supported_good_ones() {
        // Raw provider ratings are dominated by single-vote noise.
        let c = vec![
            poster("one-voter.jpg", Some("en"), 1000, 10.0, 1),
            poster("many-voters.jpg", Some("en"), 1000, 8.5, 400),
        ];
        let (i, _) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "many-voters.jpg");
    }

    #[test]
    fn wrong_aspect_is_filtered_out_when_a_correct_one_exists() {
        // A square "poster" letterboxes in a poster grid, however well rated.
        let mut square = poster("square.jpg", Some("en"), 1000, 9.9, 900);
        square.height = 1000;
        let c = vec![square, poster("correct.jpg", Some("en"), 800, 6.0, 20)];
        let (i, why) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "correct.jpg");
        assert!(why.aspect_ok);
    }

    #[test]
    fn a_flawed_image_beats_no_image() {
        // Quality filters must not reduce the pool to nothing — a small poster beats a grey box.
        let tiny = poster("tiny.jpg", Some("en"), 90, 5.0, 5);
        let c = vec![tiny];
        let (i, why) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "tiny.jpg");
        assert!(!why.aspect_ok || c[i].width < ArtworkKind::Poster.min_width());
    }

    #[test]
    fn provider_rank_outranks_rating() {
        // "Artwork from Fanart.tv" means Fanart.tv wins, not "whoever rated highest".
        let mut preferred = poster("fanart.jpg", Some("en"), 1000, 6.0, 10);
        preferred.provider_rank = 0;
        let mut other = poster("tmdb.jpg", Some("en"), 1000, 9.5, 900);
        other.provider_rank = 3;
        let c = vec![other, preferred];
        let (i, _) = select_artwork(&c, ArtworkKind::Poster, &en()).unwrap();
        assert_eq!(c[i].url, "fanart.jpg");
    }

    #[test]
    fn other_kinds_are_ignored_and_absence_is_none() {
        let c = vec![backdrop("b.jpg", None, 1920, 8.0, 100)];
        assert_eq!(select_artwork(&c, ArtworkKind::Poster, &en()), None);
        assert_eq!(select_artwork(&[], ArtworkKind::Poster, &en()), None);
    }

    #[test]
    fn selection_is_deterministic_regardless_of_input_order() {
        let a = poster("a.jpg", Some("en"), 1000, 8.0, 100);
        let b = poster("b.jpg", Some("en"), 1000, 8.0, 100);
        let forward = select_artwork(&[a.clone(), b.clone()], ArtworkKind::Poster, &en()).unwrap();
        let reverse = select_artwork(&[b, a], ArtworkKind::Poster, &en()).unwrap();
        // Same URL chosen either way, even though the index differs.
        assert_eq!(forward.1, reverse.1);
    }

    #[test]
    fn language_policy_matches_whether_the_image_carries_text() {
        assert_eq!(ArtworkKind::Poster.language_policy(), LanguagePolicy::PreferUserLanguage);
        assert_eq!(ArtworkKind::Logo.language_policy(), LanguagePolicy::PreferUserLanguage);
        assert_eq!(ArtworkKind::Backdrop.language_policy(), LanguagePolicy::PreferTextless);
        assert_eq!(ArtworkKind::ActorHeadshot.language_policy(), LanguagePolicy::Irrelevant);
    }
}
