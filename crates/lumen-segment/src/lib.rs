//! HLS segmented delivery: playlist building, segment-duration planning, and the `ffmpeg` invocation
//! that actually cuts a source into segments.
//!
//! **HLS only. DASH-MPD packaging is real, separate future work, not attempted here** -- despite this
//! being the crate a wider "HLS/DASH segmented delivery" plan pointed at, building a correct DASH
//! `MPD` manifest is its own substantial piece of work (a different XML schema, different segment
//! addressing modes, different live-vs-VOD semantics) and folding it in half-built would be worse than
//! naming the gap plainly.
//!
//! **Packaging only, not wired into `lumen serve`.** Nothing here opens a socket or serves a byte:
//! this crate builds playlists and runs `ffmpeg` to produce segment files on disk, the same
//! stage `lumen-discovery` was at before its own Stage 1 wired SSDP and DIDL-Lite browsing into
//! `lumen serve`'s HTTP surface. Actually serving `.m3u8`/`.ts`/`.m4s` files to a real HLS client --
//! byte-range-free, plain small-file HTTP responses very unlike `remote/server/http.rs`'s own
//! large-file range-serving concern -- is the next real step, not solved by this crate.
//!
//! **Segmenting only, never a re-encode.** Every job stream-copies; see [`command`]'s own module doc
//! for the exact boundary this shares with `lumen-exec`.

pub mod command;
pub mod plan;
pub mod playlist;

pub use command::{
    HlsExecError, HlsExecOutcome, HlsSegmentJob, SegmentFormat, build_command, execute,
};
pub use plan::segment_durations;
pub use playlist::{InitSegment, MasterPlaylist, MediaPlaylist, Rendition, Segment};
