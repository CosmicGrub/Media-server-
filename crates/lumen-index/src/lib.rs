//! A persistent, incremental library index -- `docs/15-next-generation-engines.md` §A.
//!
//! `lumen scan` and `lumen serve` currently re-walk and re-probe an entire library on every
//! invocation; `server.rs` holds one in-memory snapshot from startup that never refreshes. This crate
//! is the fix: it remembers what it already knows about each file and asks a caller to re-probe only
//! what a cheap `(size, mtime)` check says has actually changed.
//!
//! **What this crate is not, on purpose.** It does not walk a filesystem -- `lumen-play`'s
//! `scan.rs` already has a careful, tested walker, and a second one here would duplicate exactly the
//! kind of logic `CONTRIBUTING.md`'s Rule 1 warns against forking. It does not fetch metadata from
//! any provider -- no such provider exists yet in this codebase (every scraper in `docs/14` is still
//! aspirational, gated on the plugin runtime). What it does today is exercise `lumen-meta`'s
//! field-merge system for real, for the one fragment source that *is* real right now: the filename
//! parse. A provider fragment slots in later at [`IndexRecord::bundle`] without changing this
//! crate's shape at all.
//!
//! ```
//! use lumen_index::{Index, ProbeResult};
//! use lumen_identity::ContentSketch;
//! use std::path::PathBuf;
//!
//! let mut index = Index::default();
//! let observed = vec![PathBuf::from("/dev/null")]; // stands in for a real media path
//! let report = index.reindex(&observed, |path, _fingerprint| {
//!     Some(ProbeResult {
//!         title: path.to_string_lossy().into_owned(),
//!         year: None,
//!         sketch: Some(ContentSketch(1)),
//!         needs_review: None,
//!     })
//! });
//! assert!(report.is_ok());
//! ```

#![forbid(unsafe_code)]

mod fingerprint;
mod persist;
mod store;

pub use fingerprint::fs_fingerprint;
pub use persist::{load, save};
pub use store::{Index, IndexRecord, ProbeResult, ReindexReport};
