//! BCP-47 language resolution — `docs/14` §1.3.
//!
//! A provider asked for `pt-BR` may only have `pt`, or only `en`. Returning nothing when a usable
//! translation exists is a failure; silently returning Japanese to someone who reads neither is worse.
//! So the fallback chain is explicit and its outcome is reported, not inferred.

/// A BCP-47-ish tag, normalised for comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LangTag {
    normalized: String,
}

impl LangTag {
    pub const UNDETERMINED: &'static str = "und";

    /// Normalise a tag: lowercase, `_` to `-`, and the legacy three-letter codes folded to two-letter
    /// where an equivalent exists, so `jpn` and `ja` compare equal.
    pub fn new(tag: &str) -> Self {
        let lower = tag.trim().to_ascii_lowercase().replace('_', "-");
        if lower.is_empty() || matches!(lower.as_str(), "und" | "mis" | "zxx" | "mul") {
            return Self { normalized: Self::UNDETERMINED.to_string() };
        }
        let mut parts = lower.splitn(2, '-');
        let primary = parts.next().unwrap_or("");
        let rest = parts.next();
        let primary = three_to_two(primary);
        Self {
            normalized: match rest {
                Some(r) if !r.is_empty() => format!("{primary}-{r}"),
                _ => primary.to_string(),
            },
        }
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    /// The primary subtag: `pt-br` -> `pt`.
    pub fn base(&self) -> &str {
        self.normalized.split('-').next().unwrap_or(&self.normalized)
    }

    pub fn is_undetermined(&self) -> bool {
        self.normalized == Self::UNDETERMINED
    }

    /// Exactly the same tag, region and all.
    pub fn exact_eq(&self, other: &LangTag) -> bool {
        !self.is_undetermined() && self.normalized == other.normalized
    }

    /// Same language, possibly different region. `pt-BR` and `pt-PT` are mutually intelligible;
    /// offering one when the other was asked for is helpful, not wrong.
    pub fn base_eq(&self, other: &LangTag) -> bool {
        !self.is_undetermined() && !other.is_undetermined() && self.base() == other.base()
    }
}

/// Fold the common ISO 639-2/B three-letter codes onto their 639-1 equivalents.
///
/// Containers use both interchangeably — Matroska carries 639-2 in `Language` and BCP-47 in
/// `LanguageBCP47` — so `jpn` and `ja` must be the same language or track selection breaks.
fn three_to_two(code: &str) -> &str {
    match code {
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
        "nld" | "dut" => "nl",
        "swe" => "sv",
        "nor" => "no",
        "dan" => "da",
        "fin" => "fi",
        "pol" => "pl",
        "tur" => "tr",
        "ara" => "ar",
        "heb" => "he",
        "hin" => "hi",
        "tha" => "th",
        "vie" => "vi",
        "ind" => "id",
        "ces" | "cze" => "cs",
        "ell" | "gre" => "el",
        "hun" => "hu",
        "ron" | "rum" => "ro",
        "ukr" => "uk",
        "cat" => "ca",
        "isl" | "ice" => "is",
        other => other,
    }
}

/// How a language request was satisfied. Reported rather than hidden, because "close enough" and
/// "exactly what you asked for" are different user experiences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LanguageMatch {
    /// Exact tag including region.
    Exact,
    /// Same language, different region: `pt-BR` offered for `pt-PT`.
    Dialect,
    /// A later entry in the user's fallback list.
    Fallback,
    /// The work's own original language, offered because nothing preferred existed.
    OriginalLanguage,
    /// Something, because nothing better existed. Always worth telling the user about.
    LastResort,
}

impl LanguageMatch {
    /// True when the user got a language they actually asked for.
    pub fn is_preferred(self) -> bool {
        matches!(self, Self::Exact | Self::Dialect | Self::Fallback)
    }
}

/// Pick the best available tag for a request.
///
/// `wanted` is the user's ordered preference chain. `original` is the work's original language, used
/// only after the chain is exhausted. Returns the chosen index into `available` plus how good the
/// match was.
pub fn resolve_language(
    available: &[LangTag],
    wanted: &[LangTag],
    original: Option<&LangTag>,
) -> Option<(usize, LanguageMatch)> {
    if available.is_empty() {
        return None;
    }

    // Walk the preference chain in order, trying exact before dialect at each step. Doing it this way
    // round — rather than all-exact-then-all-dialect — respects the user's ordering: someone who
    // prefers `fr` over `en` wants `fr-CA` before `en-US`.
    for (rank, want) in wanted.iter().enumerate() {
        if let Some(i) = available.iter().position(|a| a.exact_eq(want)) {
            return Some((
                i,
                if rank == 0 { LanguageMatch::Exact } else { LanguageMatch::Fallback },
            ));
        }
        if let Some(i) = available.iter().position(|a| a.base_eq(want)) {
            return Some((
                i,
                if rank == 0 { LanguageMatch::Dialect } else { LanguageMatch::Fallback },
            ));
        }
    }

    if let Some(orig) = original
        && let Some(i) = available.iter().position(|a| a.base_eq(orig))
    {
        return Some((i, LanguageMatch::OriginalLanguage));
    }

    // Something beats nothing, but only with the label attached — a determinate language is preferred
    // over an unlabelled one so the user at least knows what they are looking at.
    let idx = available.iter().position(|a| !a.is_undetermined()).unwrap_or(0);
    Some((idx, LanguageMatch::LastResort))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(v: &[&str]) -> Vec<LangTag> {
        v.iter().map(|s| LangTag::new(s)).collect()
    }

    #[test]
    fn three_and_two_letter_codes_are_the_same_language() {
        // Matroska carries both forms; treating them as different breaks track selection outright.
        assert!(LangTag::new("jpn").exact_eq(&LangTag::new("ja")));
        assert!(LangTag::new("eng").exact_eq(&LangTag::new("EN")));
        assert!(LangTag::new("fre").exact_eq(&LangTag::new("fra")));
        assert!(LangTag::new("ger").exact_eq(&LangTag::new("de")));
    }

    #[test]
    fn regions_are_kept_but_do_not_prevent_a_dialect_match() {
        let br = LangTag::new("pt-BR");
        let pt = LangTag::new("pt-PT");
        assert_eq!(br.base(), "pt");
        assert!(!br.exact_eq(&pt), "regions differ");
        assert!(br.base_eq(&pt), "but the language is the same");
    }

    #[test]
    fn underscores_and_case_normalise() {
        assert!(LangTag::new("pt_br").exact_eq(&LangTag::new("PT-BR")));
    }

    #[test]
    fn undetermined_never_matches_anything() {
        for tag in ["", "und", "mis", "zxx", "mul", "  "] {
            let t = LangTag::new(tag);
            assert!(t.is_undetermined(), "{tag:?}");
            assert!(!t.exact_eq(&LangTag::new("und")));
            assert!(!t.base_eq(&LangTag::new("en")));
        }
    }

    #[test]
    fn exact_beats_dialect() {
        let available = tags(&["pt-PT", "pt-BR", "en"]);
        let (i, m) = resolve_language(&available, &tags(&["pt-BR"]), None).unwrap();
        assert_eq!(available[i].as_str(), "pt-br");
        assert_eq!(m, LanguageMatch::Exact);
    }

    #[test]
    fn dialect_is_offered_when_the_exact_region_is_missing() {
        let available = tags(&["pt-PT", "en"]);
        let (i, m) = resolve_language(&available, &tags(&["pt-BR"]), None).unwrap();
        assert_eq!(available[i].as_str(), "pt-pt");
        assert_eq!(m, LanguageMatch::Dialect, "helpful, and reported as not exact");
        assert!(m.is_preferred());
    }

    #[test]
    fn preference_order_is_respected_over_match_quality() {
        // Someone who prefers French over English wants fr-CA before en-US. Trying every exact match
        // before any dialect match would invert that.
        let available = tags(&["en-US", "fr-CA"]);
        let (i, m) = resolve_language(&available, &tags(&["fr", "en"]), None).unwrap();
        assert_eq!(available[i].as_str(), "fr-ca");
        assert_eq!(m, LanguageMatch::Dialect);
    }

    #[test]
    fn later_chain_entries_report_as_fallback_not_exact() {
        let available = tags(&["en", "de"]);
        let (i, m) = resolve_language(&available, &tags(&["fr", "en"]), None).unwrap();
        assert_eq!(available[i].as_str(), "en");
        assert_eq!(m, LanguageMatch::Fallback, "the user did not get their first choice");
    }

    #[test]
    fn original_language_is_used_only_after_the_chain_is_exhausted() {
        let available = tags(&["ja", "ko"]);
        let original = LangTag::new("ja");
        let (i, m) = resolve_language(&available, &tags(&["en", "fr"]), Some(&original)).unwrap();
        assert_eq!(available[i].as_str(), "ja");
        assert_eq!(m, LanguageMatch::OriginalLanguage);
        assert!(!m.is_preferred(), "not what the user asked for, and they should be told");
    }

    #[test]
    fn last_resort_prefers_a_labelled_language_over_an_unlabelled_one() {
        // Something beats nothing, but the user should at least know what they are looking at.
        let available = tags(&["und", "ko"]);
        let (i, m) = resolve_language(&available, &tags(&["en"]), None).unwrap();
        assert_eq!(available[i].as_str(), "ko");
        assert_eq!(m, LanguageMatch::LastResort);
    }

    #[test]
    fn nothing_available_yields_none_rather_than_an_empty_string() {
        assert_eq!(resolve_language(&[], &tags(&["en"]), None), None);
    }

    #[test]
    fn an_empty_preference_chain_still_returns_something_labelled_as_such() {
        let available = tags(&["de"]);
        let (_, m) = resolve_language(&available, &[], None).unwrap();
        assert_eq!(m, LanguageMatch::LastResort);
    }
}
