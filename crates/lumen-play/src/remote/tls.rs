//! Transport encryption for `lumen serve`.
//!
//! The pairing code and the token it mints (see `pairing.rs`) used to travel in plain text — anyone
//! on the same LAN segment able to sniff traffic during a pairing, or replay/inject packets, could
//! read the durable bearer token that grants control of the player, or tamper with commands in
//! flight. That is worth taking seriously for exactly the reason `pairing.rs`'s module doc gives for
//! taking pairing itself seriously: opening a LAN port is new attack surface a CLI test harness
//! never had.
//!
//! Full CA-issued TLS is the wrong shape here — there is no domain name to issue a certificate for
//! and no CA a home LAN server should be trusting. Instead: the server generates one self-signed
//! certificate the first time it runs and keeps it, and prints its fingerprint on the terminal right
//! beside the pairing code. A client pins that fingerprint the same moment it is shown the code —
//! trust-on-first-use, the same model SSH host keys use — and refuses to reconnect if a future
//! connection ever presents a different certificate. That catches exactly the LAN-attacker threat
//! model `pairing.rs` is scoped to: someone who was not shown the code (or the fingerprint) cannot
//! silently swap in their own server, and someone who was not present for the *first* pairing cannot
//! read the token off the wire on a later reconnect.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::remote::pairing::dirs_next_config_dir;

/// How long a freshly generated certificate is valid for -- `rcgen::generate_simple_self_signed`'s
/// own default is a `not_after` of year 4096, which is not a validity period so much as "never
/// expires," and `docs/15-next-generation-engines.md` §D's health check exists specifically to warn a
/// client before a pinned certificate's expiry becomes a hard connection failure. 825 days is not
/// arbitrary: it is the historical CA/Browser Forum ceiling on public TLS certificate lifetime, a
/// well-understood number to reach for rather than inventing a new one for a self-signed LAN cert.
const CERT_VALIDITY_DAYS: u64 = 825;

/// A self-signed certificate and its private key, generated once and reused across restarts.
pub struct ServerCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    /// `None` for a certificate persisted before this field existed -- see `load_or_generate`. A
    /// health check reading this reports "unknown", never a fabricated date.
    expires_at: Option<SystemTime>,
}

impl ServerCert {
    /// Load a persisted cert/key pair from `dir`, or generate and persist a new one if none exists
    /// yet.
    ///
    /// Persisted rather than regenerated on every start: a fingerprint a client pinned on first pair
    /// has to keep matching across restarts, or every reconnect would look identical to the exact
    /// attack pinning exists to catch — a server presenting a certificate nobody actually verified.
    pub fn load_or_generate(dir: &Path) -> Result<Self, String> {
        let cert_path = dir.join("tls-cert.der");
        let key_path = dir.join("tls-key.der");
        let expiry_path = dir.join("tls-cert-expires.txt");

        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            if !cert_der.is_empty() && !key_der.is_empty() {
                // Missing, unreadable, unparsable, or too large to represent as a `SystemTime` at
                // all (a corrupted or hand-edited sidecar, a torn write) is all the same outcome
                // here: unknown expiry, not a fabricated date and never a reason to fail loading an
                // otherwise-good cert -- see `expiry_from_secs`, which is what keeps a value that
                // merely parses as a valid `u64` from overflowing straight into a startup panic.
                let expires_at = std::fs::read_to_string(&expiry_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .and_then(expiry_from_secs);
                return Ok(Self { cert_der, key_der, expires_at });
            }
        }

        // No hostname to speak of on a home LAN; the subject name is never checked by a client that
        // pins the fingerprint instead of doing hostname/CA validation, so any fixed placeholder does.
        //
        // Built from `CertificateParams` directly rather than `generate_simple_self_signed` so the
        // validity window can be set explicitly (see `CERT_VALIDITY_DAYS`) instead of inheriting that
        // helper's year-4096 default -- otherwise there would be nothing for an expiry health check
        // to ever meaningfully report.
        let key_pair =
            rcgen::KeyPair::generate().map_err(|e| format!("cannot generate a TLS key: {e}"))?;
        let mut params = rcgen::CertificateParams::new(vec!["lumen-serve".to_string()])
            .map_err(|e| format!("cannot set up TLS certificate parameters: {e}"))?;
        let now = SystemTime::now();
        // A day of slack behind `now` tolerates ordinary clock skew between this machine and
        // whatever a client checks the cert against; a validity window that starts in the strict
        // present rejects a client whose clock is even slightly behind.
        let not_before = now - Duration::from_secs(24 * 60 * 60);
        let not_after = now + Duration::from_secs(CERT_VALIDITY_DAYS * 24 * 60 * 60);
        params.not_before = system_time_to_offset(not_before)?;
        params.not_after = system_time_to_offset(not_after)?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| format!("cannot generate a TLS certificate: {e}"))?;
        let cert_der = cert.der().to_vec();
        let key_der = key_pair.serialize_der();

        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        // Temp-file-then-rename, the same pattern `lumen_index::save`/`TokenStore::persist_all`
        // already established: a crash or disk-full mid-write leaves the previous file (if any)
        // intact rather than a truncated one a later load would have to guess about -- torn writes
        // are exactly how a corrupted-but-parsable expiry sidecar (see `expiry_from_secs`) happens.
        write_atomic(&cert_path, &cert_der)
            .map_err(|e| format!("cannot persist {}: {e}", cert_path.display()))?;
        write_atomic(&key_path, &key_der)
            .map_err(|e| format!("cannot persist {}: {e}", key_path.display()))?;
        let expires_secs = not_after
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock is before the unix epoch: {e}"))?
            .as_secs();
        // The cert and key just written above are load-bearing -- this server cannot run without
        // them, so a failure to persist either is fatal, correctly propagated with `?`. The expiry
        // sidecar is purely diagnostic (`docs/15` §D's health check), and a `lumen serve` that
        // otherwise started up cleanly must not refuse to run at all just because this one nice-to-
        // have file could not be written; warn and keep going with the in-memory value for this
        // session, the same "persistence failed, the thing the user is looking at right now should
        // not fail because of it" posture `server.rs` already uses for a failed pairing-token save.
        if let Err(e) = write_atomic(&expiry_path, expires_secs.to_string().as_bytes()) {
            eprintln!("warning: could not persist {}: {e}", expiry_path.display());
        }

        // Rebuilt from the same whole-second value just persisted, rather than keeping `not_after`'s
        // own sub-second precision, so a fresh `ServerCert` and one reloaded from disk report exactly
        // the same `expires_at` -- the sidecar file only ever round-trips whole seconds anyway. Using
        // the same checked construction as the reload path rather than assuming it must fit: `secs`
        // came from a `not_after` this same function just computed as `now + a bounded number of
        // days`, so it is not expected to overflow, but "not expected to" is not a proof, and the
        // reload path already has to handle exactly this gracefully.
        let expires_at = expiry_from_secs(expires_secs);

        Ok(Self { cert_der, key_der, expires_at })
    }

    /// When this certificate stops being valid, if known -- `docs/15` §D reads this for the health
    /// report's expiry warning. `None` only for a certificate persisted before this field existed;
    /// every certificate generated by this build sets it.
    pub fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    /// SHA-256 of the DER certificate, colon-hex like `openssl x509 -fingerprint` prints. Shown
    /// beside the pairing code so a person can compare it against what a client displays on first
    /// connect, and pin it there — this fingerprint, not a certificate authority, is this server's
    /// entire identity as far as a client is concerned.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(&self.cert_der);
        digest.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
    }

    /// Build a rustls server config that presents this certificate to every connection.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, String> {
        let cert_chain = vec![CertificateDer::from(self.cert_der.clone())];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()));
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .map_err(|e| format!("cannot build a TLS server config: {e}"))?;
        Ok(Arc::new(config))
    }

    /// Where the certificate lives: beside the pairing token store, in this user's own config
    /// directory rather than the binary's — see `pairing::TokenStore::default_path` for the same
    /// reasoning, and `dirs_next_config_dir` for why it is shared with that module rather than
    /// duplicated.
    pub fn default_dir() -> PathBuf {
        dirs_next_config_dir().join("lumen")
    }
}

/// `UNIX_EPOCH + Duration::from_secs(secs)` panics -- crashing the whole `lumen serve` process on
/// startup -- the moment `secs` is large enough that the resulting instant cannot be represented as
/// a `SystemTime` at all. Any `u64` up to ~1.8×10^19 parses successfully from a corrupted, hand-
/// edited, or torn-write sidecar file, so "parses as a number" is nowhere near "safe to add". `None`
/// here folds into the same "unknown expiry" outcome every other kind of bad sidecar data already
/// produces, never a hard failure over what is, in the end, a purely diagnostic value.
fn expiry_from_secs(secs: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Temp-file-then-rename: the destination path is only ever either the previous complete file or the
/// new complete one, never a truncated write a crash or a full disk caught partway through.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Install rustls's crypto backend once per process. Every `ServerConfig`/`ClientConfig` needs a
/// provider installed before it can be built; calling this more than once (e.g. from tests that spin
/// up several servers) is harmless — a second install failing because one is already there is exactly
/// the outcome that makes this safe to call unconditionally rather than threading a "have I already
/// done this" flag through every caller.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// `rcgen`'s `not_before`/`not_after` are `time::OffsetDateTime`, not `std::time::SystemTime` --
/// `time` is already in the dependency graph as one of `rcgen`'s own dependencies (see
/// `Cargo.toml`'s comment on the direct dependency this function needs), so this is a conversion
/// between two representations already present, not a reason to add anything new.
fn system_time_to_offset(t: SystemTime) -> Result<time::OffsetDateTime, String> {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock is before the unix epoch: {e}"))?
        .as_secs();
    time::OffsetDateTime::from_unix_timestamp(secs as i64)
        .map_err(|e| format!("{secs} is not a representable certificate date: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumen-tls-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_fresh_certificate_is_generated_and_persisted() {
        let dir = temp_dir("fresh");
        let cert = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        assert!(!cert.cert_der.is_empty());
        assert!(!cert.key_der.is_empty());
        assert!(dir.join("tls-cert.der").exists());
        assert!(dir.join("tls-key.der").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_reuses_the_same_certificate_rather_than_generating_a_new_one() {
        let dir = temp_dir("reuse");
        let first = ServerCert::load_or_generate(&dir).expect("first run must succeed");
        let second = ServerCert::load_or_generate(&dir).expect("second run must succeed");
        // Same fingerprint across "restarts" is the entire point: a client that pinned the first
        // run's fingerprint must still trust the server after it restarts.
        assert_eq!(first.fingerprint(), second.fingerprint());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fingerprint_is_a_64_character_colon_separated_hex_digest() {
        let dir = temp_dir("fingerprint-shape");
        let cert = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        let fp = cert.fingerprint();
        let bytes: Vec<&str> = fp.split(':').collect();
        assert_eq!(bytes.len(), 32, "SHA-256 is 32 bytes: {fp}");
        assert!(bytes.iter().all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_config_can_be_built_from_a_generated_certificate() {
        install_crypto_provider();
        let dir = temp_dir("server-config");
        let cert = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        cert.server_config().expect("a freshly generated cert must produce a valid server config");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_certificate_expires_roughly_825_days_out_not_never() {
        let dir = temp_dir("expiry-fresh");
        let cert = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        let expires_at = cert.expires_at().expect("a freshly generated cert must record an expiry");
        let secs_out = expires_at.duration_since(SystemTime::now()).unwrap().as_secs();
        let expected = CERT_VALIDITY_DAYS * 24 * 60 * 60;
        // Within a minute of the expected window either way -- exact to the second would be a flaky
        // assertion against wall-clock time elapsed during the test itself.
        assert!(secs_out.abs_diff(expected) < 60, "expected ~{expected}s out, got {secs_out}s");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_reuses_the_persisted_expiry_rather_than_recomputing_it() {
        let dir = temp_dir("expiry-reuse");
        let first = ServerCert::load_or_generate(&dir).expect("first run must succeed");
        let second = ServerCert::load_or_generate(&dir).expect("second run must succeed");
        assert_eq!(first.expires_at(), second.expires_at());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_certificate_predating_expiry_tracking_reports_unknown_rather_than_a_fabricated_date() {
        let dir = temp_dir("expiry-legacy");
        // Simulates a cert/key pair written by a build before this field existed: present, valid,
        // just with no `tls-cert-expires.txt` sidecar sitting beside it.
        let legacy = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        std::fs::remove_file(dir.join("tls-cert-expires.txt")).unwrap();

        let reloaded = ServerCert::load_or_generate(&dir).expect("reload must still succeed");
        assert_eq!(reloaded.fingerprint(), legacy.fingerprint(), "the cert itself is untouched");
        assert_eq!(reloaded.expires_at(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupted_oversized_expiry_sidecar_reports_unknown_rather_than_panicking() {
        // Regression: a sidecar value that parses fine as a `u64` but is too large to represent as a
        // `SystemTime` (a bit-flipped file, a stray extra digit, a torn write that duplicated
        // digits) used to panic on `UNIX_EPOCH + Duration::from_secs(secs)`, crashing the entire
        // `lumen serve` process at startup on every restart until a human fixed the file by hand.
        let dir = temp_dir("expiry-corrupt-oversized");
        let original = ServerCert::load_or_generate(&dir).expect("generation must succeed");
        std::fs::write(dir.join("tls-cert-expires.txt"), u64::MAX.to_string()).unwrap();

        let reloaded = ServerCert::load_or_generate(&dir)
            .expect("a corrupted sidecar must degrade gracefully, never fail cert loading");
        assert_eq!(reloaded.fingerprint(), original.fingerprint(), "the cert itself is untouched");
        assert_eq!(reloaded.expires_at(), None, "unrepresentable, not fabricated as some date");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expiry_from_secs_never_panics_across_the_full_u64_range() {
        assert!(expiry_from_secs(0).is_some());
        assert!(expiry_from_secs(1_700_000_000).is_some());
        assert_eq!(expiry_from_secs(u64::MAX), None, "must degrade, never overflow-panic");
    }

    #[test]
    fn a_failed_expiry_sidecar_write_does_not_stop_the_cert_from_being_generated_and_returned() {
        // Regression: the expiry sidecar is purely diagnostic (docs/15 §D) -- a failure to persist
        // it must never take down the whole TLS bootstrap when the load-bearing cert and key
        // themselves wrote successfully. Isolated to just the sidecar's own write by pre-occupying
        // its temp-file path with a directory, which every platform refuses to write file contents
        // into -- `tls-cert.tmp`/`tls-key.tmp` are untouched, so their writes and renames proceed
        // normally.
        let dir = temp_dir("expiry-write-fails");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("tls-cert-expires.tmp")).unwrap();

        let cert = ServerCert::load_or_generate(&dir)
            .expect("a cert/key write succeeding must be enough, even if the sidecar write fails");
        assert!(dir.join("tls-cert.der").exists(), "the load-bearing cert must still be persisted");
        assert!(dir.join("tls-key.der").exists(), "the load-bearing key must still be persisted");
        assert!(
            !dir.join("tls-cert-expires.txt").exists(),
            "the sidecar genuinely failed to write -- this test would prove nothing otherwise"
        );
        assert!(
            cert.expires_at().is_some(),
            "this session still knows its own expiry even though the write to disk failed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
