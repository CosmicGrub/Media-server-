//! Executes a [`lumen_playback::PlaybackPlan`]'s remux by shelling out to an LGPL-only `ffmpeg`
//! build (`native/ffmpeg.config`, ADR-0002) -- the piece `docs/13` §2's remux decision procedure
//! never actually runs anywhere in this workspace until now: everything up to this crate only ever
//! *decides* what a remux should look like.
//!
//! **Stage 1, honestly scoped to remuxing, not transcoding.** [`RemuxJob::from_plan`] accepts a plan
//! only when nothing in it is lossy re-encoding -- see its own doc comment for the exact boundary.
//! A real transcode engine (actual video/audio re-encoding, rate control, hardware encoder selection)
//! is a substantially larger, more speculative piece of work than this crate takes on.
//!
//! **Hand-rolled process management, no dependency.** `std::process::Command` runs `ffmpeg` to
//! completion and captures its exit status and stderr; there is no progress streaming, cancellation,
//! or job queue here -- a real production remux service needs all three, and none of them belong in
//! the crate that only knows how to build one correct `ffmpeg` invocation and run it once.

mod job;
mod run;

pub use job::{AudioAdaptation, PlanNotExecutable, RemuxJob, build_command};
pub use run::{ExecError, ExecOutcome, execute};
