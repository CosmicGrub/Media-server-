//! Property tests for `merge_fragments` and `select_artwork`.
//!
//! `proptest` has been a declared dev-dependency of this crate with nothing using it — the merge and
//! artwork-selection invariants documented in the unit tests (locks are absolute, merge is order-
//! independent and idempotent, language policy beats rating) were each checked against a handful of
//! hand-picked fixtures, not against the much larger space a fuzzed input can reach. This file closes
//! that gap: the same invariants, generalized to randomized fragments/candidates.

use lumen_meta::{
    ArtworkKind, ArtworkRef, FieldGroup, LangTag, MetadataBundle, MetadataFragment,
    ProviderRanking, Source, merge_fragments, select_artwork,
};
use proptest::prelude::*;

// ---------- generators ----------

/// A small, fixed vocabulary rather than arbitrary strings: what matters for these invariants is the
/// *shape* of overlap between fragments (same key, different providers), not the space of possible
/// UTF-8 field names, and a small vocabulary makes collisions (the interesting case) common instead of
/// vanishingly rare.
fn field_key_strategy() -> impl Strategy<Value = (FieldGroup, &'static str)> {
    prop_oneof![
        Just((FieldGroup::Titles, "title")),
        Just((FieldGroup::Descriptions, "overview")),
        Just((FieldGroup::Ratings, "rating")),
        Just((FieldGroup::ExternalIds, "imdb_id")),
    ]
}

fn provider_id_strategy() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("tmdb"), Just("trakt"), Just("nfo-plugin"), Just("anidb")]
}

fn value_strategy() -> impl Strategy<Value = String> {
    // Occasionally blank/whitespace-only, which is the specific case `merge_fragments` must refuse to
    // treat as an improvement over a populated field.
    prop_oneof![
        3 => "[a-zA-Z0-9 ]{1,12}",
        1 => Just("".to_string()),
        1 => Just("   ".to_string()),
    ]
}

fn source_strategy() -> impl Strategy<Value = Source> {
    prop_oneof![
        Just(Source::Derived),
        (provider_id_strategy(), 0u8..5).prop_map(|(id, rank)| Source::provider(id, rank)),
        Just(Source::LocalSidecar),
        Just(Source::UserEdit),
    ]
}

fn fragment_strategy() -> impl Strategy<Value = MetadataFragment> {
    (source_strategy(), proptest::collection::vec((field_key_strategy(), value_strategy()), 0..4))
        .prop_map(|(source, fields)| {
            let mut f = MetadataFragment::new(source);
            for ((group, name), value) in fields {
                f = f.with(group, name, value);
            }
            f
        })
}

// ---------- merge_fragments ----------

proptest! {
    /// A locked field is never overwritten, no matter what shows up afterward — locked once, by a user
    /// edit, means every field-group/name key that lock covers stays exactly as it was, forever.
    #[test]
    fn a_lock_survives_any_sequence_of_fragments(
        fragments in proptest::collection::vec(fragment_strategy(), 0..6),
    ) {
        let mut base = MetadataBundle::default();
        base.set_user_edit(FieldGroup::Titles, "title", "Pinned By The User");
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into()]);

        let merged = merge_fragments(&base, &fragments, &ranking);
        prop_assert_eq!(
            merged.value(FieldGroup::Titles, "title"),
            Some("Pinned By The User")
        );
        prop_assert!(merged.get(FieldGroup::Titles, "title").unwrap().locked);
    }

    /// Merging the same set of fragments in any order produces the same bundle — required because
    /// providers answer concurrently and completion order is not something the caller controls.
    #[test]
    fn merge_is_order_independent_for_any_fragment_set(
        fragments in proptest::collection::vec(fragment_strategy(), 0..6),
        seed in 0u64..1000,
    ) {
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into(), "anidb".into()]);
        let forward = merge_fragments(&MetadataBundle::default(), &fragments, &ranking);

        // A cheap deterministic shuffle keyed on `seed`, rather than pulling in a shuffle crate just
        // for this test.
        let mut shuffled = fragments.clone();
        let len = shuffled.len();
        if len > 1 {
            for i in (1..len).rev() {
                let j = (seed as usize).wrapping_add(i).wrapping_mul(2654435761) % (i + 1);
                shuffled.swap(i, j);
            }
        }
        let reordered = merge_fragments(&MetadataBundle::default(), &shuffled, &ranking);
        prop_assert_eq!(forward, reordered);
    }

    /// Re-running the exact same merge against its own output must not change anything — otherwise
    /// every routine re-scrape would write a spurious revision.
    #[test]
    fn merge_is_idempotent_for_any_fragment_set(
        fragments in proptest::collection::vec(fragment_strategy(), 0..6),
    ) {
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into(), "anidb".into()]);
        let once = merge_fragments(&MetadataBundle::default(), &fragments, &ranking);
        let twice = merge_fragments(&once, &fragments, &ranking);
        prop_assert_eq!(once, twice);
    }

    /// An empty or whitespace-only value can never replace whatever a field already held.
    #[test]
    fn a_blank_value_never_overwrites_a_populated_field(
        // Leading alnum guarantees this can never itself be blank -- the point of the test is what
        // happens to an *already-populated* field, which a blank seed would beg the question of.
        first in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,11}",
        fragments in proptest::collection::vec(fragment_strategy(), 0..6),
    ) {
        let seed = MetadataFragment::new(Source::provider("tmdb", 0))
            .with(FieldGroup::Titles, "title", first.clone());
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into(), "anidb".into()]);
        let base = merge_fragments(&MetadataBundle::default(), std::slice::from_ref(&seed), &ranking);
        let before = base.value(FieldGroup::Titles, "title").map(str::to_string);

        let merged = merge_fragments(&base, &fragments, &ranking);
        let after = merged.value(FieldGroup::Titles, "title").map(str::to_string);

        // The field can change to a *non-blank* value from a fragment that outranks the seed; it can
        // never become blank or disappear, since a blank contribution is filtered before it is ever
        // considered a candidate replacement.
        prop_assert!(after.is_some());
        prop_assert!(!after.as_deref().unwrap().trim().is_empty());
        let _ = before; // kept for readability of intent; the real assertion is on `after`.
    }

    /// Every field the bundle ends up with is either the seeded lock or something a fragment actually
    /// contributed — merge never invents a value out of nothing.
    #[test]
    fn every_merged_value_came_from_some_fragment(
        fragments in proptest::collection::vec(fragment_strategy(), 0..6),
    ) {
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into(), "anidb".into()]);
        let merged = merge_fragments(&MetadataBundle::default(), &fragments, &ranking);
        for (key, field) in &merged.fields {
            let contributed = fragments.iter().any(|f| {
                f.fields.get(key).is_some_and(|v| v == &field.value)
            });
            prop_assert!(contributed, "{key:?} = {:?} traces to no fragment", field.value);
        }
    }
}

// ---------- select_artwork ----------

fn artwork_kind_strategy() -> impl Strategy<Value = ArtworkKind> {
    prop_oneof![
        Just(ArtworkKind::Poster),
        Just(ArtworkKind::Backdrop),
        Just(ArtworkKind::Logo),
        Just(ArtworkKind::EpisodeStill),
    ]
}

fn lang_strategy() -> impl Strategy<Value = Option<&'static str>> {
    prop_oneof![Just(Some("en")), Just(Some("ja")), Just(Some("pt-PT")), Just(None)]
}

fn artwork_strategy() -> impl Strategy<Value = ArtworkRef> {
    (artwork_kind_strategy(), 1u32..4000, lang_strategy(), 0.0f32..10.0, 0u32..2000, 0u8..4)
        .prop_map(|(kind, width, lang, rating, votes, provider_rank)| {
            let (ideal, _) = kind.ideal_aspect();
            let height = ((width as f32) / ideal).round().max(1.0) as u32;
            let mut a = ArtworkRef::new(kind, format!("{width}x{height}-{rating}"), width, height)
                .with_rating(rating, votes);
            if let Some(l) = lang {
                a = a.with_language(l);
            }
            a.provider_rank = provider_rank;
            a
        })
}

proptest! {
    /// `select_artwork` never returns a candidate whose kind does not match what was asked for — the
    /// pool filter at the top of the function is the whole safety net for this, so it is worth pinning
    /// down directly rather than trusting every call site downstream to notice a mix-up.
    #[test]
    fn selection_never_returns_the_wrong_kind(
        candidates in proptest::collection::vec(artwork_strategy(), 0..10),
        kind in artwork_kind_strategy(),
    ) {
        let wanted = vec![LangTag::new("en")];
        if let Some((idx, _)) = select_artwork(&candidates, kind, &wanted) {
            prop_assert_eq!(candidates[idx].kind, kind);
        }
    }

    /// Selecting from an empty candidate list, or a list with nothing of the requested kind, is always
    /// `None` — never a panic, never a spurious index.
    #[test]
    fn selection_over_no_matching_candidates_is_none(
        candidates in proptest::collection::vec(artwork_strategy(), 0..10),
    ) {
        let none_of_this_kind: Vec<ArtworkRef> =
            candidates.into_iter().filter(|a| a.kind != ArtworkKind::AlbumCover).collect();
        prop_assert_eq!(
            select_artwork(&none_of_this_kind, ArtworkKind::AlbumCover, &[LangTag::new("en")]),
            None
        );
    }

    /// The same candidate pool in a different order must choose the same URL — a client and a server
    /// resolving the same library must never disagree just because their fetches completed in a
    /// different sequence.
    #[test]
    fn selection_is_order_independent(
        candidates in proptest::collection::vec(artwork_strategy(), 1..8),
        kind in artwork_kind_strategy(),
        seed in 0u64..1000,
    ) {
        let wanted = vec![LangTag::new("en")];
        let forward = select_artwork(&candidates, kind, &wanted).map(|(i, _)| candidates[i].url.clone());

        let mut shuffled = candidates.clone();
        let len = shuffled.len();
        if len > 1 {
            for i in (1..len).rev() {
                let j = (seed as usize).wrapping_add(i).wrapping_mul(2654435761) % (i + 1);
                shuffled.swap(i, j);
            }
        }
        let reordered = select_artwork(&shuffled, kind, &wanted).map(|(i, _)| shuffled[i].url.clone());
        prop_assert_eq!(forward, reordered);
    }
}
