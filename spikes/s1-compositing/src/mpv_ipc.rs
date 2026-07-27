//! mpv JSON-IPC client.
//!
//! Counters are read over mpv's IPC socket rather than parsed out of log output. Two reasons:
//! `--dump-stats` format varies between builds, and Lua scripting is unavailable in the LGPL-only
//! build the product will actually ship (`native/mpv.config` sets `-Dlua=disabled`). IPC is present
//! in every build and its protocol is stable.
//!
//! Deliberately dependency-free: a Unix socket via `std::os::unix::net`, a Windows named pipe via
//! `std::fs::File`. Both are line-delimited JSON, and the replies this needs are simple enough to
//! read without a JSON crate.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use crate::pacing::Sample;

/// mpv properties polled each interval. Named here rather than inline so the list is one thing to
/// audit against `mpv --list-properties` when a build changes.
///
/// [`MpvIpc::sample`] reads through this list rather than repeating the names, so the constant is the
/// actual source and cannot drift into being a stale comment.
pub const PROPERTIES: &[&str] =
    &["frame-drop-count", "vo-delayed-frame-count", "avsync", "estimated-vf-fps"];

const P_DROPS: usize = 0;
const P_DELAYED: usize = 1;
const P_AVSYNC: usize = 2;
const P_FPS: usize = 3;

/// Platform-appropriate default IPC path.
pub fn default_ipc_path(stage: &str) -> String {
    if cfg!(windows) {
        format!(r"\\.\pipe\lumen-s1-{stage}")
    } else {
        format!("/tmp/lumen-s1-{stage}.sock")
    }
}

/// Extract a JSON number for `"data"` from a one-line mpv reply.
///
/// mpv's replies are shallow and machine-generated — `{"data":23.976,"request_id":1,"error":"success"}`
/// — so a full JSON parser buys nothing here. A reply reporting an error yields `None`, which the
/// caller treats as "this build does not expose that property" rather than as a failure.
pub fn parse_data_number(line: &str) -> Option<f64> {
    if !line.contains("\"error\":\"success\"") {
        return None;
    }
    let start = line.find("\"data\":")? + "\"data\":".len();
    let rest = &line[start..];
    let end = rest.find([',', '}'])?;
    let value = rest[..end].trim().trim_matches('"');
    match value {
        "true" => Some(1.0),
        "false" => Some(0.0),
        "null" => None,
        v => v.parse().ok(),
    }
}

/// A live connection to a running mpv.
pub struct MpvIpc {
    stream: Box<dyn ReadWrite>,
    request_id: u64,
}

/// `Read + Write` in one object, so the Unix and Windows transports share a type.
pub trait ReadWrite: std::io::Read + std::io::Write + Send {}
impl<T: std::io::Read + std::io::Write + Send> ReadWrite for T {}

impl MpvIpc {
    /// Connect, retrying until `timeout` — mpv creates the socket a moment after launch, so the first
    /// attempt normally fails and that is not an error.
    pub fn connect(path: &str, timeout: Duration) -> std::io::Result<Self> {
        let deadline = Instant::now() + timeout;
        let mut last_err = None;
        while Instant::now() < deadline {
            match Self::try_connect(path) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "mpv IPC did not appear")
        }))
    }

    #[cfg(unix)]
    fn try_connect(path: &str) -> std::io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self { stream: Box::new(stream), request_id: 0 })
    }

    #[cfg(windows)]
    fn try_connect(path: &str) -> std::io::Result<Self> {
        // A Windows named pipe opens as a file. No extra crate needed for the read/write pattern used
        // here, which is strictly request/response.
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { stream: Box::new(file), request_id: 0 })
    }

    #[cfg(not(any(unix, windows)))]
    fn try_connect(_path: &str) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no IPC transport for this platform",
        ))
    }

    /// Read one property. `Ok(None)` means the build does not expose it.
    pub fn get_property(&mut self, name: &str) -> std::io::Result<Option<f64>> {
        self.request_id += 1;
        let cmd = format!(
            "{{\"command\":[\"get_property\",\"{name}\"],\"request_id\":{}}}\n",
            self.request_id
        );
        self.stream.write_all(cmd.as_bytes())?;
        self.stream.flush()?;

        // mpv interleaves unsolicited events with replies; skip anything that is not our answer.
        let mut reader = BufReader::new(&mut self.stream);
        for _ in 0..64 {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.contains(&format!("\"request_id\":{}", self.request_id)) {
                return Ok(parse_data_number(&line));
            }
        }
        Ok(None)
    }

    /// Poll every property once and build a sample.
    ///
    /// A property this build does not expose reads as `0.0` rather than aborting the stage: an older
    /// mpv missing one counter should still yield a usable run on the others.
    pub fn sample(&mut self, at_ms: u64) -> std::io::Result<Sample> {
        let mut v = [0.0f64; PROPERTIES.len()];
        for (slot, name) in v.iter_mut().zip(PROPERTIES) {
            *slot = self.get_property(name)?.unwrap_or(0.0);
        }
        Ok(Sample {
            avsync_s: v[P_AVSYNC],
            ..Sample::new(at_ms, v[P_DROPS].max(0.0) as u64, v[P_DELAYED].max(0.0) as u64, v[P_FPS])
        })
    }

    /// Ask mpv to quit. Best-effort: the process is killed anyway if this fails.
    pub fn quit(&mut self) {
        let _ = self.stream.write_all(b"{\"command\":[\"quit\"]}\n");
        let _ = self.stream.flush();
    }
}

/// The mpv arguments both stages share.
///
/// Held in one place so the baseline and the composited stage cannot drift apart — a comparison
/// between two differently-configured players measures nothing.
pub fn common_mpv_args(clip: &str, ipc_path: &str, seconds: u64) -> Vec<String> {
    vec![
        format!("--input-ipc-server={ipc_path}"),
        // gpu-next is the libplacebo renderer the product ships; measuring `gpu` would measure a
        // different pipeline.
        "--vo=gpu-next".into(),
        "--hwdec=auto-safe".into(),
        // Fullscreen and borderless, so the window manager treats both stages the same way.
        "--fullscreen=yes".into(),
        "--no-border".into(),
        // Deterministic: no resume position, no config file, no scripts from the user's own setup.
        "--no-config".into(),
        "--no-resume-playback".into(),
        "--no-osc".into(),
        "--no-terminal".into(),
        // Loop, so a clip shorter than the run does not end the stage early.
        "--loop-file=inf".into(),
        format!("--length={seconds}"),
        clip.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_replies_yield_their_number() {
        assert_eq!(
            parse_data_number("{\"data\":23.976023,\"request_id\":4,\"error\":\"success\"}"),
            Some(23.976_023)
        );
        assert_eq!(
            parse_data_number("{\"data\":0,\"request_id\":1,\"error\":\"success\"}"),
            Some(0.0)
        );
        assert_eq!(
            parse_data_number("{\"data\":-0.004,\"request_id\":9,\"error\":\"success\"}"),
            Some(-0.004)
        );
    }

    #[test]
    fn field_order_does_not_matter() {
        // mpv does not promise a field order, and assuming one would break on a future build.
        assert_eq!(
            parse_data_number("{\"request_id\":2,\"error\":\"success\",\"data\":42}"),
            Some(42.0)
        );
    }

    #[test]
    fn an_unavailable_property_is_none_rather_than_an_error() {
        // Not every build exposes every counter; a missing one must not fail the run.
        assert_eq!(
            parse_data_number("{\"error\":\"property unavailable\",\"request_id\":3}"),
            None
        );
        assert_eq!(
            parse_data_number("{\"data\":null,\"request_id\":1,\"error\":\"success\"}"),
            None
        );
    }

    #[test]
    fn events_and_malformed_lines_are_ignored() {
        assert_eq!(parse_data_number("{\"event\":\"file-loaded\"}"), None);
        assert_eq!(parse_data_number(""), None);
        assert_eq!(parse_data_number("not json at all"), None);
        assert_eq!(parse_data_number("{\"error\":\"success\"}"), None);
    }

    #[test]
    fn booleans_are_usable_as_numbers() {
        assert_eq!(
            parse_data_number("{\"data\":true,\"request_id\":1,\"error\":\"success\"}"),
            Some(1.0)
        );
        assert_eq!(
            parse_data_number("{\"data\":false,\"request_id\":1,\"error\":\"success\"}"),
            Some(0.0)
        );
    }

    #[test]
    fn both_stages_share_identical_player_configuration() {
        // A comparison between two differently-configured players measures nothing, so the shared
        // arguments must be exactly that.
        let a = common_mpv_args("clip.mkv", "/tmp/a.sock", 120);
        let b = common_mpv_args("clip.mkv", "/tmp/b.sock", 120);
        let strip = |v: Vec<String>| -> Vec<String> {
            v.into_iter().filter(|s| !s.starts_with("--input-ipc-server")).collect()
        };
        assert_eq!(strip(a), strip(b));
    }

    #[test]
    fn the_shared_arguments_pin_the_renderer_and_exclude_user_config() {
        let args = common_mpv_args("clip.mkv", "/tmp/x.sock", 60);
        let joined = args.join(" ");
        assert!(joined.contains("--vo=gpu-next"), "must measure the pipeline that will ship");
        assert!(joined.contains("--no-config"), "the user's own mpv.conf would corrupt the result");
        assert!(joined.contains("--loop-file=inf"), "a short clip must not end the stage early");
        assert_eq!(args.last().map(String::as_str), Some("clip.mkv"), "the file goes last");
    }

    #[test]
    fn ipc_paths_are_distinct_per_stage_and_platform_appropriate() {
        let a = default_ipc_path("baseline");
        let b = default_ipc_path("composited");
        assert_ne!(a, b, "two concurrent stages must not share a socket");
        if cfg!(windows) {
            assert!(a.starts_with(r"\\.\pipe\"), "{a}");
        } else {
            assert!(a.starts_with('/'), "{a}");
        }
    }

    #[test]
    fn the_property_list_covers_what_the_verdict_needs() {
        for needed in ["vo-delayed-frame-count", "estimated-vf-fps", "avsync"] {
            assert!(
                PROPERTIES.contains(&needed),
                "{needed} is used by the verdict but never polled"
            );
        }
    }

    #[test]
    fn the_property_indices_match_the_property_list() {
        // `sample` reads positionally. Reordering PROPERTIES without moving these would silently
        // record the frame rate as the drop count — numbers that look plausible and mean nothing.
        assert_eq!(PROPERTIES[P_DROPS], "frame-drop-count");
        assert_eq!(PROPERTIES[P_DELAYED], "vo-delayed-frame-count");
        assert_eq!(PROPERTIES[P_AVSYNC], "avsync");
        assert_eq!(PROPERTIES[P_FPS], "estimated-vf-fps");
    }
}
