//! Metadata provider abstraction — `docs/14` §1–§2.
//!
//! Providers (TMDB, TheTVDB, AniList, MusicBrainz, Fanart.tv, NFO sidecars) return **fragments**: the
//! fields each happens to know, tagged with where it came from. Nothing merges inside a provider, so
//! no provider can quietly overwrite another's work and the merge policy lives in one auditable place.
//!
//! Every provider is a Wasm plugin ([ADR-0003]), which is a licensing requirement as much as an
//! architectural one — TMDB and TheTVDB both gate commercial use behind revenue thresholds
//! (`docs/08` §5), so shipping no bundled keys keeps the default install out of the commercial tier.
//!
//! [ADR-0003]: ../../../docs/adr/0003-plugin-runtime.md

#![forbid(unsafe_code)]

pub mod artwork;
pub mod language;
pub mod merge;

pub use artwork::{ArtworkKind, ArtworkRef, select_artwork};
pub use language::{LangTag, resolve_language};
pub use merge::{
    Field, FieldKey, MetadataBundle, MetadataFragment, ProviderRanking, Source, merge_fragments,
};

/// What a provider can supply, declared in its plugin manifest so the host can route queries without
/// calling every provider for everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapability {
    pub id: String,
    /// Item kinds this provider knows about.
    pub kinds: Vec<ItemKind>,
    /// Field groups it can populate. Used to build the per-group ranking in `docs/14` §1.2.
    pub groups: Vec<FieldGroup>,
    /// Artwork kinds it serves, if any.
    pub artwork: Vec<ArtworkKind>,
    /// BCP-47 tags it can return text in. Empty means "unknown, ask and see".
    pub languages: Vec<String>,
    /// The provider requires a user-supplied credential. Almost all of them do, deliberately.
    pub needs_credential: bool,
    /// Attribution text the provider's terms require be displayed (`docs/08` §5).
    pub attribution: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    Movie,
    Series,
    Season,
    Episode,
    Album,
    Track,
    Artist,
    Book,
    Collection,
}

/// Field groups are the unit of provider preference: a user says "titles from TMDB, ratings from
/// Trakt, artwork from Fanart.tv" rather than ranking a hundred individual fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FieldGroup {
    Titles,
    Descriptions,
    Ratings,
    Cast,
    Genres,
    ReleaseDates,
    Certifications,
    Artwork,
    ExternalIds,
    Chapters,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_that_requires_attribution_carries_the_text() {
        // docs/08 §5: TMDB's terms require a specific notice. Losing it is a licence violation, so it
        // travels with the capability rather than living in a UI string somewhere.
        let tmdb = ProviderCapability {
            id: "com.themoviedb".into(),
            kinds: vec![ItemKind::Movie, ItemKind::Series],
            groups: vec![FieldGroup::Titles, FieldGroup::Descriptions, FieldGroup::Artwork],
            artwork: vec![ArtworkKind::Poster, ArtworkKind::Backdrop],
            languages: vec![],
            needs_credential: true,
            attribution: Some(
                "This product uses the TMDb API but is not endorsed or certified by TMDb.".into(),
            ),
        };
        assert!(tmdb.needs_credential, "no bundled keys — users supply their own");
        assert!(tmdb.attribution.as_ref().is_some_and(|a| a.contains("TMDb")));
    }
}
