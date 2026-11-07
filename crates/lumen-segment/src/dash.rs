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
    /// The manifest declared a representation whose real on-disk chunk files don't match what the
    /// manifest itself says -- `why` says how: missing entirely, or present but in the wrong count for
    /// the manifest's own `SegmentTimeline` (the signature of a stale chunk left over from a prior,
    /// unrelated job into the same `output_dir` -- see [`has_chunk_for`]'s own doc comment). This crate
    /// never serves what it hasn't confirmed is real, the same posture [`crate::command::execute`]
    /// already takes for HLS.
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
/// gave ffmpeg, with `$RepresentationID$` substituted) is a real, non-empty file, and the
/// `chunk-<id>-*.m4s` files for it in `output_dir` match what that representation's own
/// `SegmentTimeline` declares. **Precondition this does not itself enforce: `output_dir` should be
/// fresh for this job** -- a glob alone cannot tell a chunk `ffmpeg` just wrote from a same-named one
/// a prior, unrelated job into the same directory left behind, since ffmpeg's DASH muxer never cleans
/// a reused output directory, only overwrites the exact filenames it rewrites. What *is* checked here
/// as a direct defense against exactly that: the count of `chunk-<id>-*.m4s` files on disk is compared
/// against the number of segments this representation's own `SegmentTimeline` declares (summing each
/// `<S .../>` entry's `r+1`) -- when the manifest provides one, a stale leftover chunk sitting under
/// the right name pattern inflates the on-disk count past what the manifest itself says should be
/// there, and is caught as [`DashExecError::RepresentationIncomplete`] rather than silently accepted.
/// This still does not parse or re-derive `SegmentTimeline` *duration* math -- only the segment counts
/// it already encodes via `r`.
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
    let representation_bodies = parse_representation_bodies(&text);

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

        let chunk_count = count_chunks_for(&job.output_dir, id);
        if chunk_count == 0 {
            return Err(DashExecError::RepresentationIncomplete {
                representation_id: id.clone(),
                why: "no chunk segment files were found",
            });
        }
        // `expected_chunk_count` is 0 when this representation's own body carries no (parseable)
        // `SegmentTimeline` -- nothing to compare against, so only the presence check above applies.
        let expected = representation_bodies
            .iter()
            .find(|(rid, _)| rid == id)
            .map(|(_, body)| expected_chunk_count(body))
            .unwrap_or(0);
        if expected > 0 && chunk_count != expected {
            return Err(DashExecError::RepresentationIncomplete {
                representation_id: id.clone(),
                why: "chunk segment file count on disk does not match the manifest's own \
                      SegmentTimeline -- a stale chunk left over from a different, unrelated job into \
                      the same output_dir, most likely",
            });
        }
    }

    Ok(DashExecOutcome { manifest_path, representation_count: representation_ids.len(), elapsed })
}

/// A `fs::read_dir` glob-style scan counting every real `chunk-<representation_id>-*.m4s` file --
/// deliberately not *only* checking presence (a bare `.any()` glob match): a name landing in
/// `output_dir` under the right pattern is not on its own proof it came from *this* `ffmpeg`
/// invocation rather than a differently-shaped, unrelated prior job into the same directory, since
/// ffmpeg's DASH muxer never cleans a reused output directory, only overwrites the exact filenames it
/// rewrites -- confirmed live (ffmpeg 6.1.1): a representation id reassigned between two unrelated
/// jobs into the same `output_dir` leaves every chunk number the new job never reached still sitting
/// under the old job's content. Returning the real count (not just whether it's nonzero) lets
/// [`execute`] compare it against the manifest's own `SegmentTimeline`-declared count and catch
/// exactly that leftover.
fn count_chunks_for(output_dir: &Path, representation_id: &str) -> usize {
    let prefix = format!("chunk-{representation_id}-");
    std::fs::read_dir(output_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".m4s")
        })
        .count()
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

/// Same bounded scan [`parse_representation_ids`] does, but also keeps each `<Representation
/// id="...">` element's own body text around -- from just after its opening tag's `>` to its own
/// `</Representation>` -- so [`expected_chunk_count`] has something scoped to count `<S .../>` entries
/// in, without that count ever being attributed to the wrong representation. Real ffmpeg output never
/// nests one `<Representation>` inside another, so the first `</Representation>` reached is always
/// this element's own -- the same non-general-XML, bounded-by-this-element's-own-tags posture
/// [`parse_representation_ids`]'s own doc comment takes.
fn parse_representation_bodies(text: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(tag_pos) = rest.find("<Representation") {
        rest = &rest[tag_pos + "<Representation".len()..];
        let Some(tag_end) = rest.find('>') else { break };
        let id = extract_attr(&rest[..tag_end], "id");
        let body_start = tag_end + 1;
        let body = match rest[body_start..].find("</Representation>") {
            Some(end) => &rest[body_start..body_start + end],
            None => "",
        };
        if let Some(id) = id {
            out.push((id, body));
        }
        rest = &rest[tag_end..];
    }
    out
}

/// Counts how many media segments one representation's own `<SegmentTimeline>` declares -- summing
/// `r+1` for every `<S .../>` entry (DASH's run-length encoding: "this duration, repeated `r` more
/// times"; `r` defaults to 0, meaning "appears once"). Scoped to `representation_body` -- one
/// `<Representation>` element's own text, from [`parse_representation_bodies`] -- so a `<S .../>`
/// belonging to a different representation's `SegmentTimeline` is never counted here. Returns 0 for a
/// body with no `<S .../>` entries at all (no `SegmentTimeline`, or one [`execute`]'s caller never
/// asked ffmpeg to produce); callers treat 0 as "nothing to compare against", not "zero segments
/// expected".
fn expected_chunk_count(representation_body: &str) -> usize {
    let mut count = 0usize;
    let mut rest = representation_body;
    while let Some(tag_pos) = rest.find("<S ") {
        rest = &rest[tag_pos + "<S ".len()..];
        let Some(tag_end) = rest.find('>') else { break };
        let attrs = &rest[..tag_end];
        let repeat = extract_attr(attrs, "r").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        count += repeat + 1;
        rest = &rest[tag_end..];
    }
    count
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
    fn count_chunks_for_matches_only_its_own_representations_prefix() {
        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-dash-chunk-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chunk-0-00001.m4s"), b"x").unwrap();
        std::fs::write(dir.join("chunk-10-00001.m4s"), b"x").unwrap();
        std::fs::write(dir.join("chunk-10-00002.m4s"), b"x").unwrap();
        assert_eq!(count_chunks_for(&dir, "0"), 1);
        assert_eq!(count_chunks_for(&dir, "10"), 2);
        assert_eq!(
            count_chunks_for(&dir, "1"),
            0,
            "\"1\" must not match \"chunk-10-...\" by prefix alone"
        );
        assert_eq!(count_chunks_for(&dir, "2"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expected_chunk_count_sums_segment_timeline_repeat_counts() {
        // Real shape confirmed live against ffmpeg 6.1.1: a `SegmentTimeline` with a single `<S r="9"
        // />` entry (9 repeats, meaning 10 total) for one representation and a four-entry timeline
        // (1 + 8 + 1 + 1 = 11) for another, nested inside each one's own `<Representation>`.
        let video_body = r#"
            <SegmentTemplate timescale="10240" initialization="init-0.m4s" media="chunk-0-$Number%05d$.m4s" startNumber="1">
                <SegmentTimeline>
                    <S t="0" d="10240" r="9" />
                </SegmentTimeline>
            </SegmentTemplate>"#;
        assert_eq!(expected_chunk_count(video_body), 10);

        let audio_body = r#"
            <SegmentTemplate timescale="44100" initialization="init-1.m4s" media="chunk-1-$Number%05d$.m4s" startNumber="1">
                <SegmentTimeline>
                    <S t="0" d="35840" />
                    <S d="44032" r="7" />
                    <S d="45056" />
                    <S d="7848" />
                </SegmentTimeline>
            </SegmentTemplate>"#;
        assert_eq!(expected_chunk_count(audio_body), 11);

        assert_eq!(
            expected_chunk_count(""),
            0,
            "no SegmentTimeline at all means nothing to compare"
        );
    }

    #[test]
    fn parse_representation_bodies_scopes_each_bodys_segment_timeline_to_its_own_id() {
        let text = r#"<MPD><Period>
            <AdaptationSet><Representation id="0" mimeType="video/mp4">
                <SegmentTemplate><SegmentTimeline><S t="0" d="1" r="1" /></SegmentTimeline></SegmentTemplate>
            </Representation></AdaptationSet>
            <AdaptationSet><Representation id="1" mimeType="audio/mp4">
                <SegmentTemplate><SegmentTimeline><S t="0" d="1" /></SegmentTimeline></SegmentTemplate>
            </Representation></AdaptationSet>
        </Period></MPD>"#;
        let bodies = parse_representation_bodies(text);
        assert_eq!(bodies.len(), 2);
        let video = bodies.iter().find(|(id, _)| id == "0").unwrap();
        let audio = bodies.iter().find(|(id, _)| id == "1").unwrap();
        assert_eq!(expected_chunk_count(video.1), 2);
        assert_eq!(expected_chunk_count(audio.1), 1);
    }

    #[test]
    fn a_missing_ffmpeg_binary_is_a_spawn_error() {
        let err = execute(&job(), Path::new("/definitely/not/real/ffmpeg")).unwrap_err();
        assert!(matches!(err, DashExecError::Spawn(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_succeeds_reports_every_representation_it_wrote() {
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

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
        crate::test_support::write_executable_script(
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
        );

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
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-incomplete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        // Declares representation "0" in the manifest but never writes its chunk file -- the exact
        // "manifest says it exists, disk disagrees" case `execute`'s own verification exists to catch.
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/init-0.m4s\"\n\
             printf '<MPD><Period><AdaptationSet><Representation id=\"0\">' > \"$last\"\n\
             printf '</Representation></AdaptationSet></Period></MPD>' >> \"$last\"\n\
             exit 0\n",
        );

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
    fn a_stale_leftover_chunk_from_an_unrelated_prior_job_is_never_mistaken_for_this_runs_real_output()
     {
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // Seeds `output_dir` with exactly the finding's own live repro shape: 10 leftover
        // `chunk-0-*.m4s` files from an earlier, unrelated job that assigned representation id "0" to
        // a 10-segment video track, before this job's fake ffmpeg ever runs.
        for n in 1..=10 {
            std::fs::write(out_dir.join(format!("chunk-0-{n:05}.m4s")), b"stale-video-bytes")
                .unwrap();
        }

        let fake_ffmpeg = dir.join("ffmpeg");
        // This job's own real output: representation id "0" reassigned to a 2-segment audio track --
        // writes only `init-0.m4s` and overwrites `chunk-0-00001.m4s`/`chunk-0-00002.m4s`, exactly
        // like a real ffmpeg DASH muxer does, never touching (let alone removing) 00003..00010.
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             dir=$(dirname \"$last\")\n\
             printf x > \"$dir/init-0.m4s\"\n\
             printf x > \"$dir/chunk-0-00001.m4s\"\n\
             printf x > \"$dir/chunk-0-00002.m4s\"\n\
             printf '<MPD><Period><AdaptationSet><Representation id=\"0\" mimeType=\"audio/mp4\">' \
             > \"$last\"\n\
             printf '<SegmentTemplate><SegmentTimeline>' >> \"$last\"\n\
             printf '<S t=\"0\" d=\"44032\" /><S d=\"45050\" />' >> \"$last\"\n\
             printf '</SegmentTimeline></SegmentTemplate>' >> \"$last\"\n\
             printf '</Representation></AdaptationSet></Period></MPD>' >> \"$last\"\n\
             exit 0\n",
        );

        let job = DashSegmentJob {
            source: dir.join("in.mkv"),
            output_dir: out_dir,
            manifest_name: "manifest.mpd".into(),
            segment_seconds: 1,
        };
        let err = execute(&job, &fake_ffmpeg).expect_err(
            "8 stale leftover chunks under the reassigned id must be caught, not silently accepted \
             as this run's real output",
        );
        match err {
            DashExecError::RepresentationIncomplete { representation_id, why } => {
                assert_eq!(representation_id, "0");
                assert!(why.contains("does not match"), "{why}");
            }
            other => panic!("expected RepresentationIncomplete, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_subprocess_that_writes_no_representations_is_reported_as_such() {
        let _guard = crate::test_support::SCRIPT_WRITE.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir()
            .join(format!("lumen-segment-fake-ffmpeg-dash-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg");
        crate::test_support::write_executable_script(
            &fake_ffmpeg,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '<MPD></MPD>' > \"$last\"\nexit 0\n",
        );

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

    /// The finding's own live repro, reproduced against a genuine ffmpeg build (6.1.1): one real DASH
    /// job (10s video+audio, forced 1s keyframes so representation "0"=video genuinely produces 10
    /// chunks) into `output_dir`, then a second, unrelated real DASH job (2s audio-only) into the
    /// *same* `output_dir` -- whose own manifest reassigns representation id "0" to audio with only a
    /// 2-entry `SegmentTimeline`, while `chunk-0-00003.m4s` through `chunk-0-00010.m4s` on disk are
    /// still run 1's video bytes. Before this module's fix, `execute` accepted run 2 as fully verified
    /// output; confirms it no longer does. Skipped, not failed, when `ffmpeg` is not on `PATH`.
    #[cfg(unix)]
    #[test]
    fn two_unrelated_real_ffmpeg_jobs_into_the_same_output_dir_the_second_is_never_silently_accepted()
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
            .join(format!("lumen-segment-real-ffmpeg-dash-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_dir = dir.join("outdash3");
        std::fs::create_dir_all(&out_dir).unwrap();

        // Run 1's source: 10s video+audio, encoded with ffmpeg's own lavfi test sources.
        let long_source = dir.join("source_long.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=10:duration=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=10",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "10",
                "-keyint_min",
                "10",
                "-c:a",
                "aac",
                long_source.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable to encode run 1's source");
        assert!(status.success(), "encoding run 1's source failed");

        // Run 1 itself: ffmpeg's real DASH muxer, forcing a 1s keyframe interval (`-g`/`-keyint_min`)
        // so representation "0" (video) genuinely produces 10 one-second chunks -- matching the
        // finding's own repro exactly. `build_command` has no knob for `-g`/`-keyint_min`, so run 1 is
        // built by hand here; run 2 below uses this crate's real, unmodified `execute`.
        let run1_status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                long_source.to_str().unwrap(),
                "-map",
                "0",
                "-c:v",
                "libx264",
                "-g",
                "10",
                "-keyint_min",
                "10",
                "-c:a",
                "copy",
                "-f",
                "dash",
                "-seg_duration",
                "1",
                "-use_template",
                "1",
                "-use_timeline",
                "1",
                "-init_seg_name",
                INIT_SEGMENT_NAME_PATTERN,
                "-media_seg_name",
                MEDIA_SEGMENT_NAME_PATTERN,
                out_dir.join("manifest.mpd").to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable for run 1");
        assert!(run1_status.success(), "run 1 (video+audio) must succeed");
        // Confirms the fixture really does produce the shape the finding describes: 10 video chunks
        // under representation id "0" before run 2 ever starts.
        assert_eq!(count_chunks_for(&out_dir, "0"), 10, "run 1 must leave exactly 10 video chunks");

        // Run 2's source: 2s audio-only, into the *same* `output_dir`, via this crate's own,
        // unmodified `build_command`/`execute` -- no special-casing for the fact it's reused.
        let short_source = dir.join("source_short_audio.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:a",
                "aac",
                short_source.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be runnable to encode run 2's source");
        assert!(status.success(), "encoding run 2's source failed");

        let run2_job = DashSegmentJob {
            source: short_source,
            output_dir: out_dir.clone(),
            manifest_name: "manifest.mpd".to_string(),
            segment_seconds: 1,
        };
        let err = execute(&run2_job, Path::new("ffmpeg")).expect_err(
            "run 2 must be rejected: representation \"0\" is now audio with a 2-entry \
             SegmentTimeline, but 8 stale video chunks from run 1 are still sitting under that id",
        );
        match err {
            DashExecError::RepresentationIncomplete { representation_id, why } => {
                assert_eq!(representation_id, "0");
                assert!(why.contains("does not match"), "{why}");
            }
            other => panic!("expected RepresentationIncomplete, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
