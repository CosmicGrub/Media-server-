//! The playback decision ladder and track auto-selection.
//!
//! Per ADR-0004 this crate is compiled into every shell *and* the server, so a client planning
//! locally and a server planning on its behalf always reach the same answer. Divergence here shows
//! up to users as unexplained transcoding, which is the single loudest complaint about the
//! incumbents (`docs/01` gap G1).
//!
//! ```
//! use lumen_caps::ClientCapabilities;
//! use lumen_playback::{plan, select, Tier, TrackPreferences};
//! # use lumen_model::{Container, MediaSource, Transport};
//! # let source = MediaSource::new(Container::Matroska, Transport::Local);
//! let caps = ClientCapabilities::reference_native();
//! let selection = select(&source, &TrackPreferences::default(), &caps);
//! let outcome = plan(&source, selection, &caps);
//! assert_eq!(outcome.tier, Tier::T5Blocked); // an empty source has nothing to play
//! assert!(!outcome.explain().is_empty(), "every outcome carries a reason");
//! ```

#![forbid(unsafe_code)]

pub mod ladder;
pub mod plan;
pub mod reason;
pub mod select;

pub use ladder::{Selection, plan};
pub use plan::{
    AudioPath, ContainerPlan, PlaybackPlan, Rejection, SubtitleDelivery, Tier, VideoPath,
    VideoTranscodeSpec,
};
pub use reason::{BitrateCause, BurnInCause, RejectReason};
pub use select::{TrackPreferences, select};
