//! Building and running the `ffmpeg` invocation that actually cuts a source into HLS segments.
//!
//! **Segmenting only, never a re-encode** -- the same remux-only boundary `lumen-exec` draws for
//! whole-file remuxing, drawn again here: every job stream-copies (`-c copy`), so it inherits that
//! crate's own limitation of covering only sources whose codecs are already legal for the chosen
//! segment container. `docs/13` §1's HLS-TS/HLS-CMAF columns are *not* cross-checked against the
//! source here -- doing that honestly needs the same codec/container legality matrix `lumen-model`
//! already carries for remuxing, wired in as real, separate future work rather than guessed at.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Which HLS segment container to cut. `docs/13` §1 legality differs by target: MPEG-TS carries
/// H.264/AAC/AC-3 family codecs; CMAF/fMP4 is required for HEVC, AV1, and Opus (all ❌ or 🟡 under
/// HLS-TS in that matrix) and is the modern default for anything besides maximum legacy compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentFormat {
    MpegTs,
    Fmp4,
}

impl SegmentFormat {
    fn segment_extension(self) -> &'static str {
        match self {
            Self::MpegTs => "ts",
            Self::Fmp4 => "m4s",
        }
    }
}

/// One segmenting job: cut `source` into `format`-shaped segments of roughly `segment_seconds` each
/// (see [`crate::plan`] on why "roughly"), writing them and a playlist into `output_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsSegmentJob {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    pub playlist_name: String,
    pub segment_seconds: u32,
    pub format: SegmentFormat,
}

impl HlsSegmentJob {
    pub fn playlist_path(&self) -> PathBuf {
        self.output_dir.join(&self.playlist_name)
    }

    fn segment_filename_pattern(&self) -> PathBuf {
        self.output_dir.join(format!("seg_%05d.{}", self.format.segment_extension()))
    }
}

/// The value ffmpeg's `-hls_fmp4_init_filename` is given. Deliberately a bare file name, never
/// `output_dir` joined onto it -- confirmed against a real ffmpeg build (6.1.1), not just read from
/// its docs: unlike `-hls_segment_filename` and the main playlist path, both of which the HLS muxer
/// honors as given (including a full absolute path), an *absolute* `-hls_fmp4_init_filename` is
/// naively concatenated onto the output's own directory rather than replacing it, producing a
/// doubled, nonexistent path (`<output_dir>/<output_dir>/init.mp4`) and a hard "Could not write
/// header" failure -- silent to every test in this crate that only ever exercised a fake `ffmpeg`
/// shell script, which does not reproduce a real muxer's own path-resolution quirks. A bare name
/// lands exactly where this crate wants it anyway, since ffmpeg resolves it relative to
/// `playlist_path()`'s own directory -- which *is* `output_dir`.
const INIT_SEGMENT_FILENAME: &str = "init.mp4";

/// Builds the full `ffmpeg` argument list for `job` -- pure and side-effect-free, exactly like
/// `lumen_exec::build_command`, so every case is testable without a real `ffmpeg` binary anywhere.
pub fn build_command(job: &HlsSegmentJob) -> Vec<String> {
    let mut args = vec![
        "-y".to_string(),
        "-i".to_string(),
        job.source.to_string_lossy().into_owned(),
        "-map".to_string(),
        "0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-f".to_string(),
        "hls".to_string(),
        "-hls_time".to_string(),
        job.segment_seconds.to_string(),
        "-hls_playlist_type".to_string(),
        "vod".to_string(),
    ];

    if job.format == SegmentFormat::Fmp4 {
        args.extend([
            "-hls_segment_type".to_string(),
            "fmp4".to_string(),
            "-hls_fmp4_init_filename".to_string(),
            INIT_SEGMENT_FILENAME.to_string(),
        ]);
    }

    args.extend([
        "-hls_segment_filename".to_string(),
        job.segment_filename_pattern().to_string_lossy().into_owned(),
    ]);
    args.push(job.playlist_path().to_string_lossy().into_owned());
    args
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsExecOutcome {
    pub playlist_path: PathBuf,
    pub segment_count: usize,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum HlsExecError {
    Spawn(std::io::Error),
    NonZeroExit {
        status: std::process::ExitStatus,
        stderr: String,
    },
    /// `ffmpeg` exited successfully but never wrote the playlist file it was asked for.
    PlaylistMissing,
    /// `ffmpeg` exited successfully and wrote a playlist, but no segment files with the expected
    /// extension are sitting next to it -- an empty or absurdly short source, most likely.
    NoSegmentsProduced,
}

impl std::fmt::Display for HlsExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start ffmpeg: {e}"),
            Self::NonZeroExit { status, stderr } => {
                write!(f, "ffmpeg exited with {status}: {}", stderr.trim())
            }
            Self::PlaylistMissing => write!(f, "ffmpeg did not write the expected playlist file"),
            Self::NoSegmentsProduced => write!(f, "no segment files were produced"),
        }
    }
}

impl std::error::Error for HlsExecError {}

/// Runs `job` with the `ffmpeg` binary at `ffmpeg_bin` and confirms real output landed on disk: the
/// playlist file exists, and at least one segment with the right extension sits beside it. This is a
/// presence check, not [`lumen_exec`]'s stronger content-sniffing verification -- an HLS playlist is
/// plain text `lumen_probe::sniff` has no signature for, and checking every individual segment's own
/// bytes is real, separate future work.
pub fn execute(job: &HlsSegmentJob, ffmpeg_bin: &Path) -> Result<HlsExecOutcome, HlsExecError> {
    let args = build_command(job);

    let start = Instant::now();
    let output =
        std::process::Command::new(ffmpeg_bin).args(&args).output().map_err(HlsExecError::Spawn)?;
    let elapsed = start.elapsed();

    if !output.status.success() {
        return Err(HlsExecError::NonZeroExit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let playlist_path = job.playlist_path();
    if !playlist_path.is_file() {
        return Err(HlsExecError::PlaylistMissing);
    }

    let ext = job.format.segment_extension();
    let segment_count = std::fs::read_dir(&job.output_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some(ext))
                .count()
        })
        .unwrap_or(0);
    if segment_count == 0 {
        return Err(HlsExecError::NoSegmentsProduced);
    }

    Ok(HlsExecOutcome { playlist_path, segment_count, elapsed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(format: SegmentFormat) -> HlsSegmentJob {
        HlsSegmentJob {
            source: "in.mkv".into(),
            output_dir: "out".into(),
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 6,
            format,
        }
    }

    #[test]
    fn mpeg_ts_segments_need_no_init_filename() {
        let args = build_command(&job(SegmentFormat::MpegTs));
        assert!(!args.iter().any(|a| a.contains("fmp4")));
        assert!(args.windows(2).any(|w| w == ["-hls_time", "6"]));
        assert!(args.iter().any(|a| a.ends_with("seg_%05d.ts")));
        assert!(args.last().unwrap().ends_with("stream.m3u8"));
    }

    #[test]
    fn fmp4_segments_declare_an_init_filename_and_the_fmp4_segment_type() {
        let args = build_command(&job(SegmentFormat::Fmp4));
        assert!(args.windows(2).any(|w| w == ["-hls_segment_type", "fmp4"]));
        // Bare, not `output_dir` joined onto it -- see `INIT_SEGMENT_FILENAME`'s own doc comment for
        // why: a real ffmpeg build resolves an absolute value here relative to its own output
        // directory regardless, producing a doubled path and a hard failure.
        assert!(
            args.windows(2).any(|w| w == ["-hls_fmp4_init_filename", "init.mp4"]),
            "the init filename must be exactly the bare literal \"init.mp4\": {args:?}"
        );
        assert!(args.iter().any(|a| a.ends_with("seg_%05d.m4s")));
    }

    #[test]
    fn the_source_is_stream_copied_never_re_encoded() {
        let args = build_command(&job(SegmentFormat::MpegTs));
        assert!(args.windows(2).any(|w| w == ["-c", "copy"]));
    }

    #[test]
    fn a_missing_ffmpeg_binary_is_a_spawn_error() {
        let err = execute(&job(SegmentFormat::MpegTs), Path::new("/definitely/not/real/ffmpeg"))
            .unwrap_err();
        assert!(matches!(err, HlsExecError::Spawn(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_succeeds_reports_every_segment_it_wrote() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        // Ignores its real arguments and writes a playlist plus three fake segments -- enough to
        // prove `execute` really spawns, waits, and then counts what actually landed on disk.
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             : > \"$dir/seg_00000.ts\"\n\
             : > \"$dir/seg_00001.ts\"\n\
             : > \"$dir/seg_00002.ts\"\n\
             echo '#EXTM3U' > \"$last\"\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = HlsSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 6,
            format: SegmentFormat::MpegTs,
        };
        let outcome = execute(&job, &fake_ffmpeg).expect("the fake ffmpeg must succeed and verify");
        assert_eq!(outcome.segment_count, 3);
        assert!(outcome.playlist_path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_writes_no_segments_is_reported_as_such() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\necho '#EXTM3U' > \"$last\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = HlsSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 6,
            format: SegmentFormat::MpegTs,
        };
        let err = execute(&job, &fake_ffmpeg).unwrap_err();
        assert!(matches!(err, HlsExecError::NoSegmentsProduced));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every other test above proves `build_command`'s output parses and `execute` spawns/verifies
    /// correctly -- against a FAKE ffmpeg that ignores its real arguments. This is the one test that
    /// exercises a genuine ffmpeg build, and it is what actually caught the bug
    /// [`INIT_SEGMENT_FILENAME`]'s own doc comment describes: a real HLS muxer's own path-resolution
    /// quirk (`-hls_fmp4_init_filename` given as an absolute path is naively concatenated onto
    /// `output_dir`, not honored as-is, unlike `-hls_segment_filename` and the playlist path) that
    /// every fake-ffmpeg test in this file was structurally unable to notice. Skipped, not failed,
    /// when `ffmpeg`/`ffprobe` are not on `PATH` -- the same "this is infrastructure, not a defect"
    /// convention `lumen-play`'s own mpv-dependent tests already use.
    #[cfg(unix)]
    #[test]
    fn a_real_ffmpeg_build_produces_a_valid_playable_fmp4_init_and_segment() {
        if !std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("skipping: ffmpeg is not on PATH in this environment");
            return;
        }
        if !std::process::Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("skipping: ffprobe is not on PATH in this environment");
            return;
        }

        let dir =
            std::env::temp_dir().join(format!("lumen-segment-real-ffmpeg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // A short, real, stream-copyable source -- encoded with ffmpeg itself (via lavfi test
        // sources) so this test needs no other tool on `PATH`.
        let source = dir.join("source.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
                source.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable to encode the source");
        assert!(status.success(), "encoding the real source file failed");

        let job = HlsSegmentJob {
            source,
            output_dir: out_dir.clone(),
            playlist_name: "playlist.m3u8".into(),
            segment_seconds: 6,
            format: SegmentFormat::Fmp4,
        };
        let outcome = execute(&job, Path::new("ffmpeg"))
            .expect("a real ffmpeg build must accept exactly what build_command produces");
        assert!(outcome.segment_count >= 1);

        let init = out_dir.join(INIT_SEGMENT_FILENAME);
        let seg = out_dir.join("seg_00000.m4s");
        assert!(init.is_file(), "a bare -hls_fmp4_init_filename must land inside output_dir");
        assert!(seg.is_file());

        // The real proof: concatenate the real init segment with the real first media segment --
        // exactly how a CMAF/fMP4 player consumes them -- and confirm `ffprobe` (a second,
        // independent real tool) reads back a genuinely valid, playable file carrying the source's
        // own stream-copied codecs, not just "some bytes landed on disk".
        let combined = dir.join("combined.mp4");
        let mut bytes = std::fs::read(&init).unwrap();
        bytes.extend(std::fs::read(&seg).unwrap());
        std::fs::write(&combined, &bytes).unwrap();

        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name",
                "-of",
                "csv=p=0",
                combined.to_str().unwrap(),
            ])
            .output()
            .expect("ffprobe must be runnable");
        assert!(
            probe.status.success(),
            "ffprobe rejected the reassembled init+segment: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let out = String::from_utf8_lossy(&probe.stdout);
        // Column order in ffprobe's csv output is not being relied on here -- just that some line
        // names both the right codec_type and the right codec_name together.
        assert!(
            out.lines().any(|l| l.contains("h264") && l.contains("video")),
            "expected a stream-copied h264 video stream:\n{out}"
        );
        assert!(
            out.lines().any(|l| l.contains("aac") && l.contains("audio")),
            "expected a stream-copied aac audio stream:\n{out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
