//! HLS and DASH segmented delivery: playlist/manifest building, segment-duration planning, and the
//! `ffmpeg` invocations that actually cut a source into each format's own segments.
//!
//! **Both HLS and DASH-MPD packaging are real here.** This crate started HLS-only, with DASH-MPD named
//! as separate future work -- [`dash`] is that work, following [`command`]'s own established shape
//! rather than reinventing it: a different XML schema, different segment addressing (`SegmentTemplate`/
//! `SegmentTimeline` instead of an `#EXTINF` playlist), and independent per-representation segment
//! counts DASH allows and HLS's single shared timeline does not -- but the same "stream-copy, then
//! verify what really landed on disk before trusting it" posture throughout.
//!
//! **Packaging only; wired into `lumen serve`'s HTTP surface, not owned by it.** This crate builds
//! playlists/manifests and runs `ffmpeg` to produce segment files on disk -- `lumen-play`'s
//! `remote/server/hls.rs` and `remote/server/dash.rs` are what actually serve those files to a real
//! client (lazy generation, on-disk caching, authentication), the same relationship `lumen-discovery`
//! has to the SSDP/DIDL-Lite surface `lumen serve` wires it into.
//!
//! **Segmenting only, never a re-encode.** Every job stream-copies; see [`command`]'s own module doc
//! for the exact boundary this shares with `lumen-exec`, and [`dash`]'s own doc for how DASH inherits
//! the identical limitation.

pub mod command;
pub mod dash;
pub mod plan;
pub mod playlist;

pub use command::{
    HlsExecError, HlsExecOutcome, HlsSegmentJob, SegmentFormat, build_command, execute,
};
pub use dash::{
    DashExecError, DashExecOutcome, DashSegmentJob, build_command as build_dash_command,
    execute as execute_dash,
};
pub use plan::segment_durations;
pub use playlist::{InitSegment, MasterPlaylist, MediaPlaylist, Rendition, Segment};
