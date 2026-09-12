//! Field-level merge with provenance — `docs/14` §1.2.
//!
//! Providers return fragments; this is the one place they combine. Two rules matter more than the rest:
//!
//! **A locked field is never overwritten.** If a user edited a title, they said so deliberately, and a
//! provider refresh must not undo it. Kodi and Jellyfin both learned this the hard way, and it is the
//! single most-reported metadata complaint in either project.
//!
//! **Every field records where it came from.** That makes a re-scrape a diff rather than a guess, lets
//! the UI explain why a description says what it does, and is what allows the optional AI agent to
//! propose changes safely (`docs/07` §4).

use std::collections::BTreeMap;

use crate::FieldGroup;

/// Where a field's current value came from. Ordered by precedence, lowest first, so `Ord` *is* the
/// merge policy — a higher source always wins, and there is no separate rule table to drift from it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Computed from the file itself: runtime from the probe, colour from the stream.
    Derived,
    /// A metadata provider, with its rank in the user's ordering. Lower rank wins, so it is stored
    /// inverted to keep `Ord` monotonic with precedence.
    Provider { inverted_rank: u8, id: String },
    /// A `.nfo` sidecar. The filesystem is the source of truth (`docs/02` §1.5).
    LocalSidecar,
    /// The user edited it. Beats everything except an explicit lock, which it usually implies.
    UserEdit,
}

impl Source {
    /// Build a provider source from its rank in the user's ordering (0 = most preferred).
    pub fn provider(id: impl Into<String>, rank: u8) -> Self {
        Self::Provider { inverted_rank: u8::MAX - rank, id: id.into() }
    }

    pub fn is_provider(&self) -> bool {
        matches!(self, Self::Provider { .. })
    }

    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Provider { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// Field identity. A string key rather than an enum so a plugin can contribute a field the host does
/// not know about, which `docs/06` requires — a provider must be able to extend the model without a
/// host release.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldKey {
    pub group: FieldGroup,
    pub name: String,
}

impl FieldKey {
    pub fn new(group: FieldGroup, name: impl Into<String>) -> Self {
        Self { group, name: name.into() }
    }
}

/// One merged value plus its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub value: String,
    pub source: Source,
    /// Locked fields are immune to every later merge. Set by a user edit or explicitly by the user.
    pub locked: bool,
}

/// What one provider knew. Fragments never merge with each other inside a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataFragment {
    pub source: Source,
    pub fields: BTreeMap<FieldKey, String>,
}

impl MetadataFragment {
    pub fn new(source: Source) -> Self {
        Self { source, fields: BTreeMap::new() }
    }

    pub fn with(mut self, group: FieldGroup, name: &str, value: impl Into<String>) -> Self {
        self.fields.insert(FieldKey::new(group, name), value.into());
        self
    }
}

/// Per-group provider preference: "titles from TMDB, ratings from Trakt, artwork from Fanart.tv".
#[derive(Debug, Clone, Default)]
pub struct ProviderRanking {
    per_group: BTreeMap<FieldGroup, Vec<String>>,
    default_order: Vec<String>,
}

impl ProviderRanking {
    pub fn new(default_order: Vec<String>) -> Self {
        Self { per_group: BTreeMap::new(), default_order }
    }

    pub fn prefer(mut self, group: FieldGroup, order: Vec<String>) -> Self {
        self.per_group.insert(group, order);
        self
    }

    /// Rank of `provider_id` for `group`. Unlisted providers sort after every listed one, so adding a
    /// provider never silently outranks a configured preference.
    pub fn rank_of(&self, group: FieldGroup, provider_id: &str) -> u8 {
        let order = self.per_group.get(&group).unwrap_or(&self.default_order);
        order
            .iter()
            .position(|p| p == provider_id)
            .map_or(u8::MAX / 2, |i| u8::try_from(i).unwrap_or(u8::MAX / 2))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataBundle {
    pub fields: BTreeMap<FieldKey, Field>,
}

impl MetadataBundle {
    pub fn get(&self, group: FieldGroup, name: &str) -> Option<&Field> {
        self.fields.get(&FieldKey::new(group, name))
    }

    pub fn value(&self, group: FieldGroup, name: &str) -> Option<&str> {
        self.get(group, name).map(|f| f.value.as_str())
    }

    /// Lock a field so no later merge can change it.
    pub fn lock(&mut self, group: FieldGroup, name: &str) {
        if let Some(f) = self.fields.get_mut(&FieldKey::new(group, name)) {
            f.locked = true;
        }
    }

    /// Record a user edit. Implies a lock: a user who typed a value did not do so in order to have it
    /// overwritten on the next refresh.
    pub fn set_user_edit(&mut self, group: FieldGroup, name: &str, value: impl Into<String>) {
        self.fields.insert(
            FieldKey::new(group, name),
            Field { value: value.into(), source: Source::UserEdit, locked: true },
        );
    }

    /// Which providers contributed anything, for the attribution screen that `docs/08` §5 requires.
    pub fn contributing_providers(&self) -> Vec<&str> {
        let mut v: Vec<&str> =
            self.fields.values().filter_map(|f| f.source.provider_id()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Merge fragments into `base`, respecting locks and per-group provider ranking.
///
/// Deterministic and order-independent: the same fragments in any order produce the same bundle, which
/// matters because providers answer concurrently and their completion order is arbitrary.
pub fn merge_fragments(
    base: &MetadataBundle,
    fragments: &[MetadataFragment],
    ranking: &ProviderRanking,
) -> MetadataBundle {
    let mut out = base.clone();

    for fragment in fragments {
        for (key, value) in &fragment.fields {
            if value.trim().is_empty() {
                continue; // an empty overview is not an improvement on a populated one
            }
            // Re-rank the provider for this specific field's group. A provider ranked first for
            // titles may be ranked last for artwork, and the fragment does not know which group each
            // of its own fields belongs to.
            let effective = match &fragment.source {
                Source::Provider { id, .. } => Source::provider(id, ranking.rank_of(key.group, id)),
                other => other.clone(),
            };

            match out.fields.get(key) {
                // A lock is absolute. Not "usually respected", not "unless the provider is better".
                Some(existing) if existing.locked => continue,
                Some(existing) if existing.source > effective => continue,
                // Equal precedence and an identical or "greater" value: keep what is already there.
                // Two providers can land at the same effective rank (both unranked, or the same
                // provider counted twice across a resolution retry), and when they disagree on the
                // value, breaking the tie by *value* rather than by "whichever fragment came first"
                // is what keeps the whole function order-independent — a running max is order-
                // independent by construction, while "first seen wins" is not. Idempotent for the same
                // reason `select_artwork`'s URL tie-break is: comparing a value against itself is a
                // no-op, so re-merging the same fragments never churns the result.
                Some(existing)
                    if existing.source == effective
                        && existing.value.as_str() >= value.as_str() =>
                {
                    continue;
                }
                _ => {}
            }
            out.fields.insert(
                key.clone(),
                Field { value: value.clone(), source: effective, locked: false },
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use FieldGroup as G;

    fn tmdb() -> MetadataFragment {
        MetadataFragment::new(Source::provider("tmdb", 0))
            .with(G::Titles, "title", "Blade Runner")
            .with(G::Descriptions, "overview", "A blade runner must pursue six replicants.")
            .with(G::Ratings, "rating", "8.1")
    }

    fn trakt() -> MetadataFragment {
        MetadataFragment::new(Source::provider("trakt", 1))
            .with(G::Titles, "title", "Blade Runner (Trakt)")
            .with(G::Ratings, "rating", "8.7")
    }

    fn nfo() -> MetadataFragment {
        MetadataFragment::new(Source::LocalSidecar).with(G::Titles, "title", "Blade Runner [1982]")
    }

    #[test]
    fn a_locked_field_survives_every_provider() {
        // The single most-reported metadata complaint about Kodi and Jellyfin.
        let mut base = MetadataBundle::default();
        base.set_user_edit(G::Titles, "title", "My Preferred Title");

        let merged = merge_fragments(&base, &[tmdb(), trakt(), nfo()], &ProviderRanking::default());
        assert_eq!(merged.value(G::Titles, "title"), Some("My Preferred Title"));
        assert!(merged.get(G::Titles, "title").unwrap().locked);
        // Unlocked fields still populate.
        assert!(merged.value(G::Descriptions, "overview").is_some());
    }

    #[test]
    fn a_user_edit_implies_a_lock() {
        // A user who typed a value did not do so in order to have it overwritten on refresh.
        let mut base = MetadataBundle::default();
        base.set_user_edit(G::Descriptions, "overview", "my summary");
        assert!(base.get(G::Descriptions, "overview").unwrap().locked);
    }

    #[test]
    fn a_local_sidecar_outranks_every_provider() {
        // docs/02 §1.5: the filesystem is the source of truth.
        let merged = merge_fragments(
            &MetadataBundle::default(),
            &[tmdb(), nfo()],
            &ProviderRanking::default(),
        );
        assert_eq!(merged.value(G::Titles, "title"), Some("Blade Runner [1982]"));
        assert_eq!(merged.get(G::Titles, "title").unwrap().source, Source::LocalSidecar);
    }

    #[test]
    fn per_group_ranking_lets_different_providers_win_different_fields() {
        // "Titles from TMDB, ratings from Trakt" — the whole point of group-level ranking.
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into()])
            .prefer(G::Ratings, vec!["trakt".into(), "tmdb".into()]);

        let merged = merge_fragments(&MetadataBundle::default(), &[tmdb(), trakt()], &ranking);
        assert_eq!(merged.value(G::Titles, "title"), Some("Blade Runner"), "TMDB wins titles");
        assert_eq!(merged.value(G::Ratings, "rating"), Some("8.7"), "Trakt wins ratings");
    }

    #[test]
    fn merge_is_order_independent() {
        // Providers answer concurrently; their completion order is arbitrary and must not matter.
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into()]);
        let a = merge_fragments(&MetadataBundle::default(), &[tmdb(), trakt(), nfo()], &ranking);
        let b = merge_fragments(&MetadataBundle::default(), &[nfo(), trakt(), tmdb()], &ranking);
        let c = merge_fragments(&MetadataBundle::default(), &[trakt(), nfo(), tmdb()], &ranking);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn merge_is_idempotent() {
        // Re-running a scrape must not churn fields, or every refresh writes a spurious revision.
        let ranking = ProviderRanking::new(vec!["tmdb".into()]);
        let once = merge_fragments(&MetadataBundle::default(), &[tmdb()], &ranking);
        let twice = merge_fragments(&once, &[tmdb()], &ranking);
        assert_eq!(once, twice);
    }

    #[test]
    fn an_empty_value_never_replaces_a_populated_one() {
        // A provider that returns "" for an overview has no overview, not a better one.
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "empty".into()]);
        let populated = merge_fragments(&MetadataBundle::default(), &[tmdb()], &ranking);
        let blanking = MetadataFragment::new(Source::provider("empty", 0)).with(
            G::Descriptions,
            "overview",
            "   ",
        );
        let after = merge_fragments(&populated, &[blanking], &ranking);
        assert_eq!(
            after.value(G::Descriptions, "overview"),
            populated.value(G::Descriptions, "overview")
        );
    }

    #[test]
    fn derived_fields_lose_to_everything_else() {
        // A probed runtime is authoritative for the *file*, but a provider knows the work's runtime,
        // and the two are different questions. Provider wins for the metadata field.
        let derived =
            MetadataFragment::new(Source::Derived).with(G::Titles, "title", "filename.mkv");
        let merged = merge_fragments(
            &MetadataBundle::default(),
            &[derived, tmdb()],
            &ProviderRanking::new(vec!["tmdb".into()]),
        );
        assert_eq!(merged.value(G::Titles, "title"), Some("Blade Runner"));
    }

    #[test]
    fn derived_fills_gaps_no_provider_covered() {
        let derived = MetadataFragment::new(Source::Derived).with(G::Titles, "sort_title", "blade");
        let merged =
            merge_fragments(&MetadataBundle::default(), &[derived], &ProviderRanking::default());
        assert_eq!(merged.value(G::Titles, "sort_title"), Some("blade"));
        assert_eq!(merged.get(G::Titles, "sort_title").unwrap().source, Source::Derived);
    }

    #[test]
    fn an_unlisted_provider_never_outranks_a_configured_one() {
        // Installing a new plugin must not silently take over fields the user assigned.
        let ranking = ProviderRanking::new(vec!["tmdb".into()]);
        let unknown = MetadataFragment::new(Source::provider("brand-new", 0)).with(
            G::Titles,
            "title",
            "Hijacked",
        );
        let merged = merge_fragments(&MetadataBundle::default(), &[tmdb(), unknown], &ranking);
        assert_eq!(merged.value(G::Titles, "title"), Some("Blade Runner"));
    }

    #[test]
    fn provenance_is_recorded_for_every_field() {
        let ranking = ProviderRanking::new(vec!["tmdb".into(), "trakt".into()]);
        let merged = merge_fragments(&MetadataBundle::default(), &[tmdb(), trakt()], &ranking);
        for (key, field) in &merged.fields {
            assert!(field.source.is_provider(), "{key:?} lost its provenance");
        }
        assert_eq!(merged.contributing_providers(), vec!["tmdb"]);
    }

    #[test]
    fn source_ordering_is_the_merge_policy() {
        // Ord on Source *is* the precedence table, so there is no second rule set to drift from it.
        assert!(Source::UserEdit > Source::LocalSidecar);
        assert!(Source::LocalSidecar > Source::provider("x", 0));
        assert!(Source::provider("x", 0) > Source::provider("x", 5), "lower rank wins");
        assert!(Source::provider("x", 200) > Source::Derived);
    }

    #[test]
    fn plugins_can_contribute_fields_the_host_does_not_know() {
        // docs/06: a provider must be able to extend the model without a host release.
        let exotic = MetadataFragment::new(Source::provider("anidb", 0)).with(
            G::ExternalIds,
            "anidb_relation_graph",
            "17617,17618",
        );
        let merged =
            merge_fragments(&MetadataBundle::default(), &[exotic], &ProviderRanking::default());
        assert_eq!(merged.value(G::ExternalIds, "anidb_relation_graph"), Some("17617,17618"));
    }
}
