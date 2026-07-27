//! Content-based container detection and structural layout analysis.
//!
//! Implements the two rules that precede everything in `docs/12`:
//!
//! 1. **Content probing, never extension trust** — see [`magic`].
//! 2. **Unknown is not fatal** — every parser here skips what it does not recognise and returns
//!    partial findings rather than an error.
//!
//! Plus the structural analysis that decides *how* a file must be opened ([`ebml`], [`isobmff`]) and
//! the recovery ladder that escalates when a normal open fails ([`recovery`]).
//!
//! Nothing here decodes. These are the questions that must be answered before a decoder is handed
//! anything, and each corresponds to a documented way other players fail.

#![forbid(unsafe_code)]

pub mod ebml;
pub mod isobmff;
pub mod magic;
pub mod recovery;

pub use ebml::{CompressionAlgo, CuesPlacement, MatroskaLayout};
pub use isobmff::{EncryptionScheme, IsobmffLayout, MoovPlacement};
pub use magic::{Candidate, Confidence, sniff};
pub use recovery::{OpenFailure, RecoveryLadder, Rung};
