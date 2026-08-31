//! Core media domain types shared by every Lumen shell and the server.
//!
//! Deliberately dependency-free: these types cross the UniFFI / wasm-bindgen boundary and are
//! constructed from attacker-influenced container metadata, so the surface stays small and total.
//!
//! Spec references: `docs/11-compatibility-charter.md`, `docs/12-container-conformance.md`.

#![forbid(unsafe_code)]

mod codec;
mod color;
mod container;
mod source;
mod stream;

pub use codec::{AudioCodec, Codec, SubtitleCodec, VideoCodec};
pub use color::{
    ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer, HdrFormat, MasteringDisplay,
};
pub use container::Container;
pub use source::{Integrity, MediaSource, Transport};
pub use stream::{
    AudioStream, ChannelLayout, CropRect, FieldOrder, Rational, StereoMode, StreamFlags,
    StreamKind, SubtitleStream, TelecinePattern, VideoStream,
};

/// Language tag. Accepts ISO 639-2 and BCP-47; `und` when absent.
///
/// Matroska carries both `Language` (639-2) and `LanguageBCP47`; per `docs/12` §2.5 BCP-47 wins
/// when present. This type normalises both into a comparable primary subtag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Language(String);

impl Language {
    pub const UNDETERMINED: &'static str = "und";

    pub fn new(tag: &str) -> Self {
        let t = tag.trim().to_ascii_lowercase();
        if t.is_empty() || t == "und" || t == "mis" || t == "zxx" {
            Self(Self::UNDETERMINED.to_string())
        } else {
            Self(t)
        }
    }

    /// Primary subtag: `pt-BR` -> `pt`. Used for preference matching.
    pub fn primary(&self) -> &str {
        self.0.split(['-', '_']).next().unwrap_or(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_undetermined(&self) -> bool {
        self.0 == Self::UNDETERMINED
    }

    /// Two tags match if their primary subtags agree. `und` never matches anything, including `und`
    /// — an unknown language must not satisfy a user's explicit preference.
    pub fn matches(&self, other: &Language) -> bool {
        !self.is_undetermined() && self.primary() == other.primary()
    }
}

impl Default for Language {
    fn default() -> Self {
        Self(Self::UNDETERMINED.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_normalises_and_matches_on_primary_subtag() {
        assert_eq!(Language::new("JPN").as_str(), "jpn");
        assert_eq!(Language::new("pt-BR").primary(), "pt");
        assert!(Language::new("pt-BR").matches(&Language::new("pt-PT")));
        assert!(!Language::new("eng").matches(&Language::new("jpn")));
    }

    #[test]
    fn undetermined_never_matches() {
        // A track with no language must not satisfy an explicit preference, or auto-selection
        // silently picks arbitrary tracks on files with unlabelled streams.
        for tag in ["", "und", "mis", "zxx", "   "] {
            let l = Language::new(tag);
            assert!(l.is_undetermined(), "{tag:?} should be undetermined");
            assert!(!l.matches(&Language::new("und")));
            assert!(!l.matches(&Language::new("eng")));
        }
    }
}
