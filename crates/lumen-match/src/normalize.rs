//! Title normalisation and similarity.
//!
//! Matching compares user filenames against provider titles, and the two agree far less often than
//! you would hope: `Amelie` vs `Le Fabuleux Destin d'Amélie Poulain`, `WALL-E` vs `WALL·E`,
//! `Spider-Man 2` vs `Spider Man II`. Normalising both sides is what turns those into matches.
//!
//! Deliberately dependency-free. A full Unicode normalisation crate would be more thorough, but the
//! folding table below covers Latin-1 Supplement and Latin Extended-A, which is where essentially all
//! Western film and TV titles live, and it keeps the crate free of a transitive dependency tree on
//! the scanner's hot path.

/// Articles stripped from the front of a title, across the languages a mixed library actually
/// contains. Provider titles and scene releases disagree constantly about whether to keep them —
/// `The Matrix` vs `Matrix, The` vs `Matrix`.
const LEADING_ARTICLES: &[&str] = &[
    "the", "a", "an", // English
    "le", "la", "les", "l", "un", "une", "des", // French
    "der", "die", "das", "ein", "eine", // German
    "el", "los", "las", "una", // Spanish
    "il", "lo", "gli", "uno", // Italian
    "o", "os", "as", "um", "uma", // Portuguese
    "de", "het", "een", // Dutch
];

/// Fold a character to its unaccented ASCII equivalent, or `None` to drop it.
///
/// Returns a `&'static str` rather than a `char` because some characters expand: `æ` becomes `ae`,
/// `ß` becomes `ss`.
fn fold_char(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ş' | 'š' | 'ŝ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'þ' => "th",
        'ð' => "d",
        _ => return None,
    })
}

/// Roman numerals worth normalising. Sequels are written both ways — `Rocky II` and `Rocky 2` — and
/// they must compare equal. Stops at 20; beyond that, films use digits.
const ROMAN: &[(&str, &str)] = &[
    ("xx", "20"),
    ("xix", "19"),
    ("xviii", "18"),
    ("xvii", "17"),
    ("xvi", "16"),
    ("xv", "15"),
    ("xiv", "14"),
    ("xiii", "13"),
    ("xii", "12"),
    ("xi", "11"),
    ("x", "10"),
    ("ix", "9"),
    ("viii", "8"),
    ("vii", "7"),
    ("vi", "6"),
    ("v", "5"),
    ("iv", "4"),
    ("iii", "3"),
    ("ii", "2"),
];

/// Collapse `S.W.A.T.` to `SWAT`.
///
/// Initialisms are the one case where dots are part of the title rather than a separator, and there
/// is no way to tell them apart later — after tokenising, `S.W.A.T.` is indistinguishable from four
/// separate words.
///
/// Applied to **both** sides of a comparison: a provider writes `S.W.A.T.` and a release writes
/// `SWAT`, and they must normalise to the same thing.
pub fn collapse_initialisms(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;

    while i < chars.len() {
        // A run of `X.` pairs, at least two long, is an initialism — but only when it *starts* at a
        // word boundary. Without that check the tail of a longer word joins the run, and
        // `Destin.d.Amelie` collapses to `DestindAmelie`.
        let at_word_start = i == 0 || !chars[i - 1].is_alphanumeric();
        let mut run = 0;
        while i + run * 2 + 1 < chars.len()
            && chars[i + run * 2].is_alphabetic()
            && chars[i + run * 2 + 1] == '.'
        {
            run += 1;
        }
        if run >= 2 && at_word_start {
            for k in 0..run {
                out.push(chars[i + k * 2]);
            }
            i += run * 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Normalise a title for comparison: fold accents, lowercase, expand `&`, drop punctuation, collapse
/// whitespace, normalise roman numerals, and strip a leading article.
///
/// Not reversible and never shown to a user — this exists purely so two spellings of the same title
/// compare equal.
pub fn normalize_title(input: &str) -> String {
    let collapsed = collapse_initialisms(input);
    let mut folded = String::with_capacity(collapsed.len());
    for c in collapsed.chars() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        if let Some(rep) = fold_char(lower) {
            folded.push_str(rep);
        } else if lower.is_ascii_alphanumeric() {
            folded.push(lower);
        } else if lower == '&' {
            folded.push_str(" and ");
        } else {
            // Any other separator — including `·` in WALL·E and `-` in Spider-Man — becomes a space.
            folded.push(' ');
        }
    }

    let mut words: Vec<String> = folded
        .split_whitespace()
        .map(|w| {
            ROMAN
                .iter()
                .find(|(roman, _)| *roman == w)
                .map_or_else(|| w.to_string(), |(_, arabic)| (*arabic).to_string())
        })
        .collect();

    // Strip one leading article. `The The` (the band) keeps its second word, and a title that is
    // *only* an article keeps it rather than normalising to nothing.
    if words.len() > 1 && LEADING_ARTICLES.contains(&words[0].as_str()) {
        words.remove(0);
    }
    // Trailing `, The` is the library-catalogue spelling of the same thing.
    if words.len() > 1 && LEADING_ARTICLES.contains(&words[words.len() - 1].as_str()) {
        words.pop();
    }

    words.join(" ")
}

/// Levenshtein distance with a cutoff, using two rows rather than a full matrix.
///
/// The cutoff matters: scoring runs over every candidate a provider returns, for every file in a
/// library, and an unbounded quadratic comparison of two long strings is real time on a NAS.
fn levenshtein_within(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > max {
            return None; // every remaining path already exceeds the budget
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[b.len()];
    if d <= max { Some(d) } else { None }
}

/// Similarity in `0.0..=1.0` between two already-normalised titles.
///
/// Blends two measures because they fail differently:
/// - **Token-set F1** handles reordering and extra words (`Blade Runner 2049 Final Cut` vs
///   `Blade Runner 2049`), but is blind to spelling.
/// - **Edit-distance ratio** handles typos and transliteration (`Amelie` vs `Amelia`), but is
///   destroyed by word reordering.
///
/// Taking the maximum means either signal alone can carry a match, which is what real filenames need.
pub fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_tokens: Vec<&str> = a.split_whitespace().collect();
    let b_tokens: Vec<&str> = b.split_whitespace().collect();

    // Multiset intersection: each token in `b` can only be claimed once. Counting `a`'s tokens that
    // merely *appear* in `b` double-counts repeats, which pushes recall above 1 and makes the whole
    // measure asymmetric — so `similarity(x, y) != similarity(y, x)` and ranking would depend on
    // argument order. Found by `similarity_is_wellbehaved` on the input ("&", "&&").
    let mut pool = b_tokens.clone();
    let mut shared = 0usize;
    for t in &a_tokens {
        if let Some(pos) = pool.iter().position(|x| x == t) {
            pool.swap_remove(pos);
            shared += 1;
        }
    }
    let token_f1 = if shared == 0 {
        0.0
    } else {
        let precision = shared as f32 / a_tokens.len() as f32;
        let recall = shared as f32 / b_tokens.len() as f32;
        2.0 * precision * recall / (precision + recall)
    };

    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let longest = ac.len().max(bc.len());
    // Allow up to 40% of the longer string to differ before declaring no edit-distance signal.
    let budget = longest * 2 / 5;
    let edit_ratio = match levenshtein_within(&ac, &bc, budget) {
        Some(d) => 1.0 - (d as f32 / longest as f32),
        None => 0.0,
    };

    token_f1.max(edit_ratio).clamp(0.0, 1.0)
}

/// Best similarity across a primary title and any alternates (original-language titles, AKAs).
///
/// Providers return these for a reason: a French film released internationally under an English
/// title will only ever match on one of the two.
pub fn best_similarity(query: &str, primary: &str, alternates: &[String]) -> f32 {
    let q = normalize_title(query);
    let mut best = similarity(&q, &normalize_title(primary));
    for alt in alternates {
        best = best.max(similarity(&q, &normalize_title(alt)));
        if best >= 1.0 {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_are_folded() {
        assert_eq!(normalize_title("Amélie"), "amelie");
        assert_eq!(
            normalize_title("Le Fabuleux Destin d'Amélie Poulain"),
            "fabuleux destin d amelie poulain"
        );
        assert_eq!(normalize_title("Bjørn"), "bjorn");
        assert_eq!(normalize_title("Straße"), "strasse");
        assert_eq!(normalize_title("Æon Flux"), "aeon flux");
    }

    #[test]
    fn leading_articles_are_stripped_both_spellings() {
        // Providers and scene releases disagree constantly about these.
        assert_eq!(normalize_title("The Matrix"), normalize_title("Matrix"));
        assert_eq!(normalize_title("Matrix, The"), normalize_title("The Matrix"));
        assert_eq!(normalize_title("Der Untergang"), normalize_title("Untergang"));
        assert_eq!(normalize_title("Les Misérables"), normalize_title("Miserables"));
    }

    #[test]
    fn a_title_that_is_only_an_article_survives() {
        // Normalising to the empty string would make it match everything.
        assert_eq!(normalize_title("The"), "the");
        assert!(!normalize_title("Them").is_empty());
    }

    #[test]
    fn initialism_collapsing_stops_at_word_boundaries() {
        // Found by the R8 corpus: without a word-start check, the tail of a longer word joins the
        // run. `Le.Fabuleux.Destin.d.Amelie` became `Le Fabuleux DestindAmelie`.
        assert_eq!(collapse_initialisms("Destin.d.Amelie"), "Destin.d.Amelie");
        assert_eq!(collapse_initialisms("Episode.IV.A.New.Hope"), "Episode.IV.A.New.Hope");
        // Genuine initialisms still collapse.
        assert_eq!(collapse_initialisms("S.W.A.T."), "SWAT");
        assert_eq!(collapse_initialisms("The.S.W.A.T.Team"), "The.SWATTeam");
    }

    #[test]
    fn punctuation_variants_converge() {
        assert_eq!(normalize_title("WALL·E"), normalize_title("WALL-E"));
        assert_eq!(normalize_title("Spider-Man"), normalize_title("Spider Man"));
        assert_eq!(normalize_title("S.W.A.T."), normalize_title("SWAT"));
        assert_eq!(normalize_title("Fast & Furious"), normalize_title("Fast and Furious"));
    }

    #[test]
    fn roman_numerals_normalise_to_digits() {
        assert_eq!(normalize_title("Rocky II"), normalize_title("Rocky 2"));
        assert_eq!(normalize_title("Star Wars Episode IV"), normalize_title("Star Wars Episode 4"));
        // Longer numerals must not be shadowed by their prefixes: XIII is 13, not 10 + III.
        assert_eq!(normalize_title("Apollo XIII"), normalize_title("Apollo 13"));
    }

    #[test]
    fn roman_normalisation_does_not_eat_real_words() {
        // A bare "I" or "X" as a whole word is ambiguous, but multi-letter words must be untouched.
        assert_eq!(normalize_title("Vive la France"), "vive la france");
        // Only a *leading* article is stripped; "the" inside a title is part of it.
        assert_eq!(normalize_title("Ivan the Terrible"), "ivan the terrible");
    }

    #[test]
    fn identical_titles_score_one() {
        assert_eq!(similarity("blade runner", "blade runner"), 1.0);
    }

    #[test]
    fn empty_input_scores_zero_rather_than_matching_everything() {
        assert_eq!(similarity("", "blade runner"), 0.0);
        assert_eq!(similarity("blade runner", ""), 0.0);
        assert_eq!(similarity("", ""), 1.0, "two empty strings are trivially equal");
    }

    #[test]
    fn extra_words_degrade_gracefully_rather_than_failing() {
        let s = similarity(
            normalize_title("Blade Runner 2049 Final Cut").as_str(),
            "blade runner 2049",
        );
        assert!(s > 0.7, "expected a strong partial match, got {s}");
    }

    #[test]
    fn typos_and_transliterations_still_match() {
        assert!(similarity("amelie", "amelia") > 0.7);
        assert!(similarity("inglourious basterds", "inglorious bastards") > 0.7);
    }

    #[test]
    fn unrelated_titles_score_low() {
        for (a, b) in
            [("blade runner", "the godfather"), ("dune", "arrival"), ("seven samurai", "toy story")]
        {
            let s = similarity(a, b);
            assert!(s < 0.5, "{a} vs {b} scored {s}, too high");
        }
    }

    #[test]
    fn repeated_words_do_not_break_symmetry() {
        // Regression: token-set intersection used to double-count repeats, so recall exceeded 1 and
        // similarity became asymmetric. Ranking then depended on which side was the query.
        for (a, b) in [("and", "and and"), ("the the", "the"), ("a a a", "a")] {
            let ab = similarity(a, b);
            let ba = similarity(b, a);
            assert!((ab - ba).abs() < 1e-6, "{a:?}/{b:?} asymmetric: {ab} vs {ba}");
            assert!((0.0..=1.0).contains(&ab), "{a:?}/{b:?} out of range: {ab}");
        }
    }

    #[test]
    fn similarity_is_symmetric_and_bounded() {
        let pairs = [("dune", "dune part two"), ("alien", "aliens"), ("up", "us")];
        for (a, b) in pairs {
            let ab = similarity(a, b);
            let ba = similarity(b, a);
            assert!((ab - ba).abs() < 1e-6, "{a}/{b}: {ab} vs {ba}");
            assert!((0.0..=1.0).contains(&ab), "{ab} out of range");
        }
    }

    #[test]
    fn alternate_titles_rescue_foreign_releases() {
        // A French film catalogued under its English release title only matches on the alternate.
        let alternates = vec!["Le Fabuleux Destin d'Amélie Poulain".to_string()];
        let s = best_similarity("Le fabuleux destin d'Amelie Poulain", "Amélie", &alternates);
        assert!(s > 0.9, "alternate title should carry the match, got {s}");
    }

    #[test]
    fn levenshtein_cutoff_rejects_hopeless_pairs_cheaply() {
        let a: Vec<char> = "a".repeat(200).chars().collect();
        let b: Vec<char> = "b".repeat(200).chars().collect();
        assert_eq!(levenshtein_within(&a, &b, 5), None);
        // Length difference alone is enough to reject.
        let short: Vec<char> = "abc".chars().collect();
        assert_eq!(levenshtein_within(&short, &a, 10), None);
    }

    #[test]
    fn levenshtein_is_correct_on_known_cases() {
        let d = |a: &str, b: &str| {
            levenshtein_within(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>(), 99)
        };
        assert_eq!(d("kitten", "sitting"), Some(3));
        assert_eq!(d("", "abc"), Some(3));
        assert_eq!(d("same", "same"), Some(0));
    }
}
