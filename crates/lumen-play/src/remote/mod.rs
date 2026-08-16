//! `lumen serve`: a persistent, remotely controllable player. See `server` for the concurrency
//! model and `protocol` for the wire format.

pub mod pairing;
pub mod protocol;
pub mod server;
pub mod tls;
