//! Filename → structured fields, by layered token stripping.
//!
//! `docs/05` §4.4. Deliberately **not** one large regex: release naming is a set of overlapping
//! conventions with no grammar, and a monolithic pattern is impossible to debug when it mis-fires on
//! one library out of a hundred. Instead each layer removes what it can identify and hands the rest
//! on, so a failure is localised to one layer and one corpus row.
//!
//! Order matters and is chosen so the most reliable anchors run first:
//!
//! 1. **Pinned IDs** — `{tmdb-12345}`. Present ⇒ nothing else needs to be right (§4.4 rule 1).
//! 2. **Bracket groups** — anime release group, CRC32 hashes, parenthesised years.
//! 3. **Episode markers** — the strongest structural anchor when present.
//! 4. **Year** — parenthesised is trusted further than bare.
//! 5. **First technical token** — resolution, source, codec. The title ended before it.
//! 6. **Everything left before that boundary is the title.**

use crate::normalize;
use crate::tokens::{self, Edition, HdrTag, Resolution, Source, TokenClass};

/// Bare four-digit years are only trusted up to here.
///
/// `Blade Runner 2049` is a title, not a 2049 release. Parenthesised years get a wider range because
/// the user wrote them deliberately. **Bump this as time passes** — the corpus row for Blade Runner
/// 2049 is what catches it if the two ever collide.
const MAX_BARE_YEAR: u16 = 2030;
const MAX_PAREN_YEAR: u16 = 2099;
const MIN_YEAR: u16 = 1880;

/// A provider whose ID can be pinned in a filename or folder name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdProvider {
    Tmdb,
    Imdb,
    Tvdb,
    AniDb,
    AniList,
    Tvmaze,
    MusicBrainz,
    Other(String),
}

/// An externally-pinned identity. Its presence short-circuits matching entirely: the user (or their
/// naming tool) told us the answer, and second-guessing it is how libraries get silently re-matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalId {
    pub provider: IdProvider,
    pub value: String,
}

/// How the file identifies its position in a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeSpec {
    /// `S01E01`, and multi-episode files like `S01E01-E03`.
    SeasonEpisode { season: u16, episodes: Vec<u16> },
    /// A season pack or a season folder: `S02`, `Season 2`.
    SeasonOnly { season: u16 },
    /// Absolute numbering with no season, as anime releases use. Resolving these to a season needs
    /// the AniDB↔TVDB mapping tables (`docs/05` §4.4 step 6).
    Absolute(Vec<u32>),
    /// Date-based, as daily shows use.
    Date { year: u16, month: u8, day: u8 },
}

impl EpisodeSpec {
    /// True when this identifies a specific episode rather than a whole season.
    pub fn is_specific(&self) -> bool {
        !matches!(self, Self::SeasonOnly { .. })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedName {
    pub title: String,
    pub year: Option<u16>,
    pub episode: Option<EpisodeSpec>,
    /// Providers pinned in the name. Non-empty ⇒ matching is already decided.
    pub pinned_ids: Vec<ExternalId>,
    pub editions: Vec<Edition>,
    pub resolution: Option<Resolution>,
    pub source: Option<Source>,
    pub hdr: Vec<HdrTag>,
    pub video_codec: Option<&'static str>,
    pub audio_codecs: Vec<&'static str>,
    pub channel_layout: Option<&'static str>,
    pub languages: Vec<&'static str>,
    pub release_group: Option<String>,
    /// Multi-part movie: `cd1`, `part2`, `disc1`. Parts belong to one logical item (`docs/05` §4.1).
    pub part: Option<u16>,
    pub is_sample: bool,
    pub extension: Option<String>,
}

impl ParsedName {
    /// The title carried no recognisable technical tokens, so the parse rests on nothing but the
    /// title text. Worth surfacing: these are the files most likely to need review.
    pub fn is_bare(&self) -> bool {
        self.resolution.is_none()
            && self.source.is_none()
            && self.video_codec.is_none()
            && self.release_group.is_none()
            && self.episode.is_none()
    }

    /// Series content, whichever numbering scheme it uses.
    pub fn is_episodic(&self) -> bool {
        self.episode.is_some()
    }
}

/// One token with its span in the working string, plus whether it sat inside brackets.
#[derive(Debug, Clone)]
struct Tok {
    lower: String,
    start: usize,
    /// Inside `(...)`, `[...]`, or `{...}`. A year here was written deliberately.
    bracketed: bool,
}

fn is_separator(c: char) -> bool {
    matches!(c, '.' | '_' | '-' | ' ' | '[' | ']' | '(' | ')' | '{' | '}' | ',' | '~' | '\t')
}

/// Split into lowercased tokens with byte offsets.
///
/// `+` and `'` stay inside tokens so `hdr10+` and `don't` survive intact.
fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut cur_start = 0usize;
    let mut cur_bracketed = false;

    let flush = |out: &mut Vec<Tok>, cur: &mut String, start: usize, bracketed: bool| {
        if !cur.is_empty() {
            out.push(Tok { lower: std::mem::take(cur), start, bracketed });
        }
    };

    for (i, c) in s.char_indices() {
        match c {
            '[' | '(' | '{' => {
                flush(&mut out, &mut cur, cur_start, cur_bracketed);
                depth += 1;
            }
            ']' | ')' | '}' => {
                flush(&mut out, &mut cur, cur_start, cur_bracketed);
                depth = (depth - 1).max(0);
            }
            c if is_separator(c) => flush(&mut out, &mut cur, cur_start, cur_bracketed),
            c => {
                if cur.is_empty() {
                    cur_start = i;
                    cur_bracketed = depth > 0;
                }
                cur.extend(c.to_lowercase());
            }
        }
    }
    flush(&mut out, &mut cur, cur_start, cur_bracketed);
    out
}

/// Recognise `tmdb-12345`, `tmdbid-12345`, `imdb-tt1234567`, `anidb-999`, and the `id` variants.
fn parse_id_token(text: &str) -> Option<ExternalId> {
    let lower = text.to_ascii_lowercase();
    let (raw_provider, value) = lower.split_once('-').or_else(|| lower.split_once('='))?;
    let provider_name = raw_provider.trim_end_matches("id");
    let provider = match provider_name {
        "tmdb" | "themoviedb" => IdProvider::Tmdb,
        "imdb" => IdProvider::Imdb,
        "tvdb" | "thetvdb" => IdProvider::Tvdb,
        "anidb" => IdProvider::AniDb,
        "anilist" | "al" => IdProvider::AniList,
        "tvmaze" => IdProvider::Tvmaze,
        "mbid" | "musicbrainz" => IdProvider::MusicBrainz,
        _ => return None,
    };
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ExternalId { provider, value: value.to_string() })
}

/// Extract and remove every `{...}` / `[...]` segment that is a pinned ID.
fn extract_pinned_ids(stem: &str) -> (String, Vec<ExternalId>) {
    let mut ids = Vec::new();
    let mut out = String::with_capacity(stem.len());
    let mut rest = stem;

    while let Some(open) = rest.find(['{', '[']) {
        let opener = rest.as_bytes()[open];
        let closer = if opener == b'{' { '}' } else { ']' };
        let Some(close_rel) = rest[open + 1..].find(closer) else {
            break;
        };
        let close = open + 1 + close_rel;
        let inner = &rest[open + 1..close];

        match parse_id_token(inner) {
            Some(id) => {
                ids.push(id);
                out.push_str(&rest[..open]);
                out.push(' ');
            }
            None => out.push_str(&rest[..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    (out, ids)
}

/// A leading `[Group]`, as anime releases use. Returns the group and the remainder.
fn extract_leading_bracket_group(stem: &str) -> (String, Option<String>) {
    let trimmed = stem.trim_start();
    if !trimmed.starts_with('[') {
        return (stem.to_string(), None);
    }
    let Some(close) = trimmed.find(']') else {
        return (stem.to_string(), None);
    };
    let inner = trimmed[1..close].trim();
    // A hash or a technical tag in leading brackets is not a group name.
    if inner.is_empty()
        || inner.len() > 30
        || matches!(tokens::classify(&inner.to_ascii_lowercase()), TokenClass::Hash)
        || tokens::classify(&inner.to_ascii_lowercase()).is_title_boundary()
    {
        return (stem.to_string(), None);
    }
    (trimmed[close + 1..].to_string(), Some(inner.to_string()))
}

/// A trailing `-GROUP`, as scene releases use.
fn extract_trailing_group(stem: &str) -> (String, Option<String>) {
    let trimmed = stem.trim_end();
    let Some(dash) = trimmed.rfind('-') else {
        return (stem.to_string(), None);
    };
    let candidate = trimmed[dash + 1..].trim();
    let lower = candidate.to_ascii_lowercase();
    // Scene group names are a single bare word. Allowing dots let the extractor swallow an entire
    // dotted filename (`sample-Another.Movie.2019.720p`), and not excluding episode markers let it
    // eat the `-E02` continuation of a multi-episode file.
    let looks_like_episode = parse_season_episode_token(&lower).is_some()
        || digits_after(&lower, 'e').is_some_and(|(_, used)| used == lower.len())
        || digits_after(&lower, 's').is_some_and(|(_, used)| used == lower.len());
    let plausible = (2..=25).contains(&candidate.len())
        && !candidate.contains(' ')
        && !candidate.contains('.')
        && candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !candidate.chars().all(|c| c.is_ascii_digit())
        && !looks_like_episode
        && matches!(tokens::classify(&lower), TokenClass::Unknown);
    if plausible {
        (trimmed[..dash].to_string(), Some(candidate.to_string()))
    } else {
        (stem.to_string(), None)
    }
}

fn digits_after(tok: &str, prefix: char) -> Option<(u16, usize)> {
    let mut chars = tok.char_indices();
    let (_, first) = chars.next()?;
    if first != prefix {
        return None;
    }
    let rest = &tok[first.len_utf8()..];
    let digit_len = rest.chars().take_while(char::is_ascii_digit).count();
    if digit_len == 0 || digit_len > 4 {
        return None;
    }
    let value = rest[..digit_len].parse().ok()?;
    Some((value, 1 + digit_len))
}

/// `s01e01`, `s01e01e02`, `s2024e05` — season and one or more episodes in a single token.
fn parse_season_episode_token(tok: &str) -> Option<(u16, Vec<u16>)> {
    let (season, consumed) = digits_after(tok, 's')?;
    let mut rest = &tok[consumed..];
    let mut episodes = Vec::new();
    while let Some((ep, used)) = digits_after(rest, 'e') {
        episodes.push(ep);
        rest = &rest[used..];
    }
    if episodes.is_empty() || !rest.is_empty() {
        return None;
    }
    Some((season, episodes))
}

/// `1x01`, `12x105`.
fn parse_x_form(tok: &str) -> Option<(u16, u16)> {
    let (s, e) = tok.split_once('x')?;
    if s.is_empty() || e.is_empty() || s.len() > 2 || e.len() > 4 {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_digit()) || !e.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((s.parse().ok()?, e.parse().ok()?))
}

fn as_u32(tok: &str) -> Option<u32> {
    if tok.is_empty() || !tok.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    tok.parse().ok()
}

fn plausible_year(value: u16, bracketed: bool) -> bool {
    let max = if bracketed { MAX_PAREN_YEAR } else { MAX_BARE_YEAR };
    (MIN_YEAR..=max).contains(&value)
}

/// Find an episode marker. Returns the spec and the token index it starts at.
fn find_episode(toks: &[Tok]) -> Option<(EpisodeSpec, usize)> {
    // Layer 1: `SxxEyy` in one token, plus any adjacent `Eyy` continuation tokens.
    for (i, t) in toks.iter().enumerate() {
        if let Some((season, mut episodes)) = parse_season_episode_token(&t.lower) {
            let mut j = i + 1;
            while let Some(next) = toks.get(j) {
                match digits_after(&next.lower, 'e') {
                    Some((ep, used)) if used == next.lower.len() => {
                        episodes.push(ep);
                        j += 1;
                    }
                    _ => break,
                }
            }
            episodes.dedup();
            return Some((EpisodeSpec::SeasonEpisode { season, episodes }, i));
        }
    }

    // Layer 2: split `S01` `E01` across tokens.
    for (i, t) in toks.iter().enumerate() {
        let Some((season, used)) = digits_after(&t.lower, 's') else { continue };
        if used != t.lower.len() {
            continue;
        }
        let mut episodes = Vec::new();
        let mut j = i + 1;
        while let Some(next) = toks.get(j) {
            match digits_after(&next.lower, 'e') {
                Some((ep, u)) if u == next.lower.len() => {
                    episodes.push(ep);
                    j += 1;
                }
                _ => break,
            }
        }
        if !episodes.is_empty() {
            return Some((EpisodeSpec::SeasonEpisode { season, episodes }, i));
        }
    }

    // Layer 3: `1x01`.
    for (i, t) in toks.iter().enumerate() {
        if let Some((season, ep)) = parse_x_form(&t.lower) {
            return Some((EpisodeSpec::SeasonEpisode { season, episodes: vec![ep] }, i));
        }
    }

    // Layer 4: spelled out — `Season 2 Episode 5`.
    for (i, t) in toks.iter().enumerate() {
        if t.lower != "season" {
            continue;
        }
        let Some(season) = toks.get(i + 1).and_then(|n| as_u32(&n.lower)) else { continue };
        let season = u16::try_from(season).unwrap_or(u16::MAX);
        if let Some(k) = toks.iter().skip(i + 2).position(|n| n.lower == "episode") {
            let idx = i + 2 + k;
            if let Some(ep) = toks.get(idx + 1).and_then(|n| as_u32(&n.lower)) {
                let ep = u16::try_from(ep).unwrap_or(u16::MAX);
                return Some((EpisodeSpec::SeasonEpisode { season, episodes: vec![ep] }, i));
            }
        }
        return Some((EpisodeSpec::SeasonOnly { season }, i));
    }

    // Layer 5: date-based, as daily shows use. Checked before bare-number heuristics so
    // `2024 01 15` is never mistaken for a year plus an episode number.
    for (i, window) in toks.windows(3).enumerate() {
        // `?` here would abandon the whole function on the first non-numeric window, silently
        // disabling this layer and every layer below it. Skip the window instead.
        let (Some(a), Some(b), Some(c)) =
            (as_u32(&window[0].lower), as_u32(&window[1].lower), as_u32(&window[2].lower))
        else {
            continue;
        };
        let ymd =
            window[0].lower.len() == 4 && window[1].lower.len() == 2 && window[2].lower.len() == 2;
        if ymd && plausible_year(a as u16, false) && (1..=12).contains(&b) && (1..=31).contains(&c)
        {
            return Some((EpisodeSpec::Date { year: a as u16, month: b as u8, day: c as u8 }, i));
        }
    }

    // Layer 6: `S02` alone — a season pack or a season folder.
    for (i, t) in toks.iter().enumerate() {
        if let Some((season, used)) = digits_after(&t.lower, 's')
            && used == t.lower.len()
            && t.lower.len() >= 2
        {
            return Some((EpisodeSpec::SeasonOnly { season }, i));
        }
    }

    None
}

/// Anime-style absolute numbering, and the compressed `101` form.
///
/// Only consulted when no season marker was found, and never allowed to consume a token already
/// claimed as the year — otherwise `Blade Runner 2049` becomes episode 2049.
fn find_absolute_episode(
    toks: &[Tok],
    boundary: usize,
    year_index: Option<usize>,
    hyphen_positions: &[usize],
) -> Option<(EpisodeSpec, usize)> {
    // Scan backwards for the last numeric token before the boundary. Taking only `boundary - 1`
    // missed every name with a trailing descriptive word, e.g. `... - 01 (Dual Audio 10bit ...)`,
    // where the token before the first technical tag is "audio".
    let (index, value) = (0..boundary)
        .rev()
        .filter(|i| Some(*i) != year_index)
        .find_map(|i| as_u32(&toks[i].lower).map(|v| (i, v)))?;
    let t = &toks[index];
    let year_like = t.lower.len() == 4
        && (MIN_YEAR..=MAX_PAREN_YEAR).contains(&u16::try_from(value).unwrap_or(u16::MAX));

    // Rule A: written as `Title - 28`. The hyphen is the anime convention, and it disambiguates even
    // four-digit numbers (One Piece is past 1100 episodes).
    let hyphen_separated = hyphen_positions.iter().any(|h| *h < t.start && t.start - *h <= 3);
    if hyphen_separated && (1..=9999).contains(&value) && !year_like {
        return Some((EpisodeSpec::Absolute(vec![value]), index));
    }

    // Rule B: the compressed `SEE`/`SSEE` form — `101` is season 1 episode 1, `1023` is S10E23.
    // Checked before bare absolute numbering because a dotted `.101.` in a TV filename is far more
    // often this convention than an absolute episode number written without a separator.
    //
    // Excluded for anything that could be a year at all: `Movie 1984` is a year, not S19E84. Note
    // this uses the *full* year range, not the bare-year cap — `Blade Runner 2049` must not become
    // season 20 episode 49, which is exactly what the corpus caught.
    if (3..=4).contains(&t.lower.len()) && !year_like {
        let (season, episode) = (value / 100, value % 100);
        if season >= 1 && episode >= 1 {
            return Some((
                EpisodeSpec::SeasonEpisode {
                    season: u16::try_from(season).unwrap_or(u16::MAX),
                    episodes: vec![u16::try_from(episode).unwrap_or(u16::MAX)],
                },
                index,
            ));
        }
    }

    // Rule C: a one- or two-digit number immediately before a technical token.
    if t.lower.len() <= 2 && (1..=99).contains(&value) && index + 1 == boundary {
        return Some((EpisodeSpec::Absolute(vec![value]), index));
    }
    None
}

/// Index of the first `YYYY MM DD` triple, whether or not it was chosen as the episode marker.
///
/// Its year is an air date and must never be claimed as the series year.
fn find_date_triple(toks: &[Tok]) -> Option<usize> {
    toks.windows(3).position(|w| {
        let (Some(a), Some(b), Some(c)) =
            (as_u32(&w[0].lower), as_u32(&w[1].lower), as_u32(&w[2].lower))
        else {
            return false;
        };
        w[0].lower.len() == 4
            && w[1].lower.len() == 2
            && w[2].lower.len() == 2
            && plausible_year(u16::try_from(a).unwrap_or(u16::MAX), false)
            && (1..=12).contains(&b)
            && (1..=31).contains(&c)
    })
}

fn find_part(toks: &[Tok]) -> Option<u16> {
    for (i, t) in toks.iter().enumerate() {
        let inline = ["cd", "part", "pt", "disc", "disk"]
            .iter()
            .find_map(|p| t.lower.strip_prefix(p).map(|rest| (p, rest)));
        if let Some((_, rest)) = inline
            && let Some(n) = as_u32(rest)
        {
            return u16::try_from(n).ok();
        }
        if ["cd", "part", "pt", "disc", "disk"].contains(&t.lower.as_str())
            && let Some(n) = toks.get(i + 1).and_then(|n| as_u32(&n.lower))
        {
            return u16::try_from(n).ok();
        }
    }
    None
}

/// Clean the raw title span into something displayable.
fn clean_title(raw: &str) -> String {
    let collapsed: String =
        raw.chars().map(|c| if is_separator(c) && c != '-' { ' ' } else { c }).collect();
    collapsed
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == '-' || c == ':'))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a filename, or a folder name, into structured fields.
///
/// Total: any input produces a `ParsedName`, possibly with an empty title. There is no error case,
/// because a name we cannot parse still describes a file that must play (`docs/11` G0).
pub fn parse(name: &str) -> ParsedName {
    let mut out = ParsedName::default();

    // Layer 0: extension. Only stripped when it looks like one — a title ending in `.5` must not
    // lose it, and a name with a long trailing segment after a dot is not an extension.
    let stem = match name.rsplit_once('.') {
        // A name that is *only* an extension has no title. Without this, ".mkv" parses as the film
        // "mkv", which then gets matched against a provider.
        Some((head, ext)) if head.is_empty() && (1..=5).contains(&ext.len()) => {
            out.extension = Some(ext.to_ascii_lowercase());
            String::new()
        }
        Some((head, ext))
            if !head.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic()) =>
        {
            out.extension = Some(ext.to_ascii_lowercase());
            head.to_string()
        }
        _ => name.to_string(),
    };

    // A leading `sample-` is the scene convention for "this is the sample clip for X". Stripping it
    // and flagging the file is more useful than either treating `sample` as part of the title or
    // letting it zero the title entirely.
    let stem = {
        let lower = stem.to_ascii_lowercase();
        match ["sample-", "sample.", "sample_", "sample "].iter().find(|p| lower.starts_with(**p)) {
            Some(prefix) => {
                out.is_sample = true;
                stem[prefix.len()..].to_string()
            }
            None => stem,
        }
    };

    // Layer 1: pinned IDs win outright (§4.4 rule 1).
    let (stem, ids) = extract_pinned_ids(&stem);
    out.pinned_ids = ids;

    // Layer 2: bracket and trailing groups.
    let (stem, leading_group) = extract_leading_bracket_group(&stem);
    let (stem, trailing_group) = extract_trailing_group(&stem);
    out.release_group = leading_group.or(trailing_group);

    let working = normalize::collapse_initialisms(&stem);
    let toks = tokenize(&working);
    if toks.is_empty() {
        return out;
    }

    let hyphen_positions: Vec<usize> =
        working.char_indices().filter(|(_, c)| *c == '-').map(|(i, _)| i).collect();

    // Layer 3: episode markers.
    let episode_hit = find_episode(&toks);

    // Layer 4: year. Prefer the last plausible year that is not the very first token — a file called
    // `1917.1080p.mkv` is the film 1917 with no year, not an untitled 1917 release.
    let episode_index = episode_hit.as_ref().map(|(_, i)| *i);
    // A date-based episode marker occupies three tokens, and its year is an air date, not the
    // series year. `Last Week Tonight S11E04 2024.03.03` is not a 2024 series.
    let date_year_index = match &episode_hit {
        Some((EpisodeSpec::Date { .. }, i)) => Some(*i),
        _ => find_date_triple(&toks),
    };
    let year_hit = toks
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && Some(*i) != episode_index && Some(*i) != date_year_index)
        .filter_map(|(i, t)| {
            let v = as_u32(&t.lower)?;
            let v = u16::try_from(v).ok()?;
            (t.lower.len() == 4 && plausible_year(v, t.bracketed)).then_some((i, v))
        })
        // A bracketed year was written deliberately, so it outranks a bare one wherever it sits.
        .max_by_key(|(i, _)| (toks[*i].bracketed, *i));
    out.year = year_hit.map(|(_, v)| v);

    // Layer 5: the first unambiguously technical token ends the title.
    let first_boundary = toks
        .iter()
        .position(|t| tokens::classify(&t.lower).is_title_boundary())
        .unwrap_or(toks.len());

    // Layer 6: absolute numbering, only when nothing better was found.
    let episode_hit = episode_hit.or_else(|| {
        find_absolute_episode(&toks, first_boundary, year_hit.map(|(i, _)| i), &hyphen_positions)
    });

    let title_end_index =
        [Some(first_boundary), episode_hit.as_ref().map(|(_, i)| *i), year_hit.map(|(i, _)| i)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(toks.len());

    let title_end_byte = toks.get(title_end_index).map_or(working.len(), |t| t.start);
    out.title = clean_title(&working[..title_end_byte]);
    out.episode = episode_hit.map(|(spec, _)| spec);

    // Layer 7: classify everything from the boundary on. Editions and languages are collected from
    // the whole name, since they are written in both halves depending on the naming convention.
    for (i, t) in toks.iter().enumerate() {
        match tokens::classify(&t.lower) {
            TokenClass::Resolution(r) => {
                out.resolution = Some(out.resolution.map_or(r, |e| e.max(r)))
            }
            TokenClass::Source(s) => out.source = Some(out.source.map_or(s, |e| e.max(s))),
            TokenClass::Hdr(h) => {
                if !out.hdr.contains(&h) {
                    out.hdr.push(h);
                }
            }
            TokenClass::VideoCodec(c) => out.video_codec = out.video_codec.or(Some(c)),
            TokenClass::AudioCodec(c) => {
                if !out.audio_codecs.contains(&c) {
                    out.audio_codecs.push(c);
                }
            }
            TokenClass::ChannelLayout(c) => out.channel_layout = out.channel_layout.or(Some(c)),
            TokenClass::Language(l) => {
                if !out.languages.contains(&l) {
                    out.languages.push(l);
                }
            }
            TokenClass::Edition(e) if i >= title_end_index || t.bracketed => {
                if !out.editions.contains(&e) {
                    out.editions.push(e);
                }
            }
            TokenClass::Flag("flag") if t.lower == "sample" => out.is_sample = true,
            _ => {}
        }
    }

    // Channel layouts survive tokenisation as two adjacent single digits (`5` `1` from `5.1`).
    if out.channel_layout.is_none() {
        for w in toks.windows(2) {
            if let (Some(a), Some(b)) = (as_u32(&w[0].lower), as_u32(&w[1].lower))
                && w[0].lower.len() == 1
                && w[1].lower.len() == 1
                && matches!(a, 1 | 2 | 5 | 6 | 7)
                && b <= 2
            {
                out.channel_layout = match (a, b) {
                    (2, 0) => Some("2.0"),
                    (5, 1) => Some("5.1"),
                    (6, 1) => Some("6.1"),
                    (7, 1) => Some("7.1"),
                    _ => None,
                };
                if out.channel_layout.is_some() {
                    break;
                }
            }
        }
    }

    out.part = find_part(&toks);
    out.editions.sort_unstable();
    out
}

/// Merge a folder-derived parse into a file-derived one.
///
/// `docs/05` §4.4 step 3: **folder context is stronger than filename.** `/Movies/Blade Runner
/// (1982)/br.2049.mkv` is Blade Runner 1982 — the folder was named by a human or a naming tool, the
/// filename by whatever produced it.
pub fn merge_with_folder(file: &ParsedName, folder: &ParsedName) -> ParsedName {
    let mut out = file.clone();

    // A folder title is preferred whenever it exists and is not obviously worse. Filenames are
    // routinely abbreviated to the point of uselessness; folders rarely are.
    if !folder.title.is_empty() && folder.title.len() >= file.title.len() {
        out.title = folder.title.clone();
    }
    if folder.year.is_some() {
        out.year = folder.year;
    }
    // IDs are additive: either level may pin, and both are authoritative.
    for id in &folder.pinned_ids {
        if !out.pinned_ids.contains(id) {
            out.pinned_ids.push(id.clone());
        }
    }
    // A season folder supplies the season a bare episode number lacks.
    if let (Some(EpisodeSpec::Absolute(nums)), Some(EpisodeSpec::SeasonOnly { season })) =
        (&file.episode, &folder.episode)
    {
        out.episode = Some(EpisodeSpec::SeasonEpisode {
            season: *season,
            episodes: nums.iter().map(|n| u16::try_from(*n).unwrap_or(u16::MAX)).collect(),
        });
    } else if file.episode.is_none() {
        out.episode = folder.episode.clone();
    }
    for e in &folder.editions {
        if !out.editions.contains(e) {
            out.editions.push(*e);
        }
    }
    out.editions.sort_unstable();
    out
}
