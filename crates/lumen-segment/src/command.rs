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
/// playlist file exists, and every segment it declares via `#EXTINF` is a real, non-empty file sitting
/// beside it. **Deliberately not a bare `output_dir` extension scan** -- counting every `.ts`/`.m4s`
/// file in the directory, regardless of whether the playlist this run just wrote actually names it,
/// cannot tell a segment this job produced from a same-extension one a prior, unrelated job into the
/// same `output_dir` left behind (ffmpeg's HLS muxer, like its DASH muxer, never cleans a reused
/// output directory, only overwrites the exact filenames it rewrites) -- a leftover stale segment
/// would silently inflate [`HlsExecOutcome::segment_count`] past what this run actually wrote.
/// `segment_count` is therefore always exactly the playlist's own `#EXTINF` count, never a directory's
/// raw file count. This is still a presence check, not [`lumen_exec`]'s stronger content-sniffing
/// verification -- an HLS playlist is plain text `lumen_probe::sniff` has no signature for, and
/// checking every individual segment's own bytes beyond "exists and is non-empty" is real, separate
/// future work.
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
    let playlist_text =
        std::fs::read_to_string(&playlist_path).map_err(|_| HlsExecError::PlaylistMissing)?;

    let segment_names = parse_extinf_segment_names(&playlist_text);
    if segment_names.is_empty() {
        return Err(HlsExecError::NoSegmentsProduced);
    }
    for name in &segment_names {
        match std::fs::metadata(job.output_dir.join(name)) {
            Ok(m) if m.len() > 0 => {}
            // The playlist names a segment that isn't genuinely sitting on disk -- this crate never
            // serves what it hasn't confirmed is real, the same posture as every other missing-file
            // case in this module.
            _ => return Err(HlsExecError::NoSegmentsProduced),
        }
    }

    Ok(HlsExecOutcome { playlist_path, segment_count: segment_names.len(), elapsed })
}

/// Hand-rolled `#EXTINF` scanner returning each declared segment's own file name -- mirroring
/// `lumen-play`'s own `parse_extinf_pairs` (`remote/server/hls.rs`), kept here so this crate's own
/// verification doesn't have to trust a bare `output_dir` extension scan that cannot tell a segment
/// *this* run produced from one a differently-shaped, unrelated prior job left behind under the same
/// name pattern (see [`execute`]'s own doc comment). Bounded and total like every other hand-rolled
/// parser in this workspace: a malformed or truncated `#EXTINF` entry is skipped, never panics. Each
/// URI is reduced to its own file name (`Path::file_name`) before being joined onto `output_dir`,
/// exactly as defensively as `lumen-play`'s own copy of this logic already does.
fn parse_extinf_segment_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if !line.starts_with("#EXTINF:") {
            continue;
        }
        let Some(uri_line) = lines.next() else { break };
        let uri = uri_line.trim();
        if uri.is_empty() || uri.starts_with('#') {
            continue;
        }
        let Some(name) = Path::new(uri).file_name().and_then(|n| n.to_str()) else { continue };
        out.push(name.to_string());
    }
    out
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
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        // Ignores its real arguments and writes a playlist (with real `#EXTINF` entries, matching a
        // genuine ffmpeg playlist's own shape) plus three fake segments -- enough to prove `execute`
        // really spawns, waits, and then counts what the playlist itself declares.
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/seg_00000.ts\"\n\
             printf x > \"$dir/seg_00001.ts\"\n\
             printf x > \"$dir/seg_00002.ts\"\n\
             printf '#EXTM3U\\n#EXTINF:6.0,\\nseg_00000.ts\\n#EXTINF:6.0,\\nseg_00001.ts\\n' > \"$last\"\n\
             printf '#EXTINF:6.0,\\nseg_00002.ts\\n#EXT-X-ENDLIST\\n' >> \"$last\"\n\
             exit 0\n",
        );

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
    fn a_stale_leftover_segment_from_an_unrelated_prior_job_is_never_counted() {
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-hls-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // Seeds `output_dir` with leftover `.ts` segments from an earlier, unrelated job -- exactly
        // the shape the finding's own repro describes: ffmpeg's HLS muxer never cleans a reused
        // output directory, only overwrites the exact filenames it rewrites.
        std::fs::write(out_dir.join("seg_00000.ts"), b"stale-bytes-from-a-prior-job").unwrap();
        std::fs::write(out_dir.join("seg_00001.ts"), b"stale-bytes-from-a-prior-job").unwrap();
        std::fs::write(out_dir.join("seg_00002.ts"), b"stale-bytes-from-a-prior-job").unwrap();

        let fake_ffmpeg = dir.join("ffmpeg");
        // This job's own real output: a single-segment playlist that only overwrites `seg_00000.ts`,
        // never touching (let alone removing) the two stale leftovers at indices 1 and 2.
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/seg_00000.ts\"\n\
             printf '#EXTM3U\\n#EXTINF:6.0,\\nseg_00000.ts\\n#EXT-X-ENDLIST\\n' > \"$last\"\n\
             exit 0\n",
        );

        let job = HlsSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 6,
            format: SegmentFormat::MpegTs,
        };
        let outcome = execute(&job, &fake_ffmpeg).expect("the fake ffmpeg must succeed and verify");
        assert_eq!(
            outcome.segment_count, 1,
            "segment_count must reflect the playlist's own single EXTINF entry, not the 3 .ts files \
             (2 of them stale) actually sitting in output_dir"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_writes_no_segments_is_reported_as_such() {
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\necho '#EXTM3U' > \"$last\"\nexit 0\n",
        );

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

    /// The HLS analogue of the finding's own DASH repro, reproduced against a genuine ffmpeg build
    /// (6.1.1): one real HLS job (a 6s source, 1s segments) into `output_dir` leaves several `.ts`
    /// segments, then a second, unrelated real HLS job (a 2s source) into the *same* `output_dir`
    /// writes a playlist declaring only one segment -- while the earlier job's extra `.ts` files are
    /// still sitting there, untouched (ffmpeg's HLS muxer never cleans a reused output directory).
    /// Before this module's fix, `segment_count` was a bare count of every `.ts` file in `output_dir`
    /// and would have overreported run 2's real output; confirms it no longer does.
    #[cfg(unix)]
    #[test]
    fn a_second_unrelated_real_ffmpeg_jobs_leftover_segments_are_never_counted_as_this_runs_output()
    {
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

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-real-ffmpeg-hls-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // Run 1's source: 6s video, 1s segments -> several real `.ts` files land in `out_dir`.
        let long_source = dir.join("source_long.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=10:duration=6",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "10",
                "-keyint_min",
                "10",
                long_source.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable to encode run 1's source");
        assert!(status.success(), "encoding run 1's source failed");

        let run1_job = HlsSegmentJob {
            source: long_source,
            output_dir: out_dir.clone(),
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 1,
            format: SegmentFormat::MpegTs,
        };
        let run1_outcome =
            execute(&run1_job, Path::new("ffmpeg")).expect("run 1 (6s source) must succeed");
        assert!(run1_outcome.segment_count > 1, "run 1 must leave more than one segment behind");
        let leftover_count = run1_outcome.segment_count;

        // Run 2's source: 2s video, into the *same* `output_dir`, via this crate's own, unmodified
        // `build_command`/`execute` -- no special-casing for the fact it's reused.
        let short_source = dir.join("source_short.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=10:duration=2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "10",
                "-keyint_min",
                "10",
                short_source.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable to encode run 2's source");
        assert!(status.success(), "encoding run 2's source failed");

        let run2_job = HlsSegmentJob {
            source: short_source,
            output_dir: out_dir.clone(),
            playlist_name: "stream.m3u8".into(),
            segment_seconds: 1,
            format: SegmentFormat::MpegTs,
        };
        let run2_outcome =
            execute(&run2_job, Path::new("ffmpeg")).expect("run 2 (2s source) must succeed");

        assert!(
            run2_outcome.segment_count < leftover_count,
            "run 2's own segment_count ({}) must be smaller than what run 1 left behind ({}) -- \
             otherwise this test's own fixture never reproduced stale leftovers in the first place",
            run2_outcome.segment_count,
            leftover_count
        );
        let on_disk_ts_files = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("ts"))
            .count();
        assert!(
            on_disk_ts_files > run2_outcome.segment_count,
            "run 1's stale segments must still be sitting in output_dir ({on_disk_ts_files} .ts \
             files) even though run 2 only really produced {}",
            run2_outcome.segment_count
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
