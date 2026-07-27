//! Accuracy benchmark against the labelled filename corpus — research item **R8** (`docs/10` §2).
//!
//! `fixtures/filenames.tsv` holds naming conventions taken from real libraries. The test reports
//! per-field accuracy and fails below a floor, so the parser cannot regress quietly while still
//! passing its unit tests.
//!
//! The floors are at 100% because the corpus currently passes cleanly. That is a statement about
//! *these 83 rows*, not about filename parsing in general — the point of the benchmark is that
//! adding a row which fails is how new naming conventions get discovered.
//!
//! When a row fails, **check the row before changing the parser.** Of the seven failures on the
//! first run, six were parser bugs and one was a mis-labelled expectation, and one more turned out
//! to be a case where neither the row nor the parser had the genuinely useful behaviour.

use lumen_match::{EpisodeSpec, ParsedName, parse};

const CORPUS: &str = include_str!("../fixtures/filenames.tsv");

/// Accuracy floors. Raise these as the parser improves; never lower them to make a change pass.
const MIN_TITLE_ACCURACY: f64 = 1.00;
const MIN_YEAR_ACCURACY: f64 = 1.00;
const MIN_EPISODE_ACCURACY: f64 = 1.00;

#[derive(Debug)]
struct Row {
    line: usize,
    filename: String,
    title: String,
    year: Option<u16>,
    episode: Option<EpisodeSpec>,
}

fn parse_expected_episode(field: &str) -> Option<EpisodeSpec> {
    let field = field.trim();
    if field == "-" || field.is_empty() {
        return None;
    }
    if let Some(rest) = field.strip_prefix("abs:") {
        return Some(EpisodeSpec::Absolute(vec![rest.parse().expect("absolute episode number")]));
    }
    if let Some(rest) = field.strip_prefix("date:") {
        let mut parts = rest.split('-');
        return Some(EpisodeSpec::Date {
            year: parts.next()?.parse().ok()?,
            month: parts.next()?.parse().ok()?,
            day: parts.next()?.parse().ok()?,
        });
    }
    // `S01`, `S01E01`, or `S01E01+E02`.
    let (season_part, episode_part) = match field.split_once('E') {
        Some((s, e)) => (s, Some(e)),
        None => (field, None),
    };
    let season: u16 = season_part.trim_start_matches(['S', 's']).parse().ok()?;
    match episode_part {
        None => Some(EpisodeSpec::SeasonOnly { season }),
        Some(eps) => Some(EpisodeSpec::SeasonEpisode {
            season,
            episodes: eps
                .split('+')
                .map(|e| e.trim_start_matches(['E', 'e']).parse().expect("episode number"))
                .collect(),
        }),
    }
}

fn load() -> Vec<Row> {
    CORPUS
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(i, line)| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(
                fields.len() >= 4,
                "line {}: expected 4 tab-separated fields, got {}: {line:?}",
                i + 1,
                fields.len()
            );
            Row {
                line: i + 1,
                filename: fields[0].to_string(),
                title: fields[1].trim().to_string(),
                year: fields[2].trim().parse().ok(),
                episode: parse_expected_episode(fields[3]),
            }
        })
        .collect()
}

/// Titles are compared after normalisation: the corpus records what a human would call the title, and
/// separator and case differences are not what this benchmark is measuring.
fn title_matches(expected: &str, got: &ParsedName) -> bool {
    if expected == "-" {
        return got.title.is_empty();
    }
    lumen_match::normalize_title(expected) == lumen_match::normalize_title(&got.title)
}

#[test]
fn corpus_accuracy_meets_the_floor() {
    let rows = load();
    assert!(rows.len() >= 80, "corpus has shrunk to {} rows", rows.len());

    let mut title_ok = 0usize;
    let mut year_ok = 0usize;
    let mut episode_ok = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for row in &rows {
        let got = parse(&row.filename);

        if title_matches(&row.title, &got) {
            title_ok += 1;
        } else {
            failures.push(format!(
                "L{} TITLE  {:?}\n         want {:?}\n         got  {:?}",
                row.line, row.filename, row.title, got.title
            ));
        }

        if got.year == row.year {
            year_ok += 1;
        } else {
            failures.push(format!(
                "L{} YEAR   {:?}  want {:?}, got {:?}",
                row.line, row.filename, row.year, got.year
            ));
        }

        if got.episode == row.episode {
            episode_ok += 1;
        } else {
            failures.push(format!(
                "L{} EP     {:?}\n         want {:?}\n         got  {:?}",
                row.line, row.filename, row.episode, got.episode
            ));
        }
    }

    let n = rows.len() as f64;
    let title_acc = title_ok as f64 / n;
    let year_acc = year_ok as f64 / n;
    let episode_acc = episode_ok as f64 / n;

    println!("\n=== R8 parser accuracy over {} corpus rows ===", rows.len());
    println!("  title    {:>6.1}%  ({title_ok}/{})", title_acc * 100.0, rows.len());
    println!("  year     {:>6.1}%  ({year_ok}/{})", year_acc * 100.0, rows.len());
    println!("  episode  {:>6.1}%  ({episode_ok}/{})", episode_acc * 100.0, rows.len());
    if !failures.is_empty() {
        println!("\n--- {} field mismatches ---", failures.len());
        for f in &failures {
            println!("{f}");
        }
    }

    assert!(title_acc >= MIN_TITLE_ACCURACY, "title accuracy {title_acc:.3} below floor");
    assert!(year_acc >= MIN_YEAR_ACCURACY, "year accuracy {year_acc:.3} below floor");
    assert!(episode_acc >= MIN_EPISODE_ACCURACY, "episode accuracy {episode_acc:.3} below floor");
}

#[test]
fn no_corpus_row_panics_or_invents_a_title() {
    // Every row must parse without panicking, and a degenerate name must yield an empty title rather
    // than something plausible-looking that would then be matched against a provider.
    for row in load() {
        let got = parse(&row.filename);
        if row.title == "-" {
            assert!(
                got.title.is_empty(),
                "L{} invented a title {:?} from {:?}",
                row.line,
                got.title,
                row.filename
            );
        }
    }
}

#[test]
fn pinned_ids_are_extracted_wherever_they_appear() {
    // §4.4 rule 1: an explicit ID is authoritative, so extraction must be reliable regardless of
    // where in the name it sits.
    let cases = [
        ("The Godfather (1972) {tmdb-238}.mkv", "238"),
        ("{tmdb-238} The Godfather.mkv", "238"),
        ("The Godfather {tmdbid-238} (1972) [1080p].mkv", "238"),
    ];
    for (name, expected) in cases {
        let got = parse(name);
        assert_eq!(got.pinned_ids.len(), 1, "{name}: {:?}", got.pinned_ids);
        assert_eq!(got.pinned_ids[0].value, expected, "{name}");
        assert!(got.title.to_lowercase().contains("godfather"), "{name}: {:?}", got.title);
    }
}

#[test]
fn technical_fields_are_extracted_from_a_full_remux_name() {
    let got = parse(
        "Blade.Runner.2049.2017.2160p.UHD.BluRay.REMUX.HDR.DV.TrueHD.7.1.Atmos-FraMeSToR.mkv",
    );
    assert_eq!(got.title, "Blade Runner 2049");
    assert_eq!(got.year, Some(2017));
    assert_eq!(got.resolution, Some(lumen_match::Resolution::P2160));
    assert_eq!(got.source, Some(lumen_match::Source::Remux), "REMUX must outrank BluRay");
    assert!(got.hdr.contains(&lumen_match::HdrTag::DolbyVision));
    assert!(got.hdr.contains(&lumen_match::HdrTag::Hdr10));
    assert!(got.audio_codecs.contains(&"truehd"));
    assert!(got.audio_codecs.contains(&"atmos"));
    assert_eq!(got.channel_layout, Some("7.1"));
    assert_eq!(got.release_group.as_deref(), Some("FraMeSToR"));
    assert_eq!(got.extension.as_deref(), Some("mkv"));
    assert!(!got.is_bare());
}

#[test]
fn folder_context_overrides_an_abbreviated_filename() {
    // §4.4 step 3, and the example the doc gives verbatim.
    let file = parse("br.2049.mkv");
    let folder = parse("Blade Runner 2049 (2017)");
    let merged = lumen_match::merge_with_folder(&file, &folder);
    assert_eq!(merged.title, "Blade Runner 2049");
    assert_eq!(merged.year, Some(2017));
}

#[test]
fn a_season_folder_supplies_the_season_a_bare_episode_lacks() {
    let file = parse("Frieren - 28 [1080p].mkv");
    let folder = parse("Season 1");
    let merged = lumen_match::merge_with_folder(&file, &folder);
    assert_eq!(
        merged.episode,
        Some(EpisodeSpec::SeasonEpisode { season: 1, episodes: vec![28] }),
        "absolute numbering plus a season folder resolves to a season/episode pair"
    );
}

#[test]
fn folder_ids_are_additive_not_overriding() {
    let file = parse("movie {imdb-tt0111161}.mkv");
    let folder = parse("The Shawshank Redemption (1994) {tmdb-278}");
    let merged = lumen_match::merge_with_folder(&file, &folder);
    assert_eq!(merged.pinned_ids.len(), 2, "both levels pin, both are authoritative");
}
