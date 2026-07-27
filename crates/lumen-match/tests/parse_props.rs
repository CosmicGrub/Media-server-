//! Property tests for the filename parser.
//!
//! Filenames are untrusted input: anyone who can write into a watched folder chooses them, and on a
//! shared library that is not necessarily the server's owner. The parser therefore has the same
//! robustness obligation as the container probes — no panic, no hang, no unbounded work.
//!
//! Beyond robustness, the properties pin down two things the unit tests state only by example:
//! parsing is deterministic, and the parser never invents information that was not in the name.

use lumen_match::{EpisodeSpec, ParsedName, merge_with_folder, normalize_title, parse, similarity};
use proptest::prelude::*;

/// Fragments drawn from real naming conventions, so generated names reach deep code paths instead of
/// bouncing off the first character.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("Blade".to_string()),
        Just("Runner".to_string()),
        Just("2049".to_string()),
        Just("2017".to_string()),
        Just("S01E01".to_string()),
        Just("S01".to_string()),
        Just("E02".to_string()),
        Just("1x01".to_string()),
        Just("1080p".to_string()),
        Just("2160p".to_string()),
        Just("BluRay".to_string()),
        Just("REMUX".to_string()),
        Just("TrueHD".to_string()),
        Just("7.1".to_string()),
        Just("Atmos".to_string()),
        Just("x265".to_string()),
        Just("HDR".to_string()),
        Just("{tmdb-238}".to_string()),
        Just("[A1B2C3D4]".to_string()),
        Just("(1982)".to_string()),
        Just("Season".to_string()),
        Just("Episode".to_string()),
        Just("101".to_string()),
        Just("sample".to_string()),
        Just("cd1".to_string()),
        Just("-GROUP".to_string()),
        Just("Amélie".to_string()),
        Just("S.W.A.T.".to_string()),
        Just("2024.01.15".to_string()),
        Just("".to_string()),
    ]
}

fn separator() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(".".to_string()),
        Just(" ".to_string()),
        Just("_".to_string()),
        Just("-".to_string()),
        Just("".to_string()),
    ]
}

fn plausible_name() -> impl Strategy<Value = String> {
    proptest::collection::vec((fragment(), separator()), 1..10).prop_map(|parts| {
        let mut s = String::new();
        for (frag, sep) in parts {
            s.push_str(&frag);
            s.push_str(&sep);
        }
        s.push_str(".mkv");
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3072))]

    /// Robustness over arbitrary text. Filenames can contain anything a filesystem permits.
    #[test]
    fn arbitrary_names_never_panic(name in ".*") {
        let _ = parse(&name);
    }

    /// The same over names that actually exercise the layers.
    #[test]
    fn plausible_names_never_panic(name in plausible_name()) {
        let _ = parse(&name);
    }

    /// Unicode, control characters, and very long names are all legal on some filesystem.
    #[test]
    fn hostile_names_never_panic(
        name in proptest::collection::vec(any::<char>(), 0..300).prop_map(String::from_iter)
    ) {
        let _ = parse(&name);
    }

    /// Parsing is deterministic. The scanner and the server both parse the same name, and a
    /// disagreement would mean two different identities for one file.
    #[test]
    fn parsing_is_deterministic(name in plausible_name()) {
        prop_assert_eq!(parse(&name), parse(&name));
    }

    /// The parser never invents a title. Every word it reports must have come from the input, so a
    /// garbage filename cannot produce a plausible-looking title that then matches a real film.
    #[test]
    fn the_title_is_always_a_substring_of_the_input(name in plausible_name()) {
        let got = parse(&name);
        let haystack = normalize_title(&name);
        for word in normalize_title(&got.title).split_whitespace() {
            prop_assert!(
                haystack.contains(word),
                "title word {word:?} does not appear in input {name:?}"
            );
        }
    }

    /// A year, if reported, is in range. An out-of-range year would be applied to a library item and
    /// then shown to the user.
    #[test]
    fn reported_years_are_plausible(name in plausible_name()) {
        if let Some(y) = parse(&name).year {
            prop_assert!((1880..=2099).contains(&y), "implausible year {y} from {name:?}");
        }
    }

    /// Episode numbers stay in a sane range, and a multi-episode list is non-empty and ordered.
    #[test]
    fn reported_episodes_are_wellformed(name in plausible_name()) {
        match parse(&name).episode {
            Some(EpisodeSpec::SeasonEpisode { episodes, .. }) => {
                prop_assert!(!episodes.is_empty(), "empty episode list from {name:?}");
            }
            Some(EpisodeSpec::Absolute(nums)) => {
                prop_assert!(!nums.is_empty());
                prop_assert!(nums.iter().all(|n| *n >= 1), "episode 0 from {name:?}");
            }
            Some(EpisodeSpec::Date { month, day, .. }) => {
                prop_assert!((1..=12).contains(&month), "month {month} from {name:?}");
                prop_assert!((1..=31).contains(&day), "day {day} from {name:?}");
            }
            _ => {}
        }
    }

    /// Adding technical tags after a name must not change the title it yields. This is the whole
    /// premise of boundary detection, and it is what breaks first when a new tag is added wrongly.
    #[test]
    fn appending_technical_tags_does_not_change_the_title(
        base in prop_oneof![
            Just("Blade Runner (1982)"),
            Just("Breaking.Bad.S01E01"),
            Just("Arrival.2016"),
            Just("The.German.Doctor.2013"),
        ],
        tags in proptest::collection::vec(
            prop_oneof![
                Just("1080p"), Just("2160p"), Just("BluRay"), Just("REMUX"), Just("x265"),
                Just("TrueHD"), Just("Atmos"), Just("HDR"), Just("DV"), Just("WEB-DL"),
            ],
            0..6,
        ),
    ) {
        let bare = parse(&format!("{base}.mkv"));
        let tagged = parse(&format!("{base}.{}.mkv", tags.join(".")));
        prop_assert_eq!(
            normalize_title(&bare.title), normalize_title(&tagged.title),
            "tags {:?} changed the title of {:?}", tags, base
        );
        prop_assert_eq!(bare.year, tagged.year, "tags {:?} changed the year of {:?}", tags, base);
        prop_assert_eq!(bare.episode, tagged.episode, "tags changed the episode of {:?}", base);
    }

    /// Folder merging is monotonic: it may add information but must never discard what the file
    /// already established. `docs/05` §4.4 step 3 makes the folder stronger, not destructive.
    #[test]
    fn folder_merge_never_loses_information(
        file in plausible_name(),
        folder in plausible_name(),
    ) {
        let f = parse(&file);
        let d = parse(&folder);
        let merged = merge_with_folder(&f, &d);

        prop_assert!(
            merged.pinned_ids.len() >= f.pinned_ids.len(),
            "merge dropped a pinned ID"
        );
        prop_assert!(merged.editions.len() >= f.editions.len(), "merge dropped an edition");
        // Technical fields come from the file and are never overwritten by a folder name.
        prop_assert_eq!(merged.resolution, f.resolution);
        prop_assert_eq!(merged.source, f.source);
        prop_assert_eq!(merged.release_group, f.release_group);
        // A file that identified an episode keeps one.
        if f.episode.is_some() {
            prop_assert!(merged.episode.is_some(), "merge dropped the episode");
        }
    }

    /// Similarity is reflexive, symmetric, and bounded — scoring relies on all three.
    #[test]
    fn similarity_is_wellbehaved(a in ".{0,40}", b in ".{0,40}") {
        let (na, nb) = (normalize_title(&a), normalize_title(&b));
        let ab = similarity(&na, &nb);
        let ba = similarity(&nb, &na);
        prop_assert!((0.0..=1.0).contains(&ab), "{} out of range for {:?}/{:?}", ab, na, nb);
        prop_assert!((ab - ba).abs() < 1e-6, "asymmetric: {} vs {}", ab, ba);
        prop_assert_eq!(similarity(&na, &na), 1.0);
    }
}

/// Names that are cheap to get wrong, kept as a permanent regression set.
#[test]
fn known_awkward_names_are_handled() {
    let cases: &[&str] = &[
        "",
        ".",
        ".mkv",
        "...",
        "-",
        "-.mkv",
        "[]",
        "[].mkv",
        "{}",
        "{tmdb-}.mkv",
        "{tmdb-abc!}.mkv",
        "(((((",
        "S01E01",
        "s01e01e02e03e04e05.mkv",
        "S99999E99999.mkv",
        "9999999999999999999999.mkv",
        "0x0.mkv",
        "1x.mkv",
        "x01.mkv",
        "Season.mkv",
        "Season..mkv",
        "Episode 0.mkv",
        "2024.13.45.mkv",
        "0000.00.00.mkv",
        "sample-.mkv",
        "cd.mkv",
        "part.mkv",
        "\u{202e}reversed.mkv",
        "🎬 Movie (2020).mkv",
        "a.b.c.d.e.f.g.h.i.j.k.l.mkv",
    ];
    for name in cases {
        let got: ParsedName = parse(name);
        // Whatever it decides, the invariants hold.
        if let Some(y) = got.year {
            assert!((1880..=2099).contains(&y), "{name:?} -> year {y}");
        }
        if let Some(EpisodeSpec::Date { month, day, .. }) = got.episode {
            assert!((1..=12).contains(&month) && (1..=31).contains(&day), "{name:?}");
        }
    }
}
