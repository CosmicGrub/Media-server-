//! Who is allowed to control this player.
//!
//! The moment `lumen serve` opens a LAN port, that is new attack surface a CLI test harness never
//! had, and it is worth taking as seriously as that sentence implies without over-building it. The
//! model: a short numeric code, shown once on the terminal that started the server, typed once into
//! a client. The client exchanges it for a long opaque token and keeps that instead. The code is
//! single-use and expires after a few minutes; the token is what every later connection presents.
//!
//! This is not a login system. There is no username, no password reset, no revocation UI. It is
//! sized to the actual threat on a home LAN — someone on the same network who was not shown the
//! code should not be able to drive the player — and no further.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a pairing code stays valid if nobody uses it.
pub const CODE_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// A freshly generated pairing code, and when it stops being accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCode {
    pub code: String,
    pub expires_at: SystemTime,
}

/// Six digits, the way a phone's own 2FA prompts already look — the least new UI a user has to
/// learn to type one code from a terminal into an app.
pub fn generate_code(random_u32: u32) -> String {
    format!("{:06}", random_u32 % 1_000_000)
}

/// A 128-bit token, hex-encoded. Long enough that guessing it is not a real strategy on a LAN with a
/// realistic number of connection attempts, short enough to fit on one line of a config file.
pub fn generate_token(random_bytes: [u8; 16]) -> String {
    random_bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum PairResult {
    /// The code matched and had not expired. Carries the token the caller should mint and hand back
    /// — generation is the caller's job, this only judges the code, so a test can supply a fixed
    /// token rather than depend on real randomness to check the judging logic.
    Accepted,
    WrongCode,
    Expired,
}

/// Judge a submitted code against the pending one.
///
/// A pure function over three plain values on purpose: whether a code is accepted must not depend on
/// hidden state, only on what is passed in, so every branch — right code late, wrong code, right
/// code in time — is a one-line test rather than something that needs a running server and a clock
/// to reach.
pub fn judge(pending: &PendingCode, submitted: &str, now: SystemTime) -> PairResult {
    if now > pending.expires_at {
        return PairResult::Expired;
    }
    // Not constant-time. A six-digit code guessed over however many TCP connections a LAN attacker
    // can open in a ten-minute window is already the weak point; shaving microseconds off a string
    // comparison does not change that, and pretending otherwise would be a false sense of rigour.
    if submitted == pending.code { PairResult::Accepted } else { PairResult::WrongCode }
}

/// Caps how many wrong-code guesses the pairing endpoint tolerates in a sliding window, across every
/// connection combined — a fresh `TcpStream` per guess costs an attacker nothing, so the limit has to
/// live here rather than per-connection.
///
/// A six-digit code has a million possibilities; capping guesses at a handful per minute turns
/// "guessable given enough parallel connections" back into "not guessable within the code's
/// lifetime", which is the property the design doc above claims but did not, until this, enforce.
pub struct AttemptLimiter {
    max_attempts: u32,
    window: Duration,
    attempts: Vec<SystemTime>,
}

impl AttemptLimiter {
    pub fn new(max_attempts: u32, window: Duration) -> Self {
        Self { max_attempts, window, attempts: Vec::new() }
    }

    /// The default policy: 5 wrong guesses per minute. Five is enough to cover a mistyped digit or
    /// two; a real attacker needs on the order of 100,000 tries on average to land a six-digit code,
    /// which this limit stretches out to weeks rather than the seconds unlimited parallel connections
    /// would otherwise allow.
    pub fn default_policy() -> Self {
        Self::new(5, Duration::from_secs(60))
    }

    /// Record an attempt at `now` and report whether it is still within budget. Old attempts fall out
    /// of the window as they age, so a burst of guesses does not permanently lock the code out — only
    /// sustained guessing does.
    pub fn record(&mut self, now: SystemTime) -> bool {
        self.attempts.retain(|&t| now.duration_since(t).map(|d| d < self.window).unwrap_or(true));
        if self.attempts.len() as u32 >= self.max_attempts {
            return false;
        }
        self.attempts.push(now);
        true
    }
}

/// Tokens that have successfully paired, persisted so restarting the server does not un-pair every
/// device that already has one.
///
/// One token per line, nothing else — no client name, no last-seen time. Anything beyond "is this
/// token one we minted" is a feature for later, not a reason to complicate the file format now.
#[derive(Debug, Clone, Default)]
pub struct TokenStore {
    tokens: std::collections::HashSet<String>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_valid(&self, token: &str) -> bool {
        self.tokens.contains(token)
    }

    pub fn add(&mut self, token: String) {
        self.tokens.insert(token);
    }

    /// Every currently-valid token, for `lumen unpair`'s listing — never printed in full (see
    /// `main.rs`'s `unpair` command), just enough to let a person recognise which device is which.
    pub fn tokens(&self) -> impl Iterator<Item = &str> {
        self.tokens.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Remove every token matching `needle` exactly, or — if nothing matches exactly — every token
    /// that starts with it, so a person can revoke by typing the short prefix `lumen unpair` prints
    /// rather than the full 32-character secret. Returns the tokens actually removed, so the caller
    /// can report what happened rather than a bare count: revoking nothing and revoking one token
    /// that happened to be empty are different outcomes worth telling apart.
    pub fn remove_matching(&mut self, needle: &str) -> Vec<String> {
        if self.tokens.remove(needle) {
            return vec![needle.to_string()];
        }
        let matches: Vec<String> =
            self.tokens.iter().filter(|t| t.starts_with(needle)).cloned().collect();
        for m in &matches {
            self.tokens.remove(m);
        }
        matches
    }

    pub fn clear(&mut self) -> usize {
        let n = self.tokens.len();
        self.tokens.clear();
        n
    }

    /// Load from disk. A missing or unreadable file is an empty store, not an error — the first run
    /// has no file yet, and a corrupt one should cost re-pairing, not stop the server from starting.
    pub fn load(path: &Path) -> Self {
        let Ok(mut f) = std::fs::File::open(path) else { return Self::new() };
        let mut text = String::new();
        if f.read_to_string(&mut text).is_err() {
            return Self::new();
        }
        Self {
            tokens: text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
        }
    }

    /// Append-only on disk: a token is never removed by this. Rewriting the whole file on every
    /// pairing would also invite a torn write clobbering every previously paired device's token at
    /// once — appending one line cannot lose an earlier one, whatever interrupts it.
    pub fn persist_new(&self, path: &Path, token: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{token}")
    }

    /// Rewrite the whole file to match this store exactly — the operation revocation actually needs,
    /// since removing a token means the file must stop naming it, not just avoid losing others.
    /// Written to a sibling temp file and renamed into place rather than truncated in place, so a
    /// process killed mid-write leaves either the old complete file or the new one, never a half-
    /// written truncation that silently un-pairs every other device too.
    pub fn persist_all(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            let mut sorted: Vec<&str> = self.tokens().collect();
            sorted.sort_unstable();
            for t in sorted {
                writeln!(f, "{t}")?;
            }
        }
        std::fs::rename(&tmp, path)
    }

    /// Where the token file lives: beside the rest of this user's Lumen state, not beside the binary
    /// — the binary may be read-only or on removable media, per `docs`'s installation model.
    pub fn default_path() -> PathBuf {
        let base = dirs_next_config_dir();
        base.join("lumen").join("paired-clients.txt")
    }
}

/// A minimal `dirs`-shaped lookup, hand-written rather than pulling in the crate for one path on
/// three platforms. `pub(crate)` rather than private: `tls.rs` needs the same directory for the
/// persisted server certificate, which belongs beside the token store for the same reason.
pub(crate) fn dirs_next_config_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(std::env::temp_dir)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_always_six_digits() {
        for seed in [0u32, 1, 999_999, 1_000_000, u32::MAX] {
            let code = generate_code(seed);
            assert_eq!(code.len(), 6, "{seed} produced {code:?}");
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn a_token_is_32_lowercase_hex_characters() {
        let token = generate_token([0xAB; 16]);
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(token.starts_with("abababab"));
    }

    #[test]
    fn the_right_code_in_time_is_accepted() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pending = PendingCode { code: "123456".into(), expires_at: now + CODE_LIFETIME };
        assert_eq!(judge(&pending, "123456", now), PairResult::Accepted);
    }

    #[test]
    fn the_wrong_code_is_rejected_even_before_it_would_expire() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pending = PendingCode { code: "123456".into(), expires_at: now + CODE_LIFETIME };
        assert_eq!(judge(&pending, "654321", now), PairResult::WrongCode);
    }

    #[test]
    fn the_right_code_after_it_expired_is_still_rejected() {
        // Expiry is checked first and wins outright — a code correct-but-late must read as
        // "get a new code", not "wrong code", because retyping the same digits will never work.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pending =
            PendingCode { code: "123456".into(), expires_at: now - Duration::from_secs(1) };
        assert_eq!(judge(&pending, "123456", now), PairResult::Expired);
    }

    #[test]
    fn a_token_store_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-pairing-test-{}-{:x}",
            std::process::id(),
            std::ptr::from_ref(&dir_anchor()) as usize
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tokens.txt");

        let mut store = TokenStore::load(&path);
        assert!(!store.is_valid("abc123"), "nothing paired yet");

        store.add("abc123".into());
        store.persist_new(&path, "abc123").unwrap();

        // A fresh load — simulating the server restarting — must still trust it.
        let reloaded = TokenStore::load(&path);
        assert!(reloaded.is_valid("abc123"), "a restart must not un-pair an existing device");
        assert!(!reloaded.is_valid("someone-elses-token"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn dir_anchor() -> u8 {
        0
    }

    #[test]
    fn a_missing_token_file_is_an_empty_store_not_an_error() {
        let path = std::env::temp_dir().join("lumen-pairing-definitely-does-not-exist.txt");
        let _ = std::fs::remove_file(&path);
        let store = TokenStore::load(&path);
        assert!(!store.is_valid("anything"));
    }

    #[test]
    fn revoking_an_exact_token_removes_only_that_one() {
        let mut store = TokenStore::new();
        store.add("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        store.add("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        let removed = store.remove_matching("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(removed, vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]);
        assert!(!store.is_valid("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(store.is_valid("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
    }

    #[test]
    fn revoking_by_prefix_matches_the_unambiguous_token_it_names() {
        let mut store = TokenStore::new();
        store.add("abc123abc123abc123abc123abc123a".into());
        store.add("zzz999zzz999zzz999zzz999zzz999z".into());
        let removed = store.remove_matching("abc123");
        assert_eq!(removed, vec!["abc123abc123abc123abc123abc123a".to_string()]);
        assert!(!store.is_valid("abc123abc123abc123abc123abc123a"));
        assert!(store.is_valid("zzz999zzz999zzz999zzz999zzz999z"));
    }

    #[test]
    fn revoking_an_ambiguous_prefix_removes_every_match_and_says_so() {
        // Deliberately not the common case (32 random hex characters make a colliding prefix
        // astronomically unlikely) -- but if it ever happens, silently revoking only one of the two
        // matches would leave the caller believing a token was revoked when it was not.
        let mut store = TokenStore::new();
        store.add("abc111aaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        store.add("abc222bbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        let mut removed = store.remove_matching("abc");
        removed.sort();
        assert_eq!(
            removed,
            vec![
                "abc111aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "abc222bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            ]
        );
        assert!(store.is_empty());
    }

    #[test]
    fn revoking_a_token_that_does_not_exist_removes_nothing() {
        let mut store = TokenStore::new();
        store.add("realtoken0000000000000000000000".into());
        assert!(store.remove_matching("nosuchtoken").is_empty());
        assert!(store.is_valid("realtoken0000000000000000000000"));
    }

    #[test]
    fn clear_revokes_every_token_and_reports_how_many() {
        let mut store = TokenStore::new();
        store.add("a0000000000000000000000000000000".into());
        store.add("b0000000000000000000000000000000".into());
        assert_eq!(store.clear(), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn persist_all_round_trips_a_revocation_to_disk() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-pairing-revoke-test-{}-{:x}",
            std::process::id(),
            std::ptr::from_ref(&dir_anchor()) as usize
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tokens.txt");

        let mut store = TokenStore::new();
        store.add("keep0000000000000000000000000000".into());
        store.add("revoke00000000000000000000000000".into());
        store.persist_all(&path).unwrap();

        store.remove_matching("revoke00000000000000000000000000");
        store.persist_all(&path).unwrap();

        // A restart after the revocation must not resurrect the revoked token from a stale file.
        let reloaded = TokenStore::load(&path);
        assert!(reloaded.is_valid("keep0000000000000000000000000000"));
        assert!(!reloaded.is_valid("revoke00000000000000000000000000"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_attempt_limiter_allows_up_to_the_configured_maximum() {
        let mut limiter = AttemptLimiter::new(3, Duration::from_secs(60));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(limiter.record(now));
        assert!(limiter.record(now));
        assert!(limiter.record(now));
        assert!(!limiter.record(now), "a fourth attempt within the window must be refused");
    }

    #[test]
    fn the_attempt_limiter_forgets_attempts_once_they_age_out_of_the_window() {
        let mut limiter = AttemptLimiter::new(1, Duration::from_secs(60));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(limiter.record(now));
        assert!(!limiter.record(now), "the window has not elapsed yet");

        let later = now + Duration::from_secs(61);
        assert!(limiter.record(later), "the earlier attempt should have aged out by now");
    }
}
