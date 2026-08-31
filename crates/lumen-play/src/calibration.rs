//! Fidelity Telemetry & Calibration Engine -- `docs/15-next-generation-engines.md` §C.
//!
//! The fidelity ladder's own doc comment (`fidelity.rs`) says it plainly: a tier is **modelled, not
//! measured** -- real demux data in, a declared capability profile applied, a projected outcome out.
//! What closes part of that gap needs no hardware lab at all: mpv already knows, for real, whether it
//! decoded a file's video track on the GPU or the CPU (`hwdec-current`), and `session.rs` already
//! queries it for every real playback session (`lumen play`/`lumen test`) -- it just never gets
//! compared to anything. This module is that comparison.
//!
//! **Scope, stated honestly.** The native profile's declared [`lumen_caps::VideoDecodeCaps::hardware`]
//! claim is the one thing checked here. The original design also proposed comparing predicted audio
//! passthrough (TrueHD/DTS-HD MA reaching the AVR as a bitstream) against `audio-out-params` -- left
//! out of this build because it would not measure anything real yet: nothing in `session.rs` asks mpv
//! for spdif passthrough in the first place (no `--audio-spdif` is ever passed), so mpv decodes every
//! bitstream format to PCM by default regardless of what the AVR could do, and every single file would
//! "miss" for a reason that has nothing to do with the fidelity model. Checking a thing this codebase
//! never actually requested would be evidence about a missing feature, not about the model's honesty
//! -- worth doing once passthrough is actually requested, not before.
//!
//! **Strictly local, on purpose.** The log lives at [`default_log_path`], in the same
//! `XDG`-style config directory `TokenStore` and the TLS certificate already use, in plain JSON
//! Lines a person can open and read. Nothing here ever sends it anywhere -- it is a calibration
//! record, not telemetry, and the distinction matters enough to say twice: once here, once in
//! `docs/15`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fidelity;
use crate::json::{self, quote};
use crate::session::{FileResult, Outcome};

/// One played file's predicted-vs-observed hardware decode path.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationEntry {
    pub path: PathBuf,
    pub unix_secs: u64,
    /// FFmpeg's own codec name, as mpv reported it -- kept even when unrecognised, since "we don't
    /// know this one" is itself worth recording.
    pub video_codec: Option<String>,
    /// What the native profile's own declared capabilities say about this codec: `Some(true)` if it
    /// claims hardware decode, `Some(false)` if it explicitly does not (VC-1, in the current table),
    /// `None` if the codec is not recognised at all -- no claim either way, so nothing to compare.
    pub predicted_hardware_decode: Option<bool>,
    /// mpv's raw `hwdec-current` value for this session -- `"no"` for software, a decoder name
    /// (`"videotoolbox"`, `"nvdec"`, ...) for hardware.
    pub observed_hwdec: Option<String>,
    /// Carried along as context, not compared against a prediction -- the ladder does not predict a
    /// frame-drop count, so there is nothing here to call a hit or a miss.
    pub dropped_frames: Option<u64>,
    /// How long this session actually played, in seconds -- the denominator [`drop_rate_flagged`]
    /// needs to turn a raw dropped-frame count into a rate: a long session naturally accumulates more
    /// drops than a short one at the same underlying rate, so the count alone says nothing.
    ///
    /// [`drop_rate_flagged`]: CalibrationEntry::drop_rate_flagged
    pub seconds_played: f64,
    /// The file's own reported frame rate, when known -- the other half of the expected-frame-count
    /// calculation `drop_rate_flagged` needs. `None` for VFR content or a build too old to have
    /// reported it, both of which mean there is nothing to judge a raw drop count against.
    pub expected_fps: Option<f64>,
}

/// Below this many seconds played, any dropped-frame count is noise, not signal -- a session that
/// barely started (an immediate seek away, a quick preview) can report a handful of drops purely
/// from startup that would never recur in normal viewing.
const MIN_SECONDS_FOR_DROP_RATE: f64 = 2.0;

/// A visually perceptible amount of stutter starts well under "most frames are fine" -- 2% is a
/// commonly cited threshold for when dropped frames become noticeable rather than lost in normal
/// playback jitter, and matches the order of magnitude this module's own doc comment already uses
/// elsewhere ("a real, checkable miss," not a statistical curiosity).
const DROP_RATE_THRESHOLD: f64 = 0.02;

impl CalibrationEntry {
    /// `None` when there is nothing to compare (an unrecognised codec, or hardware decode was never
    /// even queried this session). `Some(true)` when what mpv actually did agrees with what the
    /// native profile's own declared capabilities predicted; `Some(false)` on a real, checkable miss.
    pub fn hardware_decode_as_predicted(&self) -> Option<bool> {
        let predicted = self.predicted_hardware_decode?;
        let observed = self.observed_hwdec.as_ref()?;
        let observed_hardware = !observed.is_empty() && observed != "no";
        Some(predicted == observed_hardware)
    }

    /// `Some(true)` when this session's dropped-frame *rate* -- not the raw count -- exceeds
    /// [`DROP_RATE_THRESHOLD`], a real, visually-relevant stutter signal the ladder never predicts a
    /// value for and so cannot be judged against a prediction the way hardware decode is; this is a
    /// threshold check against reality, not a hit/miss against a claim. `None` when there is nothing
    /// to judge: no drop count was reported, the frame rate is unknown (VFR content, or a build too
    /// old to have reported it), or the session played too briefly for a count to mean anything (see
    /// [`MIN_SECONDS_FOR_DROP_RATE`]).
    pub fn drop_rate_flagged(&self) -> Option<bool> {
        let dropped = self.dropped_frames?;
        let fps = self.expected_fps.filter(|f| *f > 0.0)?;
        if self.seconds_played < MIN_SECONDS_FOR_DROP_RATE {
            return None;
        }
        let expected_frames = fps * self.seconds_played;
        let rate = dropped as f64 / expected_frames;
        Some(rate > DROP_RATE_THRESHOLD)
    }

    fn to_json_line(&self) -> String {
        format!(
            "{{\"path\":{},\"unix_secs\":{},\"video_codec\":{},\"predicted_hardware_decode\":{},\
             \"observed_hwdec\":{},\"dropped_frames\":{},\"seconds_played\":{},\
             \"expected_fps\":{}}}",
            quote(&self.path.to_string_lossy()),
            self.unix_secs,
            opt_str(self.video_codec.as_deref()),
            opt_bool(self.predicted_hardware_decode),
            opt_str(self.observed_hwdec.as_deref()),
            opt_num(self.dropped_frames),
            self.seconds_played,
            opt_f64(self.expected_fps),
        )
    }

    fn from_json_line(line: &str) -> Option<Self> {
        let v = json::parse(line).ok()?;
        Some(Self {
            path: PathBuf::from(v.get("path")?.as_str()?),
            unix_secs: v.get("unix_secs")?.as_f64()? as u64,
            video_codec: v.get("video_codec").and_then(json::Value::as_str).map(str::to_string),
            predicted_hardware_decode: v
                .get("predicted_hardware_decode")
                .and_then(json::Value::as_bool),
            observed_hwdec: v
                .get("observed_hwdec")
                .and_then(json::Value::as_str)
                .map(str::to_string),
            dropped_frames: v.get("dropped_frames").and_then(json::Value::as_f64).map(|n| n as u64),
            // Absent in a line written before this field existed -- 0.0 reads back as "too brief to
            // judge" via `MIN_SECONDS_FOR_DROP_RATE`, the same honest "nothing to compare" outcome an
            // old entry genuinely earns rather than a fabricated duration.
            seconds_played: v.get("seconds_played").and_then(json::Value::as_f64).unwrap_or(0.0),
            expected_fps: v.get("expected_fps").and_then(json::Value::as_f64),
        })
    }
}

fn opt_str(s: Option<&str>) -> String {
    s.map_or_else(|| "null".to_string(), quote)
}
fn opt_num(n: Option<u64>) -> String {
    n.map_or_else(|| "null".to_string(), |v| v.to_string())
}
fn opt_bool(b: Option<bool>) -> String {
    b.map_or_else(|| "null".to_string(), |v| v.to_string())
}
fn opt_f64(f: Option<f64>) -> String {
    f.map_or_else(|| "null".to_string(), |v| v.to_string())
}

/// Build a calibration entry from one real playback result -- `None` when there is nothing to record
/// at all: a file that never played, or one with no video track (a music file has no decode path to
/// calibrate).
pub fn observe(r: &FileResult) -> Option<CalibrationEntry> {
    if r.outcome != Outcome::Played {
        return None;
    }
    r.video_codec.as_ref()?;
    Some(CalibrationEntry {
        path: r.path.clone(),
        unix_secs: unix_now(),
        video_codec: r.video_codec.clone(),
        predicted_hardware_decode: predicted_hardware_decode(r),
        observed_hwdec: r.hwdec.clone(),
        dropped_frames: r.dropped_frames,
        seconds_played: r.seconds_played,
        expected_fps: r.fps,
    })
}

/// What the native profile's own declared capability table (the same one `fidelity::assess` plans
/// against) claims about this file's video codec -- reusing `fidelity::video_codec`'s name mapping
/// rather than a second one that could silently drift from the one the actual tier decision uses.
///
/// The codec name has to come from the *selected* video track in `r.tracks` (FFmpeg's short name,
/// e.g. `"h264"`), not from `r.video_codec` -- that field holds mpv's human-readable `video-codec`
/// property (`"H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10"`), which `fidelity::video_codec`'s matcher
/// never recognises. This is the same track `fidelity::assess`'s own `mpv_selection` picks, so a
/// multi-video-track file is judged on the one that actually played, not just the first one listed.
fn predicted_hardware_decode(r: &FileResult) -> Option<bool> {
    let track_codec = r.tracks.iter().find(|t| t.kind == "video" && t.selected)?.codec.as_deref();
    let codec = fidelity::video_codec(track_codec);
    fidelity::native_profile().video_caps_for(&codec).map(|c| c.hardware)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Default location for the calibration log: the same config-directory convention `TokenStore` and
/// the TLS certificate already use.
pub fn default_log_path() -> PathBuf {
    crate::remote::pairing::dirs_next_config_dir().join("lumen").join("calibration.jsonl")
}

/// Append one entry. Never overwrites, never truncates, never transmits anywhere -- every session's
/// worth of evidence stays, so the log's own length over time is itself part of the signal.
pub fn append(path: &Path, entry: &CalibrationEntry) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", entry.to_json_line())
}

/// Read every entry back. A corrupt line is skipped rather than failing the whole read -- the same
/// "one bad entry never takes down the batch" posture `lumen-index`'s persistence uses.
pub fn read_all(path: &Path) -> std::io::Result<Vec<CalibrationEntry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text.lines().filter_map(CalibrationEntry::from_json_line).collect())
}

/// A short, human-readable summary for `lumen doctor` -- turns the raw log into "how often has the
/// hardware-decode prediction actually held", the number the fidelity model's own honesty depends on.
pub fn summarize(entries: &[CalibrationEntry]) -> String {
    let mut s = hardware_decode_summary(entries);
    if let Some(drop_section) = drop_rate_summary(entries) {
        s.push('\n');
        s.push_str(&drop_section);
    }
    s
}

fn hardware_decode_summary(entries: &[CalibrationEntry]) -> String {
    let checkable: Vec<&CalibrationEntry> =
        entries.iter().filter(|e| e.hardware_decode_as_predicted().is_some()).collect();
    if checkable.is_empty() {
        return format!(
            "no calibration data yet ({} session{} logged, none with a codec this build has a hardware-decode claim for)",
            entries.len(),
            if entries.len() == 1 { "" } else { "s" }
        );
    }
    let matched =
        checkable.iter().filter(|e| e.hardware_decode_as_predicted() == Some(true)).count();
    let missed = checkable.len() - matched;
    if missed == 0 {
        format!(
            "{matched}/{} real playback session{} matched the fidelity model's hardware-decode prediction",
            checkable.len(),
            if checkable.len() == 1 { "" } else { "s" }
        )
    } else {
        let mut s = format!(
            "{matched}/{} real playback sessions matched the fidelity model's hardware-decode prediction -- {missed} did not:",
            checkable.len()
        );
        for e in checkable.iter().filter(|e| e.hardware_decode_as_predicted() == Some(false)) {
            let predicted =
                if e.predicted_hardware_decode == Some(true) { "hardware" } else { "software" };
            let observed = match e.observed_hwdec.as_deref() {
                Some("no") | Some("") | None => "software",
                Some(_) => "hardware",
            };
            s.push_str(&format!(
                "\n  {} ({}) -- predicted {predicted}, mpv actually used {observed}",
                e.path.display(),
                e.video_codec.as_deref().unwrap_or("unknown codec"),
            ));
        }
        s
    }
}

/// `None` when no session has anything to judge -- unlike the hardware-decode section above, this is
/// worth omitting entirely rather than printing an empty "no data" line every time, since a build
/// that never reported an fps (an older client, or a run of VFR-only content) would otherwise print
/// two redundant "nothing to compare" sentences back to back.
fn drop_rate_summary(entries: &[CalibrationEntry]) -> Option<String> {
    let checkable: Vec<&CalibrationEntry> =
        entries.iter().filter(|e| e.drop_rate_flagged().is_some()).collect();
    if checkable.is_empty() {
        return None;
    }
    let flagged: Vec<&&CalibrationEntry> =
        checkable.iter().filter(|e| e.drop_rate_flagged() == Some(true)).collect();
    if flagged.is_empty() {
        return Some(format!(
            "{}/{} real playback session{} stayed under the {:.0}% dropped-frame threshold",
            checkable.len(),
            checkable.len(),
            if checkable.len() == 1 { "" } else { "s" },
            DROP_RATE_THRESHOLD * 100.0,
        ));
    }
    let mut s = format!(
        "{}/{} real playback sessions exceeded the {:.0}% dropped-frame threshold:",
        flagged.len(),
        checkable.len(),
        DROP_RATE_THRESHOLD * 100.0,
    );
    for e in flagged {
        // Both are `Some` here: `drop_rate_flagged` only returns `Some(_)` when `dropped_frames` and
        // a positive `expected_fps` are both present, so re-deriving the same rate to display is
        // exactly what was just judged, not a second, possibly-inconsistent computation.
        let dropped = e.dropped_frames.unwrap_or(0);
        let expected_frames = e.expected_fps.unwrap_or(0.0) * e.seconds_played;
        let rate =
            if expected_frames > 0.0 { dropped as f64 / expected_frames * 100.0 } else { 0.0 };
        s.push_str(&format!(
            "\n  {} -- {dropped} dropped over {:.0}s ({rate:.1}%)",
            e.path.display(),
            e.seconds_played,
        ));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{TrackCounts, TrackInfo};

    /// `video_codec` here is FFmpeg's short name (`"hevc"`, `"vc1"`, ...) -- the same string a real
    /// selected video track carries in `TrackInfo::codec`, which is what `predicted_hardware_decode`
    /// actually reads. It is also stashed in `FileResult::video_codec` (mpv's own friendly
    /// `video-codec` property in real playback) purely so `observe`'s "was there a video track at
    /// all" gate and the display text in `summarize` have something to show.
    fn played_result(video_codec: Option<&str>, hwdec: Option<&str>) -> FileResult {
        FileResult {
            path: PathBuf::from("/lib/movie.mkv"),
            label: "movie".into(),
            outcome: Outcome::Played,
            seconds_played: 5.0,
            file_format: Some("matroska,webm".into()),
            video_codec: video_codec.map(String::from),
            audio_codec: None,
            width: None,
            height: None,
            fps: None,
            duration: None,
            hwdec: hwdec.map(String::from),
            pixel_format: None,
            primaries: None,
            gamma: None,
            colormatrix: None,
            seekable: None,
            audio_channels: None,
            track_counts: TrackCounts::default(),
            tracks: video_codec
                .map(|c| {
                    vec![TrackInfo {
                        kind: "video".into(),
                        selected: true,
                        codec: Some(c.to_string()),
                        ..Default::default()
                    }]
                })
                .unwrap_or_default(),
            fidelity: None,
            delayed_frames: None,
            dropped_frames: Some(3),
        }
    }

    #[test]
    fn a_file_that_never_played_produces_no_entry() {
        let mut r = played_result(Some("hevc"), Some("videotoolbox"));
        r.outcome = Outcome::Failed("unrecognized file format".into());
        assert!(observe(&r).is_none());
    }

    #[test]
    fn an_audio_only_file_produces_no_entry() {
        let r = played_result(None, None);
        assert!(observe(&r).is_none(), "nothing to calibrate without a video decode path");
    }

    #[test]
    fn a_codec_the_native_profile_claims_hardware_for_predicts_hardware() {
        let r = played_result(Some("hevc"), Some("videotoolbox"));
        let entry = observe(&r).unwrap();
        assert_eq!(entry.predicted_hardware_decode, Some(true));
        assert_eq!(entry.hardware_decode_as_predicted(), Some(true));
    }

    #[test]
    fn a_codec_the_native_profile_explicitly_claims_no_hardware_for_predicts_software() {
        // VC-1 is declared `hardware: false` in reference_native's own table.
        let r = played_result(Some("vc1"), Some("no"));
        let entry = observe(&r).unwrap();
        assert_eq!(entry.predicted_hardware_decode, Some(false));
        assert_eq!(
            entry.hardware_decode_as_predicted(),
            Some(true),
            "software was exactly what was predicted"
        );
    }

    #[test]
    fn a_hardware_decode_predicted_but_not_observed_is_a_real_miss() {
        let r = played_result(Some("hevc"), Some("no"));
        let entry = observe(&r).unwrap();
        assert_eq!(entry.hardware_decode_as_predicted(), Some(false));
    }

    #[test]
    fn software_observed_when_hwdec_was_never_even_queried_is_not_a_confirmed_match() {
        // `hwdec-current` missing entirely (None) is a different situation from mpv answering "no" --
        // treated as nothing to compare, not silently folded into either outcome.
        let r = played_result(Some("hevc"), None);
        let entry = observe(&r).unwrap();
        assert_eq!(entry.hardware_decode_as_predicted(), None);
    }

    #[test]
    fn an_unrecognised_codec_makes_no_claim_either_way() {
        let r = played_result(Some("some_future_codec_this_build_does_not_know"), Some("no"));
        let entry = observe(&r).unwrap();
        assert_eq!(entry.predicted_hardware_decode, None);
        assert_eq!(entry.hardware_decode_as_predicted(), None);
    }

    fn played_result_with_drops(fps: f64, seconds_played: f64, dropped_frames: u64) -> FileResult {
        let mut r = played_result(Some("hevc"), Some("videotoolbox"));
        r.fps = Some(fps);
        r.seconds_played = seconds_played;
        r.dropped_frames = Some(dropped_frames);
        r
    }

    #[test]
    fn a_drop_rate_under_the_threshold_is_not_flagged() {
        // 24 fps for 60s expects 1440 frames; 10 dropped is ~0.7%, under the 2% threshold.
        let entry = observe(&played_result_with_drops(24.0, 60.0, 10)).unwrap();
        assert_eq!(entry.drop_rate_flagged(), Some(false));
    }

    #[test]
    fn a_drop_rate_over_the_threshold_is_flagged() {
        // 24 fps for 60s expects 1440 frames; 100 dropped is ~7%, well over the 2% threshold.
        let entry = observe(&played_result_with_drops(24.0, 60.0, 100)).unwrap();
        assert_eq!(entry.drop_rate_flagged(), Some(true));
    }

    #[test]
    fn no_known_frame_rate_is_not_a_confirmed_judgement() {
        // VFR content, or a build too old to have reported fps -- nothing to divide by, so nothing
        // to judge, not a fabricated rate against an assumed frame rate.
        let mut r = played_result(Some("hevc"), Some("videotoolbox"));
        r.fps = None;
        r.dropped_frames = Some(500);
        let entry = observe(&r).unwrap();
        assert_eq!(entry.drop_rate_flagged(), None);
    }

    #[test]
    fn a_session_too_brief_to_mean_anything_is_not_judged() {
        let entry = observe(&played_result_with_drops(24.0, 0.5, 5)).unwrap();
        assert_eq!(
            entry.drop_rate_flagged(),
            None,
            "a fraction of a second played is not enough signal to judge, however many drops"
        );
    }

    #[test]
    fn no_dropped_frame_count_at_all_is_not_judged() {
        let mut r = played_result(Some("hevc"), Some("videotoolbox"));
        r.fps = Some(24.0);
        r.dropped_frames = None;
        let entry = observe(&r).unwrap();
        assert_eq!(entry.drop_rate_flagged(), None);
    }

    #[test]
    fn append_then_read_all_round_trips_every_field() {
        let dir = std::env::temp_dir().join(format!("lumen-calibration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calibration.jsonl");

        let a = observe(&played_result(Some("hevc"), Some("no"))).unwrap();
        let b = observe(&played_result(Some("vc1"), Some("no"))).unwrap();
        append(&path, &a).unwrap();
        append(&path, &b).unwrap();

        let loaded = read_all(&path).unwrap();
        assert_eq!(loaded.len(), 2, "append must never overwrite a prior entry");
        assert_eq!(loaded[0].video_codec.as_deref(), Some("hevc"));
        assert_eq!(loaded[0].predicted_hardware_decode, Some(true));
        assert_eq!(loaded[0].dropped_frames, Some(3));
        assert_eq!(loaded[0].seconds_played, 5.0);
        assert_eq!(loaded[1].video_codec.as_deref(), Some("vc1"));
        assert_eq!(loaded[1].predicted_hardware_decode, Some(false));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expected_fps_round_trips_and_a_missing_one_reads_back_as_null() {
        let dir =
            std::env::temp_dir().join(format!("lumen-calibration-fps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calibration.jsonl");

        let with_fps = observe(&played_result_with_drops(24.0, 60.0, 10)).unwrap();
        let without_fps = observe(&played_result(Some("hevc"), Some("no"))).unwrap();
        append(&path, &with_fps).unwrap();
        append(&path, &without_fps).unwrap();

        let loaded = read_all(&path).unwrap();
        assert_eq!(loaded[0].expected_fps, Some(24.0));
        assert_eq!(loaded[0].seconds_played, 60.0);
        assert_eq!(loaded[1].expected_fps, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v1_line_with_no_drop_rate_columns_still_loads_with_nothing_to_judge() {
        // Exactly what a build before this field existed would have written -- no seconds_played or
        // expected_fps keys at all.
        let line = r#"{"path":"/a.mkv","unix_secs":1000,"video_codec":"hevc","predicted_hardware_decode":true,"observed_hwdec":"no","dropped_frames":500}"#;
        let entry = CalibrationEntry::from_json_line(line).unwrap();
        assert_eq!(entry.seconds_played, 0.0);
        assert_eq!(entry.expected_fps, None);
        assert_eq!(
            entry.drop_rate_flagged(),
            None,
            "an old entry with no duration on record must not be judged as if it played 0 seconds \
             worth of a real session"
        );
    }

    #[test]
    fn summarize_reports_a_drop_rate_section_only_when_there_is_something_to_judge() {
        let no_data = summarize(&[]);
        assert!(
            !no_data.contains("dropped-frame threshold"),
            "an empty log has nothing to say about drop rate either: {no_data}"
        );

        let nothing_checkable =
            summarize(&[observe(&played_result(Some("hevc"), Some("no"))).unwrap()]);
        assert!(
            !nothing_checkable.contains("dropped-frame threshold"),
            "no fps ever reported means nothing to judge: {nothing_checkable}"
        );

        let mut flagged = observe(&played_result_with_drops(24.0, 60.0, 100)).unwrap();
        flagged.path = PathBuf::from("/lib/stuttery.mkv");
        let with_a_flag = summarize(&[flagged]);
        assert!(with_a_flag.contains("dropped-frame threshold"), "{with_a_flag}");
        assert!(with_a_flag.contains("stuttery.mkv"), "{with_a_flag}");
    }

    #[test]
    fn reading_a_missing_log_is_an_empty_list_not_an_error() {
        let path = std::env::temp_dir().join("lumen-calibration-does-not-exist-8817.jsonl");
        assert_eq!(read_all(&path).unwrap(), Vec::new());
    }

    #[test]
    fn one_corrupt_line_is_dropped_without_losing_the_rest_of_the_log() {
        let dir =
            std::env::temp_dir().join(format!("lumen-calibration-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calibration.jsonl");
        std::fs::write(
            &path,
            "not json at all\n{\"path\":\"/a.mkv\",\"unix_secs\":1000,\"video_codec\":\"hevc\",\"predicted_hardware_decode\":true,\"observed_hwdec\":\"no\",\"dropped_frames\":null}\n",
        )
        .unwrap();

        let loaded = read_all(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].video_codec.as_deref(), Some("hevc"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn summarize_reports_no_data_yet_for_an_empty_log() {
        let s = summarize(&[]);
        assert!(s.contains("no calibration data"), "{s}");
    }

    #[test]
    fn summarize_counts_matches_and_names_every_miss() {
        let matched = observe(&played_result(Some("hevc"), Some("videotoolbox"))).unwrap();
        let mut missed = observe(&played_result(Some("av1"), Some("no"))).unwrap();
        missed.path = PathBuf::from("/lib/broken-hw-decode.mkv");

        let s = summarize(&[matched, missed]);
        assert!(s.contains("1/2"), "{s}");
        assert!(s.contains("broken-hw-decode.mkv"), "{s}");
        assert!(s.contains("predicted hardware, mpv actually used software"), "{s}");
    }

    #[test]
    fn summarize_ignores_entries_with_nothing_to_compare() {
        let uncheckable = observe(&played_result(Some("unknown_codec"), Some("no"))).unwrap();
        let s = summarize(&[uncheckable]);
        assert!(s.contains("no calibration data"), "{s}");
        assert!(s.contains("1 session"), "{s}");
    }
}
