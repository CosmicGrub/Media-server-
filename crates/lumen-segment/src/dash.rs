//! Building and running the `ffmpeg` invocation that cuts a source into DASH segments -- an MPD
//! manifest plus one independent per-representation sequence of init/media segment files. The DASH
//! counterpart to [`crate::command`]'s HLS muxing, following the same shape.
//!
//! **Segmenting only, never a re-encode** -- the same boundary [`crate::command`]'s own module doc
//! draws for HLS, drawn again here: every job stream-copies (`-c copy`), so it inherits that module's
//! own limitation of covering only sources whose codecs are already legal for the chosen container.
//! Confirmed live against a real ffmpeg build (6.1.1), not assumed: a source carrying a stream the
//! MP4-family DASH output can't carry (tried with an embedded SRT subtitle track) makes ffmpeg exit
//! non-zero ("Could not write header (incorrect codec parameters ?): Invalid argument"), which
//! [`execute`] reports as an ordinary [`DashExecError::NonZeroExit`] -- not a new problem DASH
//! introduces, just the same `-map 0 -c copy` container-legality limitation HLS already has, hit the
//! same way.
//!
//! **Representations segment independently, on their own frame/GOP boundaries.** Verified live: a
//! 6-second video+audio source cut at a 6-second target produced exactly one video chunk (its whole
//! duration landed in one GOP-aligned segment) but *two* audio chunks for the same wall-clock span --
//! normal and expected in DASH, unlike HLS's single shared segment timeline. Nothing in this module
//! assumes representations produce matching segment counts; [`execute`]'s own verification checks each
//! representation independently for exactly that reason.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The value ffmpeg's `-init_seg_name` is given, with `$RepresentationID$` substituted per
/// representation once ffmpeg reports which IDs it actually produced. Deliberately a bare relative
/// pattern, never `output_dir` joined onto it -- confirmed against a real ffmpeg build (6.1.1), not
/// read off ffmpeg's docs: **unlike** `-hls_fmp4_init_filename` (the exact path-doubling bug
/// `command.rs`'s own `INIT_SEGMENT_FILENAME` doc comment documents for HLS), the DASH muxer's
/// `-init_seg_name`/`-media_seg_name` do not resolve relative to the output directory regardless of
/// what they are given -- verified live by running exactly this bare pattern and confirming both the
/// files that actually landed on disk *and* the manifest's own `SegmentTemplate initialization=`
/// attribute reference the identical bare name, with no directory component anywhere. Kept bare
/// regardless of that: a DASH client resolves `initialization=`/`media=` relative to the manifest's
/// own URL, so an absolute filesystem path baked into the template would be meaningless to a real
/// client even if ffmpeg silently accepted it -- this is a deliberately correct choice, not the
/// accidental bug HLS shipped and had to be fixed after the fact.
const INIT_SEGMENT_NAME_PATTERN: &str = "init-$RepresentationID$.m4s";

/// The value ffmpeg's `-media_seg_name` is given -- see [`INIT_SEGMENT_NAME_PATTERN`]'s own doc
/// comment for why this stays a bare relative pattern. `$Number%05d$` matches ffmpeg's own observed
/// behavior live: 1-indexed, zero-padded to 5 digits, independently per representation (confirmed:
/// `chunk-0-00001.m4s`, `chunk-1-00001.m4s`, `chunk-1-00002.m4s`, ... -- representation "1" reaching
/// `00002` while representation "0" never does, for the same source, is not a bug).
const MEDIA_SEGMENT_NAME_PATTERN: &str = "chunk-$RepresentationID$-$Number%05d$.m4s";

/// One segmenting job: cut `source` into a DASH-MPD manifest plus fMP4 init/media segments for every
/// representation ffmpeg derives from it, targeting roughly `segment_seconds` per segment (ffmpeg
/// snaps to keyframe boundaries the same way its HLS muxer does -- see [`crate::plan`]'s own doc on
/// why "roughly"), writing everything into `output_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSegmentJob {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    pub manifest_name: String,
    pub segment_seconds: u32,
}

impl DashSegmentJob {
    pub fn manifest_path(&self) -> PathBuf {
        self.output_dir.join(&self.manifest_name)
    }
}

/// Builds the full `ffmpeg` argument list for `job` -- pure and side-effect-free, exactly like
/// [`crate::command::build_command`], so every case is testable without a real `ffmpeg` binary
/// anywhere. `-use_template 1 -use_timeline 1` is the combination confirmed live to produce a VOD
/// manifest whose `SegmentTemplate` carries a real `SegmentTimeline` of exact, keyframe-accurate
/// segment durations rather than an idealized fixed-duration template a real segment could drift from.
pub fn build_command(job: &DashSegmentJob) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-i".to_string(),
        job.source.to_string_lossy().into_owned(),
        "-map".to_string(),
        "0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-f".to_string(),
        "dash".to_string(),
        "-seg_duration".to_string(),
        job.segment_seconds.to_string(),
        "-use_template".to_string(),
        "1".to_string(),
        "-use_timeline".to_string(),
        "1".to_string(),
        "-init_seg_name".to_string(),
        INIT_SEGMENT_NAME_PATTERN.to_string(),
        "-media_seg_name".to_string(),
        MEDIA_SEGMENT_NAME_PATTERN.to_string(),
        job.manifest_path().to_string_lossy().into_owned(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashExecOutcome {
    pub manifest_path: PathBuf,
    pub representation_count: usize,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum DashExecError {
    Spawn(std::io::Error),
    NonZeroExit {
        status: std::process::ExitStatus,
        stderr: String,
    },
    /// `ffmpeg` exited successfully but never wrote the manifest file it was asked for.
    ManifestMissing,
    /// The manifest was written, but a hand-rolled scan for `<Representation id="...">` found none --
    /// an empty or absurdly short source, most likely.
    NoRepresentationsProduced,
    /// The manifest declared a representation that has no real init or media segment file sitting next
    /// to it on disk -- `why` says which is missing. This crate never serves what it hasn't confirmed
    /// is real, the same posture [`crate::command::execute`] already takes for HLS.
    RepresentationIncomplete {
        representation_id: String,
        why: &'static str,
    },
}

impl std::fmt::Display for DashExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start ffmpeg: {e}"),
            Self::NonZeroExit { status, stderr } => {
                write!(f, "ffmpeg exited with {status}: {}", stderr.trim())
            }
            Self::ManifestMissing => write!(f, "ffmpeg did not write the expected MPD manifest"),
            Self::NoRepresentationsProduced => {
                write!(f, "the manifest declared no representations")
            }
            Self::RepresentationIncomplete { representation_id, why } => {
                write!(f, "representation {representation_id:?} is incomplete: {why}")
            }
        }
    }
}

impl std::error::Error for DashExecError {}

/// Runs `job` with the `ffmpeg` binary at `ffmpeg_bin` and confirms real output landed on disk: the
/// manifest exists and names at least one representation, and for every representation it names, that
/// representation's own init segment (built from the same [`INIT_SEGMENT_NAME_PATTERN`] `build_command`
/// gave ffmpeg, with `$RepresentationID$` substituted) is a real, non-empty file, and at least one
/// `chunk-<id>-*.m4s` file for it exists in `output_dir`. This is a presence check, the DASH analogue
/// of [`crate::command::execute`]'s own posture -- it does not parse or validate `SegmentTimeline`
/// math, which needs no re-derivation here: a real chunk file landing on disk under the name the
/// manifest's own template promises is what matters, not modeling DASH's segment-addressing arithmetic
/// a second time.
pub fn execute(job: &DashSegmentJob, ffmpeg_bin: &Path) -> Result<DashExecOutcome, DashExecError> {
    let args = build_command(job);

    let start = Instant::now();
    let output = std::process::Command::new(ffmpeg_bin)
        .args(&args)
        .output()
        .map_err(DashExecError::Spawn)?;
    let elapsed = start.elapsed();

    if !output.status.success() {
        return Err(DashExecError::NonZeroExit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let manifest_path = job.manifest_path();
    if !manifest_path.is_file() {
        return Err(DashExecError::ManifestMissing);
    }
    let text =
        std::fs::read_to_string(&manifest_path).map_err(|_| DashExecError::ManifestMissing)?;

    let representation_ids = parse_representation_ids(&text);
    if representation_ids.is_empty() {
        return Err(DashExecError::NoRepresentationsProduced);
    }

    for id in &representation_ids {
        let init_name = INIT_SEGMENT_NAME_PATTERN.replace("$RepresentationID$", id);
        match std::fs::metadata(job.output_dir.join(&init_name)) {
            Ok(m) if m.len() > 0 => {}
            _ => {
                return Err(DashExecError::RepresentationIncomplete {
                    representation_id: id.clone(),
                    why: "init segment is missing or empty",
                });
            }
        }
        if !has_chunk_for(&job.output_dir, id) {
            return Err(DashExecError::RepresentationIncomplete {
                representation_id: id.clone(),
                why: "no chunk segment files were found",
            });
        }
    }

    Ok(DashExecOutcome { manifest_path, representation_count: representation_ids.len(), elapsed })
}

/// A `fs::read_dir` glob-style scan for at least one real `chunk-<representation_id>-*.m4s` file --
/// deliberately not parsing `SegmentTimeline` to know exactly how many to expect (see [`execute`]'s
/// own doc comment on why): a real file existing under the name pattern the manifest's own template
/// promises is the only thing this needs to prove.
fn has_chunk_for(output_dir: &Path, representation_id: &str) -> bool {
    let prefix = format!("chunk-{representation_id}-");
    std::fs::read_dir(output_dir).into_iter().flatten().flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with(&prefix) && name.ends_with(".m4s")
    })
}

/// Hand-rolled `<Representation id="...">` scanner -- bounded and total, the same "malformed input is
/// skipped, never a panic" posture every other parser in this workspace takes (mirroring
/// `remote/server/hls.rs`'s own `parse_extinf_pairs`). Not a general XML parser: this crate only ever
/// needs the one thing a real ffmpeg-written manifest's own structure guarantees -- an `id="..."`
/// attribute on each `<Representation` element's own opening tag -- never the wider MPD schema
/// (`AdaptationSet` nesting, `SegmentTemplate`/`SegmentTimeline` internals) this module deliberately
/// does not model.
fn parse_representation_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(tag_pos) = rest.find("<Representation") {
        rest = &rest[tag_pos + "<Representation".len()..];
        // Bounded to this element's own opening tag (up to its closing `>`) so an attribute belonging
        // to a later element can never be mistaken for this one's.
        let Some(tag_end) = rest.find('>') else { break };
        if let Some(id) = extract_attr(&rest[..tag_end], "id") {
            out.push(id);
        }
        rest = &rest[tag_end..];
    }
    out
}

/// Finds `name="..."` inside one tag's own attribute text and returns the quoted value. `None` for
/// anything malformed (no such attribute, an unterminated quote) rather than panicking.
fn extract_attr(tag_attrs: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag_attrs.find(&needle)? + needle.len();
    let end = tag_attrs[start..].find('"')?;
    Some(tag_attrs[start..start + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> DashSegmentJob {
        DashSegmentJob {
            source: "in.mkv".into(),
            output_dir: "out".into(),
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 6,
        }
    }

    #[test]
    fn build_command_targets_the_dash_muxer_with_template_and_timeline_addressing() {
        let args = build_command(&job());
        assert!(args.windows(2).any(|w| w == ["-f", "dash"]));
        assert!(args.windows(2).any(|w| w == ["-seg_duration", "6"]));
        assert!(args.windows(2).any(|w| w == ["-use_template", "1"]));
        assert!(args.windows(2).any(|w| w == ["-use_timeline", "1"]));
    }

    #[test]
    fn init_and_media_segment_names_are_bare_patterns_not_joined_onto_output_dir() {
        // Confirmed live against a real ffmpeg build that this pattern (unlike HLS's
        // `-hls_fmp4_init_filename`) is *not* subject to a path-doubling bug -- see this module's own
        // `INIT_SEGMENT_NAME_PATTERN` doc comment. Kept bare regardless, since a DASH client resolves
        // these relative to the manifest's own URL either way.
        let args = build_command(&job());
        assert!(
            args.windows(2).any(|w| w == ["-init_seg_name", "init-$RepresentationID$.m4s"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["-media_seg_name", "chunk-$RepresentationID$-$Number%05d$.m4s"]),
            "{args:?}"
        );
    }

    #[test]
    fn the_manifest_path_is_the_final_argument() {
        let args = build_command(&job());
        assert!(args.last().unwrap().ends_with("manifest.mpd"));
    }

    #[test]
    fn the_source_is_stream_copied_never_re_encoded() {
        let args = build_command(&job());
        assert!(args.windows(2).any(|w| w == ["-c", "copy"]));
    }

    #[test]
    fn representation_ids_are_read_in_document_order() {
        let text = r#"<MPD><Period>
            <AdaptationSet><Representation id="0" mimeType="video/mp4"></Representation></AdaptationSet>
            <AdaptationSet><Representation id="1" mimeType="audio/mp4"></Representation></AdaptationSet>
        </Period></MPD>"#;
        assert_eq!(parse_representation_ids(text), vec!["0".to_string(), "1".to_string()]);
    }

    #[test]
    fn a_manifest_with_no_representations_is_an_empty_list_not_a_panic() {
        assert!(parse_representation_ids("<MPD></MPD>").is_empty());
        assert!(parse_representation_ids("").is_empty());
        // An unterminated tag -- no closing `>` -- must not panic or loop forever.
        assert!(parse_representation_ids("<Representation id=\"0\"").is_empty());
    }

    #[test]
    fn an_attribute_from_a_later_tag_is_never_attributed_to_an_earlier_one() {
        // Regression guard for the bound on `extract_attr`'s search: it must be scoped to one tag's
        // own text, not the rest of the document.
        let text = r#"<Representation mimeType="video/mp4"><X id="not-mine"/></Representation>
                       <Representation id="1"></Representation>"#;
        assert_eq!(parse_representation_ids(text), vec!["1".to_string()]);
    }

    #[test]
    fn has_chunk_for_matches_only_its_own_representations_prefix() {
        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-dash-chunk-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chunk-0-00001.m4s"), b"x").unwrap();
        std::fs::write(dir.join("chunk-10-00001.m4s"), b"x").unwrap();
        assert!(has_chunk_for(&dir, "0"));
        assert!(has_chunk_for(&dir, "10"));
        assert!(!has_chunk_for(&dir, "1"), "\"1\" must not match \"chunk-10-...\" by prefix alone");
        assert!(!has_chunk_for(&dir, "2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_ffmpeg_binary_is_a_spawn_error() {
        let err = execute(&job(), Path::new("/definitely/not/real/ffmpeg")).unwrap_err();
        assert!(matches!(err, DashExecError::Spawn(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_succeeds_reports_every_representation_it_wrote() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-ok-{}", std::process::id()));
        // A stale directory from an earlier, differently-shaped run under a recycled PID (this
        // sandbox's PID space is small enough for that to happen across repeated test runs) must never
        // be reused as-is: `create_dir_all` alone does not clear pre-existing content, so a leftover
        // file from a previous scenario could silently satisfy this test's own assertions.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        // Ignores its real arguments and writes a two-representation manifest plus real init/chunk
        // files -- enough to prove `execute` really spawns, waits, parses the manifest it wrote, and
        // then verifies what actually landed on disk against it.
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/init-0.m4s\"\n\
             printf x > \"$dir/init-1.m4s\"\n\
             printf x > \"$dir/chunk-0-00001.m4s\"\n\
             printf x > \"$dir/chunk-1-00001.m4s\"\n\
             printf x > \"$dir/chunk-1-00002.m4s\"\n\
             printf '<MPD><Period>' > \"$last\"\n\
             printf '<AdaptationSet><Representation id=\"0\"></Representation></AdaptationSet>' \
             >> \"$last\"\n\
             printf '<AdaptationSet><Representation id=\"1\"></Representation></AdaptationSet>' \
             >> \"$last\"\n\
             printf '</Period></MPD>' >> \"$last\"\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = DashSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 6,
        };
        let outcome = execute(&job, &fake_ffmpeg).expect("the fake ffmpeg must succeed and verify");
        assert_eq!(outcome.representation_count, 2);
        assert!(outcome.manifest_path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_representation_missing_its_chunk_files_is_reported_as_incomplete() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-incomplete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        // Declares representation "0" in the manifest but never writes its chunk file -- the exact
        // "manifest says it exists, disk disagrees" case `execute`'s own verification exists to catch.
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/init-0.m4s\"\n\
             printf '<MPD><Period><AdaptationSet><Representation id=\"0\">' > \"$last\"\n\
             printf '</Representation></AdaptationSet></Period></MPD>' >> \"$last\"\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = DashSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 6,
        };
        let err = execute(&job, &fake_ffmpeg).unwrap_err();
        assert!(matches!(err, DashExecError::RepresentationIncomplete { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_writes_no_representations_is_reported_as_such() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '<MPD></MPD>' > \"$last\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let job = DashSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 6,
        };
        let err = execute(&job, &fake_ffmpeg).unwrap_err();
        assert!(matches!(err, DashExecError::NoRepresentationsProduced));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every other test above proves `build_command`'s output parses and `execute` spawns/verifies
    /// correctly -- against a FAKE ffmpeg that ignores its real arguments. This is the one test that
    /// exercises a genuine ffmpeg build, and it is what actually confirms the claims this module's own
    /// doc comments make about real DASH-MPD output: bare relative `SegmentTemplate` addressing,
    /// independent per-representation segment counts, and genuinely playable, codec-preserving
    /// reassembled output. Skipped, not failed, when `ffmpeg`/`ffprobe` are not on `PATH` -- the same
    /// convention `command.rs`'s own real-ffmpeg test already uses.
    #[cfg(unix)]
    #[test]
    fn a_real_ffmpeg_build_produces_a_valid_playable_dash_manifest_and_segments() {
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

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-real-ffmpeg-dash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // A short, real, stream-copyable video+audio source -- encoded with ffmpeg itself (via lavfi
        // test sources) so this test needs no other tool on `PATH`.
        let source = dir.join("source.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=10:duration=6",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
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

        let job = DashSegmentJob {
            source,
            output_dir: out_dir.clone(),
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 6,
        };
        let outcome = execute(&job, Path::new("ffmpeg"))
            .expect("a real ffmpeg build must accept exactly what build_command produces");
        assert_eq!(
            outcome.representation_count, 2,
            "a video+audio source must produce exactly two representations"
        );

        let manifest_text = std::fs::read_to_string(&outcome.manifest_path).unwrap();
        // The real proof this isn't the HLS bug in a new shape: the manifest's own SegmentTemplate
        // must reference the same bare, relative names `-init_seg_name`/`-media_seg_name` were given,
        // never an absolute build-directory path leaking into content ffmpeg itself decided on.
        assert!(
            manifest_text.contains(r#"initialization="init-$RepresentationID$.m4s""#),
            "manifest must reference the bare init pattern:\n{manifest_text}"
        );
        assert!(
            !manifest_text.contains(dir.to_str().unwrap()),
            "the manifest must never leak the build directory's own absolute path:\n{manifest_text}"
        );

        let init0 = out_dir.join("init-0.m4s");
        let chunk0 = out_dir.join("chunk-0-00001.m4s");
        let init1 = out_dir.join("init-1.m4s");
        assert!(init0.is_file(), "representation 0's init segment must exist");
        assert!(chunk0.is_file(), "representation 0's first chunk must exist");
        assert!(init1.is_file(), "representation 1's init segment must exist");

        // No `.tmp`-suffixed leftovers -- confirmed live that ffmpeg's DASH muxer finalizes every
        // output file (manifest, init, and chunk segments alike) atomically, the same as its HLS muxer.
        let leftover_tmp = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!leftover_tmp, "no .tmp file should remain once ffmpeg has exited successfully");

        // Concatenate init + first chunk for the video representation -- exactly how a real DASH
        // player consumes them -- and confirm `ffprobe` (a second, independent real tool) reads back
        // genuinely valid, playable media carrying the source's own stream-copied codecs.
        let combined = dir.join("combined.mp4");
        let mut bytes = std::fs::read(&init0).unwrap();
        bytes.extend(std::fs::read(&chunk0).unwrap());
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
            "ffprobe rejected the reassembled init+chunk: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let out = String::from_utf8_lossy(&probe.stdout);
        assert!(
            out.lines().any(|l| l.contains("h264") && l.contains("video")),
            "expected a stream-copied h264 video stream:\n{out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
