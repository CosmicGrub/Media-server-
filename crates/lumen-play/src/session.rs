//! Playback session: launch mpv on a playlist, follow it, and record what happened to each file.
//!
//! This is the part that turns a player into a test. Playing a library tells you nothing unless
//! something is watching; what makes the run evidence is the per-file record of whether the file
//! opened, which codecs came out, whether hardware decoding actually engaged, and — when it failed —
//! mpv's own reason for it.
//!
//! **One broken file must never end the run.** `--keep-open=no` plus `--idle=yes` means mpv advances
//! past a file it cannot open and waits at the end rather than exiting, so a thousand-file scan
//! finishes even if the first fifty are corrupt. That is the "no refusal" guarantee (`docs/11` §G2)
//! applied to the playlist as a whole.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::ipc::{self, Mpv};
use crate::json::Value;
use crate::scan::{Scan, ScannedFile};

#[derive(Debug, Clone, Default)]
pub struct PlayOptions {
    /// Play only this many seconds of each file, then advance. The library-test mode: it walks a
    /// whole collection in minutes and reports which files failed.
    pub seconds_each: Option<u64>,
    /// Start paused, so the operator can position the window before anything is measured.
    pub start_paused: bool,
    /// Video output to request. `None` uses the built-in preference order.
    pub vo: Option<String>,
    /// Hardware decoding mode passed to mpv.
    pub hwdec: String,
    pub fullscreen: bool,
    /// Play the files in a shuffled order.
    pub shuffle: bool,
    /// Extra arguments passed through verbatim.
    pub extra_args: Vec<String>,
    /// Build the playlist and print it, launching nothing.
    pub dry_run: bool,
    /// Ask mpv to bitstream HD audio (TrueHD, DTS-HD, E-AC-3, AC-3) via S/PDIF rather than decoding
    /// it, so `calibration`'s predicted-vs-observed comparison has something real to check. Off by
    /// default: unlike hardware decode, this changes what actually comes out of the audio device, and
    /// a sink that cannot accept a bitstream would get silence rather than the PCM fallback mpv
    /// otherwise provides.
    pub audio_passthrough: bool,
}

impl PlayOptions {
    pub fn new() -> Self {
        Self { hwdec: "auto-safe".into(), fullscreen: true, ..Default::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Opened and produced frames.
    Played,
    /// mpv reported an error for this file.
    Failed(String),
    /// The run ended before reaching it.
    NotReached,
}

impl Outcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// What playing one file revealed about it.
#[derive(Debug, Clone)]
pub struct FileResult {
    pub path: PathBuf,
    pub label: String,
    pub outcome: Outcome,
    pub seconds_played: f64,
    pub file_format: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub duration: Option<f64>,
    /// The decoder actually in use. `no` means software — worth knowing, since a library that plays
    /// only because the CPU is carrying it will not play on a weaker device.
    pub hwdec: Option<String>,
    /// Pixel format, e.g. `yuv420p10`. Ten-bit is the giveaway for an HDR master.
    pub pixel_format: Option<String>,
    /// Colour primaries, e.g. `bt.2020`.
    pub primaries: Option<String>,
    /// Transfer function. `pq` is HDR10/Dolby Vision, `hlg` is broadcast HDR, anything else is SDR.
    pub gamma: Option<String>,
    /// YUV-to-RGB matrix coefficients, e.g. `bt.2020-ncl`.
    pub colormatrix: Option<String>,
    /// Whether mpv can seek in this file. A long video that reports `false` has lost its index —
    /// Matroska Cues or an MP4 `moov` — which plays start-to-finish but cannot be navigated. It is
    /// the defect a play-through test would never notice, because playing forward still works.
    pub seekable: Option<bool>,
    pub audio_channels: Option<String>,
    pub track_counts: TrackCounts,
    /// Every track the demuxer exposed, not only the selected one. The per-file properties above
    /// describe what played; this describes what was *there*, which is what a fidelity decision
    /// needs — a file whose second audio track is TrueHD is a different proposition from one that
    /// only carries AAC, even when mpv chose the AAC either way.
    pub tracks: Vec<TrackInfo>,
    /// Fidelity tiers this file reaches, modelled from what the demuxer reported.
    pub fidelity: Option<crate::fidelity::Fidelity>,
    /// Frames the video output presented late, over this file.
    pub delayed_frames: Option<u64>,
    pub dropped_frames: Option<u64>,
    /// Whether this session asked mpv to bitstream HD audio (`PlayOptions::audio_passthrough`).
    /// Carried per-result, not just per-session, so `calibration::observe` knows whether
    /// [`Self::audio_out_format`]'s absence means "not bitstreamed" or "never asked" -- claiming a
    /// passthrough miss on a session that never requested passthrough would blame the model for a
    /// gap in what this codebase asks mpv to do, not for a wrong prediction.
    pub audio_spdif_requested: bool,
    /// mpv's `audio-out-params/format` after playback -- `"spdif-ac3"`/`"spdif-dts-hd"`/... when a
    /// bitstream reached the sink, a PCM sample format (`"s16"`, `"floatp"`, ...) when mpv decoded it
    /// instead. Only meaningful when [`Self::audio_spdif_requested`] is true.
    pub audio_out_format: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackCounts {
    pub video: usize,
    pub audio: usize,
    pub subtitle: usize,
}

/// One entry of mpv's `track-list`.
///
/// Deliberately stringly-typed at this layer: this is a transcription of what mpv said, and the
/// translation into the workspace's codec enums happens in `fidelity`, where it can be tested
/// against known names without a running mpv.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackInfo {
    /// `video`, `audio` or `sub`.
    pub kind: String,
    pub id: u32,
    /// FFmpeg's short codec name — `h264`, `dts`, `hdmv_pgs_subtitle`.
    pub codec: Option<String>,
    /// Codec profile where the build reports one. This is what separates DTS-HD MA from its DTS
    /// core, and Main 10 from Main — distinctions the tier depends on.
    pub codec_profile: Option<String>,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub external: bool,
    pub selected: bool,
    pub hearing_impaired: bool,
    pub visual_impaired: bool,
    pub channels: Option<u8>,
    pub sample_rate: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub bitrate_bps: Option<u64>,
}

impl FileResult {
    fn new(f: &ScannedFile, opts: &PlayOptions) -> Self {
        Self {
            path: f.path.clone(),
            label: f.label(),
            outcome: Outcome::NotReached,
            seconds_played: 0.0,
            file_format: None,
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            fps: None,
            duration: None,
            hwdec: None,
            pixel_format: None,
            primaries: None,
            gamma: None,
            colormatrix: None,
            seekable: None,
            audio_channels: None,
            track_counts: TrackCounts::default(),
            tracks: Vec::new(),
            fidelity: None,
            delayed_frames: None,
            dropped_frames: None,
            audio_spdif_requested: opts.audio_passthrough,
            audio_out_format: None,
        }
    }

    /// True when the file played but only because the CPU decoded it.
    pub fn software_decoded(&self) -> bool {
        self.outcome == Outcome::Played
            && self.hwdec.as_deref().is_some_and(|h| h == "no" || h.is_empty())
    }

    /// True when the transfer function says this is an HDR master.
    ///
    /// The transfer function decides it, not the primaries: BT.2020 primaries with a conventional
    /// gamma curve is a wide-gamut SDR file, and calling that HDR would misreport a real distinction
    /// this product has to get right.
    pub fn is_hdr(&self) -> bool {
        self.gamma.as_deref().is_some_and(|g| g == "pq" || g == "hlg")
    }

    /// Played, but cannot be seeked — the index is missing or unusable.
    ///
    /// Only meaningful for something long enough to want to navigate; a thirty-second clip that
    /// cannot seek is not a library problem worth reporting.
    pub fn unseekable(&self) -> bool {
        self.outcome == Outcome::Played
            && self.seekable == Some(false)
            && self.duration.is_some_and(|d| d > 60.0)
    }

    pub fn resolution(&self) -> Option<String> {
        Some(format!("{}x{}", self.width?, self.height?))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionReport {
    pub results: Vec<FileResult>,
    pub mpv_version: Option<String>,
    pub vo_used: Option<String>,
    pub elapsed_s: f64,
    /// mpv exited before the playlist finished.
    pub ended_early: bool,
}

impl SessionReport {
    pub fn played(&self) -> usize {
        self.results.iter().filter(|r| r.outcome == Outcome::Played).count()
    }
    pub fn failed(&self) -> impl Iterator<Item = &FileResult> {
        self.results.iter().filter(|r| r.outcome.is_failure())
    }
    pub fn not_reached(&self) -> usize {
        self.results.iter().filter(|r| r.outcome == Outcome::NotReached).count()
    }
    pub fn software_decoded(&self) -> impl Iterator<Item = &FileResult> {
        self.results.iter().filter(|r| r.software_decoded())
    }
    pub fn unseekable(&self) -> impl Iterator<Item = &FileResult> {
        self.results.iter().filter(|r| r.unseekable())
    }
}

/// The mpv arguments a session runs with.
///
/// Split out so the argument list is inspectable without launching anything — `--dry-run` prints
/// exactly this, which is the difference between "the player did something odd" and a diagnosis.
pub fn mpv_args(ipc_path: &str, opts: &PlayOptions) -> Vec<String> {
    let mut args = vec![
        format!("--input-ipc-server={ipc_path}"),
        // No `--playlist` here on purpose. mpv given a playlist on the command line starts playing
        // before this process can connect, and the first `start-file` event is gone before anyone is
        // listening — which silently shifts every result onto the wrong file. The playlist is sent
        // over IPC once the connection exists, so no event can be missed.
        format!("--hwdec={}", opts.hwdec),
        // gpu-next is the libplacebo renderer this product is built on.
        format!("--vo={}", opts.vo.as_deref().unwrap_or("gpu-next")),
        // A file that cannot be opened must not stop the run: advance past it, and idle at the end
        // rather than exiting, so the session can collect its results before quitting.
        "--keep-open=no".into(),
        "--idle=yes".into(),
        // Show a window even for a file with no video, so an audio-only or broken file is visibly
        // being attempted rather than appearing to hang.
        "--force-window=yes".into(),
        // Subtitles and external audio sitting beside the file are part of the library.
        "--sub-auto=fuzzy".into(),
        "--audio-file-auto=fuzzy".into(),
        // Never stop for a missing codec or a bad stream — try, and report.
        "--audio-fallback-to-null=yes".into(),
        // Quiet the terminal without going silent. mpv's status line repaints several times a
        // second and buries this tool's own per-file output — on a ten-file run it produced roughly
        // eight hundred lines of `V: 00:00:03 / 00:00:05 (61%)` around nine lines that mattered.
        // Error messages are kept, because they name the codec or container that failed.
        "--term-status-msg=".into(),
        "--msg-level=all=error".into(),
        // mpv must not read the terminal this process is sharing, or it competes for keystrokes.
        // The playback window still takes its own input.
        "--no-input-terminal".into(),
    ];
    if opts.fullscreen {
        args.push("--fullscreen=yes".into());
    }
    if opts.start_paused {
        args.push("--pause=yes".into());
    }
    if opts.audio_passthrough {
        // mpv's own accepted codec list for this option, per its manual: ac3, dts, dts-hd, eac3,
        // truehd. Without this flag mpv decodes every one of them to PCM regardless of what the
        // sink could actually take, which is exactly the "never asked" gap that made this
        // uncheckable before.
        args.push("--audio-spdif=ac3,dts,dts-hd,eac3,truehd".into());
    }
    args.extend(opts.extra_args.iter().cloned());
    args
}

/// Write the playlist mpv will read.
///
/// A file per line, in the order chosen. Passing thousands of paths as arguments would exceed the
/// command-line limit on every platform, and the file is also a record of what the run covered.
pub fn write_playlist(dir: &Path, files: &[&ScannedFile]) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("lumen-playlist-{}.txt", std::process::id()));
    let mut text = String::new();
    for f in files {
        // mpv reads this as UTF-8, one path per line. A path containing a newline cannot be
        // represented and is skipped rather than corrupting every following entry.
        let p = f.path.to_string_lossy();
        if p.contains('\n') || p.contains('\r') {
            continue;
        }
        text.push_str(&p);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

fn spawn_mpv(args: &[String]) -> std::io::Result<Child> {
    // Resolved rather than assumed, so a bundled mpv sitting next to this binary is used in
    // preference to whatever happens to be installed.
    let exe = crate::mpvbin::find().unwrap_or_else(|| PathBuf::from("mpv"));
    Command::new(exe)
        .args(args)
        .stdout(Stdio::null())
        // mpv's own errors go to the terminal, where they belong: when a file fails, its message is
        // the most useful thing on screen.
        .stderr(Stdio::inherit())
        .spawn()
}

/// Run a playback session over the scanned files.
pub fn run(
    scan: &Scan,
    order: &[usize],
    opts: &PlayOptions,
    mut progress: impl FnMut(&FileResult, usize, usize),
) -> Result<SessionReport, String> {
    let files: Vec<&ScannedFile> = order.iter().map(|&i| &scan.files[i]).collect();
    if files.is_empty() {
        return Err("nothing to play: the scan found no media files".into());
    }

    let tmp = std::env::temp_dir();
    let playlist = write_playlist(&tmp, &files).map_err(|e| format!("playlist: {e}"))?;
    let ipc_path = ipc::default_ipc_path("play");
    let _ = std::fs::remove_file(&ipc_path);
    let args = mpv_args(&ipc_path, opts);

    if opts.dry_run {
        println!("mpv \\\n  {}", args.join(" \\\n  "));
        println!("  (then over IPC: loadlist {} replace)", playlist.display());
        println!("\nplaylist ({} files): {}", files.len(), playlist.display());
        return Ok(SessionReport {
            results: files.iter().map(|f| FileResult::new(f, opts)).collect(),
            ..Default::default()
        });
    }

    let mut child = spawn_mpv(&args)
        .map_err(|e| format!("cannot launch mpv: {e}\n\n{}", crate::mpvbin::install_hint()))?;

    let mut mpv = match Mpv::connect(&ipc_path, Duration::from_secs(20)) {
        Ok(m) => m,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "mpv started but its IPC socket never appeared ({e}). The build may not support \
                 --input-ipc-server; check `mpv --version`."
            ));
        }
    };

    // Now that events cannot be missed, hand mpv the playlist.
    let list_arg = playlist.to_string_lossy().into_owned();
    if mpv.command(&["loadlist", &list_arg, "replace"]).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("mpv would not accept the playlist".into());
    }
    if opts.shuffle {
        let _ = mpv.command(&["playlist-shuffle"]);
    }

    // Set by LUMEN_DEBUG_EVENTS=1. Dumps every event mpv sends, which is the only way to tell a
    // protocol misunderstanding from a logic bug when the results look plausible but are wrong.
    let debug_events = std::env::var_os("LUMEN_DEBUG_EVENTS").is_some();

    let start = Instant::now();
    let mut report = SessionReport {
        results: files.iter().map(|f| FileResult::new(f, opts)).collect(),
        mpv_version: mpv.get_string("mpv-version"),
        vo_used: mpv.get_string("current-vo"),
        ..Default::default()
    };

    let total = files.len();
    // Path -> our index, for the one case where an entry id is unavailable.
    let by_path: std::collections::HashMap<String, usize> =
        files.iter().enumerate().map(|(i, f)| (f.path.to_string_lossy().into_owned(), i)).collect();

    // Entry id -> our index. This is the authoritative mapping: `playlist_entry_id` rides on the
    // `start-file` and `end-file` events themselves, so it cannot race the way a property read can.
    //
    // Reading the `path` property on `start-file` looks equivalent and is not — at that moment mpv
    // still reports the *previous* file, so every result lands one position early. That bug produced
    // a report where a corrupt 18-byte file was credited with 320x240 MPEG-4 video.
    let ids = mpv.get("playlist").map(|pl| entry_id_map(&pl, &by_path)).unwrap_or_default();
    if debug_events {
        eprintln!("[playlist] {} entries mapped by id", ids.len());
    }

    let mut current: Option<usize> = None;
    let mut reached = 0usize;
    let mut file_started = Instant::now();
    // Counter baselines, so per-file late-frame counts are deltas rather than the run's total.
    let mut base_delayed = 0u64;
    let mut base_dropped = 0u64;

    loop {
        if let Ok(Some(_)) = child.try_wait() {
            report.ended_early = reached < total;
            break;
        }

        // Advance when the per-file budget is spent. This is what makes a whole library testable in
        // minutes instead of days.
        if let (Some(limit), Some(_)) = (opts.seconds_each, current)
            && file_started.elapsed() >= Duration::from_secs(limit)
        {
            let _ = mpv.command(&["playlist-next", "force"]);
            file_started = Instant::now();
        }

        let Some(event) = mpv.next_event(Duration::from_millis(250)) else {
            // (no event this tick)
            if mpv.is_closed() {
                break;
            }
            continue;
        };

        if debug_events {
            eprintln!("[event] {event:?}");
        }

        match ipc::event_name(&event) {
            Some("start-file") => {
                current = event_index(&event, &ids)
                    .or_else(|| current.map(|c| (c + 1).min(total - 1)).or(Some(0)));
                reached += 1;
                file_started = Instant::now();
            }
            Some("file-loaded") => {
                // `file-loaded` carries no entry id, but by now the `path` property *is* the current
                // file, so it recovers the position if the start-file event was somehow missed.
                if current.is_none() {
                    current = mpv.get_string("path").and_then(|p| by_path.get(&p).copied());
                }
                if let Some(i) = current {
                    collect_properties(&mut mpv, &mut report.results[i]);
                    base_delayed =
                        mpv.get_f64("vo-delayed-frame-count").unwrap_or(0.0).max(0.0) as u64;
                    base_dropped = mpv.get_f64("frame-drop-count").unwrap_or(0.0).max(0.0) as u64;
                    // A file that loaded has, at minimum, opened. If it later errors the reason
                    // overwrites this; if the run is cut short, "played" is still the honest answer.
                    report.results[i].outcome = Outcome::Played;
                    // Assessed after the outcome is set, because a file that has not played is
                    // deliberately given no tier — and here, where the scanned file is in hand: the
                    // ladder wants the sniffed container and the file size, neither of which mpv
                    // reports.
                    report.results[i].fidelity =
                        crate::fidelity::assess(&report.results[i], files[i]);
                    progress(&report.results[i], i + 1, total);
                }
            }
            Some("end-file") => {
                // The entry id wins over `current`: a file that failed to open never emitted
                // `file-loaded`, and this is the only event that says which file it was.
                if let Some(i) = event_index(&event, &ids) {
                    current = Some(i);
                }
                if let Some(i) = current {
                    let r = &mut report.results[i];
                    r.seconds_played = file_started.elapsed().as_secs_f64();
                    let delayed =
                        mpv.get_f64("vo-delayed-frame-count").unwrap_or(0.0).max(0.0) as u64;
                    let dropped = mpv.get_f64("frame-drop-count").unwrap_or(0.0).max(0.0) as u64;
                    r.delayed_frames = Some(delayed.saturating_sub(base_delayed));
                    r.dropped_frames = Some(dropped.saturating_sub(base_dropped));

                    let reason = event.get("reason").and_then(Value::as_str).unwrap_or("unknown");
                    if ipc::end_reason_is_failure(reason) {
                        let detail = event
                            .get("file_error")
                            .and_then(Value::as_str)
                            .unwrap_or("mpv reported an error opening this file");
                        r.outcome = Outcome::Failed(detail.to_string());
                        progress(&report.results[i], i + 1, total);
                    }
                }
            }
            // mpv idles at the end of the playlist rather than exiting, which is what lets the last
            // file's results be collected. Quitting here is what actually ends the run.
            Some("idle") => {
                mpv.quit();
                break;
            }
            _ => {}
        }
    }

    report.elapsed_s = start.elapsed().as_secs_f64();
    mpv.quit();
    std::thread::sleep(Duration::from_millis(200));
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&ipc_path);
    let _ = std::fs::remove_file(&playlist);
    Ok(report)
}

fn collect_properties(mpv: &mut Mpv, r: &mut FileResult) {
    r.file_format = mpv.get_string("file-format");
    r.video_codec = mpv.get_string("video-codec");
    r.audio_codec = mpv.get_string("audio-codec");
    r.width = mpv.get_f64("width").map(|v| v as u32);
    r.height = mpv.get_f64("height").map(|v| v as u32);
    r.fps = mpv.get_f64("container-fps").or_else(|| mpv.get_f64("estimated-vf-fps"));
    r.duration = mpv.get_f64("duration");
    r.hwdec = mpv.get_string("hwdec-current");
    // mpv exposes nested parameters as flat `a/b` property names, so no tree walking is needed.
    r.pixel_format = mpv.get_string("video-params/pixelformat");
    r.primaries = mpv.get_string("video-params/primaries");
    r.gamma = mpv.get_string("video-params/gamma");
    r.colormatrix = mpv.get_string("video-params/colormatrix");
    // Only meaningful when this session actually requested passthrough (`--audio-spdif`); queried
    // unconditionally regardless, since a `None` here is exactly the honest "not requested" signal
    // `FileResult::audio_spdif_requested` needs a counterpart for.
    r.audio_out_format = mpv.get_string("audio-out-params/format");
    r.seekable = mpv.get("seekable").and_then(|v| v.as_bool());
    r.audio_channels = mpv.get_string("audio-params/channel-count");
    if let Some(list) = mpv.get("track-list") {
        r.track_counts = count_tracks(&list);
        r.tracks = parse_tracks(&list);
    }
}

/// Transcribe mpv's `track-list` into [`TrackInfo`].
///
/// Fields older mpv builds do not emit — `codec-profile` arrived in 0.38 — simply come back `None`
/// rather than failing the entry, because a missing profile string must not cost us the track.
pub fn parse_tracks(list: &Value) -> Vec<TrackInfo> {
    fn flag(t: &Value, key: &str) -> bool {
        t.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    fn text(t: &Value, key: &str) -> Option<String> {
        t.get(key).and_then(Value::as_str).map(str::to_owned)
    }
    fn num(t: &Value, key: &str) -> Option<f64> {
        t.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && *v >= 0.0)
    }

    list.as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|t| {
            let kind = text(t, "type")?;
            Some(TrackInfo {
                kind,
                id: num(t, "id").unwrap_or(0.0) as u32,
                codec: text(t, "codec"),
                codec_profile: text(t, "codec-profile"),
                lang: text(t, "lang"),
                title: text(t, "title"),
                default: flag(t, "default"),
                forced: flag(t, "forced"),
                external: flag(t, "external"),
                selected: flag(t, "selected"),
                hearing_impaired: flag(t, "hearing-impaired"),
                visual_impaired: flag(t, "visual-impaired"),
                channels: num(t, "demux-channel-count").map(|v| v.min(255.0) as u8),
                sample_rate: num(t, "demux-samplerate").map(|v| v as u32),
                width: num(t, "demux-w").map(|v| v as u32),
                height: num(t, "demux-h").map(|v| v as u32),
                fps: num(t, "demux-fps"),
                bitrate_bps: num(t, "demux-bitrate").map(|v| v as u64),
            })
        })
        .collect()
}

/// Map mpv's playlist entry ids onto our own indices.
///
/// Built from the `playlist` property rather than assumed to be `id - 1`: mpv only documents entry
/// ids as unique and stable, not as consecutive from one, and `playlist-shuffle` reorders entries
/// while keeping their ids. Matching on filename is what makes both facts irrelevant.
pub fn entry_id_map(
    playlist: &Value,
    by_path: &std::collections::HashMap<String, usize>,
) -> std::collections::HashMap<u64, usize> {
    let mut out = std::collections::HashMap::new();
    for (pos, entry) in playlist.as_array().unwrap_or(&[]).iter().enumerate() {
        let Some(id) = entry.get("id").and_then(Value::as_f64) else { continue };
        let index = entry
            .get("filename")
            .and_then(Value::as_str)
            .and_then(|f| by_path.get(f).copied())
            // Positional fallback for a build that reports a filename we cannot match — better than
            // dropping the entry, which would lose that file's result entirely.
            .or_else(|| (pos < by_path.len()).then_some(pos));
        if let Some(i) = index {
            out.insert(id as u64, i);
        }
    }
    out
}

/// Our index for the file an event refers to, via its `playlist_entry_id`.
fn event_index(event: &Value, ids: &std::collections::HashMap<u64, usize>) -> Option<usize> {
    let id = event.get("playlist_entry_id")?.as_f64()?;
    ids.get(&(id as u64)).copied()
}

/// Count video, audio and subtitle tracks in mpv's `track-list`.
pub fn count_tracks(list: &Value) -> TrackCounts {
    let mut c = TrackCounts::default();
    for t in list.as_array().unwrap_or(&[]) {
        match t.get("type").and_then(Value::as_str) {
            Some("video") => c.video += 1,
            Some("audio") => c.audio += 1,
            Some("sub") => c.subtitle += 1,
            _ => {}
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    #[test]
    fn the_arguments_keep_a_broken_file_from_ending_the_run() {
        // The single most important property of a library test: fifty corrupt files at the front of
        // a thousand-file scan must not stop it at file one.
        let args = mpv_args("/tmp/s.sock", &PlayOptions::new());
        let joined = args.join(" ");
        assert!(joined.contains("--keep-open=no"), "must advance past a file it cannot open");
        assert!(joined.contains("--idle=yes"), "must not exit before results are collected");
        assert!(joined.contains("--force-window=yes"), "a failing file must be visibly attempted");
    }

    #[test]
    fn the_status_line_is_suppressed_without_silencing_errors() {
        // Found by running this against real media on Linux: mpv's status line repaints several
        // times a second, and a ten-file run buried nine lines of report under ~800 lines of
        // `V: 00:00:03 / 00:00:05 (61%)`. Errors must survive, because they name what failed.
        let args = mpv_args("/tmp/s.sock", &PlayOptions::new());
        let joined = args.join(" ");
        assert!(joined.contains("--term-status-msg="), "the status line must be off");
        assert!(joined.contains("--msg-level=all=error"), "errors must still be shown");
        assert!(
            !joined.contains("--no-terminal"),
            "--no-terminal would suppress the error text too, which is the useful part"
        );
        assert!(joined.contains("--no-input-terminal"), "mpv must not steal our keystrokes");
    }

    #[test]
    fn the_arguments_pick_up_sidecar_subtitles_and_audio() {
        let args = mpv_args("/tmp/s.sock", &PlayOptions::new());
        let joined = args.join(" ");
        assert!(joined.contains("--sub-auto=fuzzy"));
        assert!(joined.contains("--audio-file-auto=fuzzy"));
    }

    #[test]
    fn the_video_output_is_gpu_next_unless_overridden() {
        let default = mpv_args("/s", &PlayOptions::new());
        assert!(default.iter().any(|a| a == "--vo=gpu-next"));
        let overridden =
            mpv_args("/s", &PlayOptions { vo: Some("gpu".into()), ..PlayOptions::new() });
        assert!(overridden.iter().any(|a| a == "--vo=gpu"));
        assert!(!overridden.iter().any(|a| a == "--vo=gpu-next"));
    }

    #[test]
    fn extra_arguments_come_last_so_they_win() {
        // mpv takes the last occurrence of an option, so a pass-through override has to be able to
        // beat our defaults — otherwise `--vo=x11` on the command line would silently do nothing.
        let opts = PlayOptions { extra_args: vec!["--vo=x11".into()], ..PlayOptions::new() };
        let args = mpv_args("/s", &opts);
        let vo_positions: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| a.starts_with("--vo="))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(args[*vo_positions.last().unwrap()], "--vo=x11");
    }

    #[test]
    fn the_playlist_is_not_passed_on_the_command_line() {
        // mpv given a playlist as an argument starts playing before this process can connect, and
        // the first `start-file` event is gone before anyone is listening — which shifts every
        // result onto the wrong file. Observed for real: a corrupt 18-byte file was credited with
        // 320x240 MPEG-4 video that belonged to the next file in the list.
        let args = mpv_args("/tmp/s.sock", &PlayOptions::new());
        assert!(
            !args.iter().any(|a| a.starts_with("--playlist")),
            "the playlist must be sent over IPC after connecting: {args:?}"
        );
        assert!(args.iter().any(|a| a == "--idle=yes"), "mpv must wait for it");
    }

    #[test]
    fn entry_ids_map_to_our_indices_by_filename() {
        let by_path: std::collections::HashMap<String, usize> =
            [("/m/a.mkv".to_string(), 0), ("/m/b.mkv".to_string(), 1)].into_iter().collect();
        // Shuffled: mpv reordered the entries but kept their ids. Matching on filename is what makes
        // the order irrelevant — assuming `id - 1` would swap these two files' results.
        let pl =
            parse(r#"[{"id":2,"filename":"/m/b.mkv"},{"id":1,"filename":"/m/a.mkv"}]"#).unwrap();
        let ids = entry_id_map(&pl, &by_path);
        assert_eq!(ids.get(&1), Some(&0));
        assert_eq!(ids.get(&2), Some(&1));
    }

    #[test]
    fn entry_ids_are_not_assumed_to_start_at_one() {
        // mpv documents entry ids as unique and stable, not as consecutive from one. A playlist
        // edited during the run can leave gaps.
        let by_path: std::collections::HashMap<String, usize> =
            [("/m/a.mkv".to_string(), 0)].into_iter().collect();
        let pl = parse(r#"[{"id":97,"filename":"/m/a.mkv"}]"#).unwrap();
        assert_eq!(entry_id_map(&pl, &by_path).get(&97), Some(&0));
    }

    #[test]
    fn an_unmatched_filename_falls_back_to_position_rather_than_being_dropped() {
        // A build that normalises the path it reports would otherwise lose that file's result
        // entirely — a silent hole in the report, which is worse than an approximate answer.
        let by_path: std::collections::HashMap<String, usize> =
            [("/m/a.mkv".to_string(), 0), ("/m/b.mkv".to_string(), 1)].into_iter().collect();
        // A build that normalises the case of what it reports back, which Windows genuinely does.
        let pl =
            parse(r#"[{"id":1,"filename":"/M/A.MKV"},{"id":2,"filename":"/m/b.mkv"}]"#).unwrap();
        let ids = entry_id_map(&pl, &by_path);
        assert_eq!(ids.get(&1), Some(&0), "positional fallback");
        assert_eq!(ids.get(&2), Some(&1), "exact match still wins");
    }

    #[test]
    fn an_event_resolves_to_the_file_its_entry_id_names() {
        let ids: std::collections::HashMap<u64, usize> = [(1, 0), (2, 1)].into_iter().collect();
        let ev = parse(r#"{"event":"end-file","playlist_entry_id":2,"reason":"error"}"#).unwrap();
        assert_eq!(event_index(&ev, &ids), Some(1));
        // An event without an id must not resolve to a guess; the caller has a fallback for that.
        assert_eq!(event_index(&parse(r#"{"event":"file-loaded"}"#).unwrap(), &ids), None);
        // An unknown id is not a silent zero.
        let unknown = parse(r#"{"event":"end-file","playlist_entry_id":99}"#).unwrap();
        assert_eq!(event_index(&unknown, &ids), None);
    }

    #[test]
    fn track_lists_are_counted_by_type() {
        let list = parse(
            r#"[{"type":"video"},{"type":"audio"},{"type":"audio"},
                {"type":"sub"},{"type":"sub"},{"type":"sub"}]"#,
        )
        .unwrap();
        assert_eq!(count_tracks(&list), TrackCounts { video: 1, audio: 2, subtitle: 3 });
        assert_eq!(count_tracks(&parse("[]").unwrap()), TrackCounts::default());
    }

    #[test]
    fn software_decoding_is_only_flagged_for_files_that_actually_played() {
        // A failed file has no decoder at all, and reporting it as "software decoded" would put it
        // in two contradictory buckets of the same report.
        let base = FileResult {
            path: PathBuf::from("/a.mkv"),
            label: "a".into(),
            outcome: Outcome::Played,
            hwdec: Some("no".into()),
            ..FileResult::new(&dummy_file(), &PlayOptions::default())
        };
        assert!(base.software_decoded());

        let hw = FileResult { hwdec: Some("vaapi".into()), ..base.clone() };
        assert!(!hw.software_decoded());

        let failed = FileResult { outcome: Outcome::Failed("x".into()), ..base.clone() };
        assert!(!failed.software_decoded());
    }

    fn blank() -> FileResult {
        FileResult::new(&dummy_file(), &PlayOptions::default())
    }

    fn dummy_file() -> ScannedFile {
        ScannedFile {
            path: PathBuf::from("/a.mkv"),
            size: 1,
            extension: Some("mkv".into()),
            kind: crate::scan::MediaKind::Video,
            container: None,
            confidence: None,
            evidence: None,
            extension_mismatch: false,
            unidentified: false,
            identity: None,
            parsed: lumen_match::parse("a.mkv"),
        }
    }

    #[test]
    fn a_playlist_is_written_one_path_per_line_in_order() {
        let dir = std::env::temp_dir().join(format!("lumen-pl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = ScannedFile { path: PathBuf::from("/m/b.mkv"), ..dummy_file() };
        let b = ScannedFile { path: PathBuf::from("/m/a.mkv"), ..dummy_file() };
        let p = write_playlist(&dir, &[&a, &b]).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec!["/m/b.mkv", "/m/a.mkv"],
            "order is preserved"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_containing_a_newline_is_skipped_rather_than_corrupting_the_playlist() {
        // One path per line cannot represent an embedded newline, and writing it anyway would make
        // every following entry a nonexistent file.
        let dir = std::env::temp_dir().join(format!("lumen-pl-nl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = ScannedFile { path: PathBuf::from("/m/we\nird.mkv"), ..dummy_file() };
        let good = ScannedFile { path: PathBuf::from("/m/fine.mkv"), ..dummy_file() };
        let p = write_playlist(&dir, &[&bad, &good]).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), vec!["/m/fine.mkv"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_scan_is_an_error_naming_the_cause() {
        let err = run(&Scan::default(), &[], &PlayOptions::new(), |_, _, _| {}).unwrap_err();
        assert!(err.contains("no media files"), "{err}");
    }

    #[test]
    fn hdr_is_decided_by_the_transfer_function_not_the_primaries() {
        // BT.2020 primaries with a conventional gamma curve is wide-gamut SDR. Calling that HDR
        // would misreport a distinction this product exists to get right.
        let base = FileResult { outcome: Outcome::Played, ..blank() };
        assert!(FileResult { gamma: Some("pq".into()), ..base.clone() }.is_hdr());
        assert!(FileResult { gamma: Some("hlg".into()), ..base.clone() }.is_hdr());
        assert!(
            !FileResult {
                gamma: Some("bt.1886".into()),
                primaries: Some("bt.2020".into()),
                ..base.clone()
            }
            .is_hdr(),
            "wide-gamut SDR is not HDR"
        );
        assert!(!FileResult { gamma: None, ..base }.is_hdr(), "unknown is not HDR");
    }

    #[test]
    fn an_unseekable_file_is_only_flagged_when_it_is_long_enough_to_matter() {
        // Plays forward but cannot be navigated — a lost Matroska Cues element. A short clip that
        // cannot seek is not a library defect worth a line in the report.
        let long = FileResult {
            outcome: Outcome::Played,
            seekable: Some(false),
            duration: Some(7200.0),
            ..blank()
        };
        assert!(long.unseekable());
        assert!(!FileResult { duration: Some(20.0), ..long.clone() }.unseekable());
        assert!(!FileResult { seekable: Some(true), ..long.clone() }.unseekable());
        // A file that never opened has no index to complain about.
        assert!(!FileResult { outcome: Outcome::Failed("x".into()), ..long }.unseekable());
    }

    #[test]
    fn outcomes_distinguish_failure_from_never_getting_there() {
        // A run cut short leaves files unattempted, and counting those as failures would report a
        // library as broken when it was simply not finished.
        assert!(Outcome::Failed("x".into()).is_failure());
        assert!(!Outcome::NotReached.is_failure());
        assert!(!Outcome::Played.is_failure());
    }
}
