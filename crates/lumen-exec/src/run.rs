//! Actually running the `ffmpeg` invocation [`build_command`] describes, and sanity-checking what it
//! produced.
//!
//! **No new dependency for the subprocess itself.** `std::process::Command` is all a one-shot,
//! run-to-completion external process needs; this crate does not stream `ffmpeg`'s progress output or
//! manage a pool of concurrent jobs (a real gap for a production remux queue, left to whatever calls
//! this crate, not solved here).

use std::path::Path;
use std::time::{Duration, Instant};

use lumen_model::Container;

use crate::job::{RemuxJob, build_command};

/// What a completed remux produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub output_bytes: u64,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum ExecError {
    /// `job.container` is not one of the containers this executor has a verified `ffmpeg` recipe
    /// for -- see [`crate::job::ffmpeg_format`].
    UnsupportedContainer(Container),
    /// The `ffmpeg` binary itself could not be started (not found, not executable, ...).
    Spawn(std::io::Error),
    /// `ffmpeg` ran and exited with a failure status. Carries its stderr, since that is where a real
    /// ffmpeg failure (a codec/container mismatch this crate's own container matrix missed, a
    /// corrupt source, a full disk) explains itself.
    NonZeroExit { status: std::process::ExitStatus, stderr: String },
    /// `ffmpeg` exited successfully but the file it wrote does not sniff back as the container the
    /// job asked for -- a real bug worth surfacing loudly rather than shipping a mislabelled file.
    OutputMismatch { expected: Container, sniffed: Option<Container> },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedContainer(c) => write!(f, "no verified ffmpeg recipe for {c:?}"),
            Self::Spawn(e) => write!(f, "could not start ffmpeg: {e}"),
            Self::NonZeroExit { status, stderr } => {
                write!(f, "ffmpeg exited with {status}: {}", stderr.trim())
            }
            Self::OutputMismatch { expected, sniffed } => {
                write!(f, "expected the remux output to sniff as {expected:?}, got {sniffed:?}")
            }
        }
    }
}

impl std::error::Error for ExecError {}

/// Runs `job` with the `ffmpeg` binary at `ffmpeg_bin`, waits for it to finish, and confirms the
/// output actually is what was asked for.
pub fn execute(job: &RemuxJob, ffmpeg_bin: &Path) -> Result<ExecOutcome, ExecError> {
    let args = build_command(job).map_err(ExecError::UnsupportedContainer)?;

    let start = Instant::now();
    let output =
        std::process::Command::new(ffmpeg_bin).args(&args).output().map_err(ExecError::Spawn)?;
    let elapsed = start.elapsed();

    if !output.status.success() {
        return Err(ExecError::NonZeroExit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    verify_output(&job.output, job.container)?;

    let output_bytes = std::fs::metadata(&job.output).map(|m| m.len()).unwrap_or(0);
    Ok(ExecOutcome { output_bytes, elapsed })
}

/// Reads back the first few kilobytes of a just-written remux and confirms the top signature match
/// is the container the job asked for. The same head-bytes size `lumen scan`'s own sniffing already
/// uses, not a new convention invented here.
const SNIFF_BYTES: usize = 4096;

fn verify_output(path: &Path, expected: Container) -> Result<(), ExecError> {
    let head = read_head(path).unwrap_or_default();
    let sniffed = lumen_probe::sniff(&head).into_iter().next().map(|c| c.container);
    if sniffed == Some(expected) {
        Ok(())
    } else {
        Err(ExecError::OutputMismatch { expected, sniffed })
    }
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; SNIFF_BYTES];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::AudioAdaptation;

    fn write_matroska_stub(path: &Path) {
        // Just enough of the real EBML+Matroska DocType signature for `lumen_probe::sniff` to
        // recognise it -- a full stub, not a real playable file, is all `verify_output` needs.
        let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3];
        bytes.extend_from_slice(b"matroska");
        bytes.resize(64, 0);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn verify_output_accepts_a_file_that_sniffs_as_the_expected_container() {
        let dir = std::env::temp_dir().join(format!("lumen-exec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.mkv");
        write_matroska_stub(&path);
        assert!(verify_output(&path, Container::Matroska).is_ok());
        assert!(verify_output(&path, Container::Mp4).is_err(), "a Matroska file is not an MP4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_output_reports_a_missing_file_as_a_mismatch_not_a_panic() {
        let missing = std::env::temp_dir().join("lumen-exec-test-does-not-exist.mkv");
        let err = verify_output(&missing, Container::Matroska).unwrap_err();
        // `lumen_probe::sniff` never returns an empty list (G2 forbids "unsupported format"), so an
        // unreadable file still gets a sniffed candidate -- just never the expected one.
        assert!(matches!(
            err,
            ExecError::OutputMismatch { sniffed: Some(Container::RawElementaryStream), .. }
        ));
    }

    #[test]
    fn an_unsupported_container_is_refused_before_any_process_is_spawned() {
        let job = RemuxJob {
            source: "in.avi".into(),
            output: "out.avi".into(),
            container: Container::Avi,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        // A binary name that cannot possibly exist would still prove nothing was spawned; asserting
        // on the error variant is the real check.
        let err = execute(&job, Path::new("/nonexistent/ffmpeg")).unwrap_err();
        assert!(matches!(err, ExecError::UnsupportedContainer(Container::Avi)));
    }

    #[test]
    fn a_missing_ffmpeg_binary_is_a_spawn_error() {
        let job = RemuxJob {
            source: "in.mkv".into(),
            output: "out.mkv".into(),
            container: Container::Matroska,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        let err = execute(&job, Path::new("/definitely/not/a/real/ffmpeg/binary")).unwrap_err();
        assert!(matches!(err, ExecError::Spawn(_)));
    }

    // Exercises the real subprocess path end to end with a fake `ffmpeg` shell script, so the
    // spawn/wait/exit-status/stderr-capture logic is proven correct without depending on a real
    // ffmpeg being installed anywhere this test runs.
    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_succeeds_produces_a_verified_outcome() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("lumen-exec-fake-ffmpeg-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        let output_path = dir.join("out.mkv");
        // Ignores its real arguments and just writes a Matroska-signature stub to whatever `-y -i
        // <in> ... <out>` named as the last argument -- enough to prove `execute` really spawns,
        // waits, and then verifies the file that lands on disk, not a mocked-out shortcut.
        std::fs::write(
            &fake_ffmpeg,
            // POSIX `sh`, not bash: no `${@: -1}` array slicing, so the last argument is found by
            // walking every argument and keeping whichever one is seen last. `dash`'s builtin
            // `printf` does not understand `\xHH` hex escapes (it prints them literally), so the
            // EBML magic bytes are spelled out in octal instead -- \032=0x1A, \337=0xDF, \243=0xA3;
            // 0x45 is printable ASCII 'E'.
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\n\
             printf '\\032E\\337\\243matroska' > \"$last\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = RemuxJob {
            source: dir.join("in.mkv"),
            output: output_path.clone(),
            container: Container::Matroska,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        let outcome = execute(&job, &fake_ffmpeg).expect("the fake ffmpeg must succeed and verify");
        assert!(outcome.output_bytes > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_fails_reports_its_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-exec-fake-ffmpeg-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        std::fs::write(&fake_ffmpeg, "#!/bin/sh\necho 'Unknown encoder specified' 1>&2\nexit 1\n")
            .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = RemuxJob {
            source: dir.join("in.mkv"),
            output: dir.join("out.mkv"),
            container: Container::Matroska,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        let err = execute(&job, &fake_ffmpeg).unwrap_err();
        match err {
            ExecError::NonZeroExit { stderr, .. } => {
                assert!(stderr.contains("Unknown encoder"));
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every test above exercises a FAKE ffmpeg. `build_command` here has no auxiliary-filename
    /// option comparable to HLS's `-hls_fmp4_init_filename` (a real ffmpeg-version-specific
    /// path-resolution quirk `lumen-segment`'s own real-ffmpeg test exists to catch) -- every argument
    /// here is either a codec/bitstream-filter choice or the one `job.output` path, which a real
    /// ffmpeg build honors as given (already proven live: the same absolute-path behavior the HLS
    /// playlist path and segment pattern both rely on correctly). This test exists anyway, to convert
    /// that reasoning into a real, standing proof rather than trusting it — a real remux against a
    /// real ffmpeg build, verified both by `execute()`'s own content-sniffing and, independently, by
    /// `ffprobe`. Skipped, not failed, when `ffmpeg`/`ffprobe` are not on `PATH`.
    #[cfg(unix)]
    #[test]
    fn a_real_ffmpeg_build_remuxes_a_real_source_and_the_output_verifies_both_ways() {
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
            std::env::temp_dir().join(format!("lumen-exec-real-ffmpeg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

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

        let output = dir.join("out.mkv");
        let job = RemuxJob {
            source,
            output: output.clone(),
            container: Container::Matroska,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        let outcome = execute(&job, Path::new("ffmpeg")).expect(
            "a real ffmpeg build must accept exactly what build_command produces, and the \
                     remuxed output must verify against execute()'s own content-sniffing",
        );
        assert!(outcome.output_bytes > 0);

        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name",
                "-of",
                "csv=p=0",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("ffprobe must be runnable");
        assert!(
            probe.status.success(),
            "ffprobe rejected the real remux output: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let out = String::from_utf8_lossy(&probe.stdout);
        // Column order in ffprobe's csv output is not being relied on -- just that some line names
        // both the right codec_type and the right codec_name together, proving the remux really
        // stream-copied the source's own codecs rather than silently transcoding or dropping a track.
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
