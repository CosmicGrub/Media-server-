//! Turning a [`PlaybackPlan`] into a concrete `ffmpeg` invocation.
//!
//! **Stage 1, honestly scoped: remux only, never a re-encode.** [`RemuxJob::from_plan`] accepts a
//! plan only when every stream either passes through untouched or is adapted through an operation
//! that is still a remux in substance -- a bitstream filter or a lossless-domain LPCM decode, never
//! lossy re-encoding. A plan whose [`VideoPath`] or [`AudioPath`] actually requires transcoding, or
//! whose [`SubtitleDelivery`] asks for burn-in or out-of-band extraction, is refused with a specific
//! [`PlanNotExecutable`] rather than silently downgraded or half-executed. Building the transcode
//! engine those plans need is real, separate future work -- not something to fake here.

use std::path::PathBuf;

use lumen_model::{AudioCodec, Container};
use lumen_playback::{AudioPath, ContainerPlan, PlaybackPlan, SubtitleDelivery, VideoPath};

/// What happens to the selected audio stream. Every variant here is still fundamentally a remux:
/// nothing lossy is re-encoded, matching `docs/13` §1.1's own ladder for lossless-domain audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioAdaptation {
    /// The bitstream copied exactly as it demuxes.
    Copy,
    /// Drop the DTS-HD/DTS:X extension substreams via ffmpeg's `dca_core` bitstream filter, keeping
    /// the embedded DTS core as the original bitstream -- no decode, no re-encode.
    ExtractDtsCore,
    /// Decode to LPCM at the plan's own sample rate and channel count. Sample-domain lossless; only
    /// object-based positioning (Atmos/DTS:X) is lost, which is why the ladder still scores this T1
    /// or T2 rather than T3.
    DecodeToLpcm { sample_rate: u32, channels: u8 },
}

/// A fully-resolved remux to run: everything `build_command` needs, with no further decisions left.
#[derive(Debug, Clone, PartialEq)]
pub struct RemuxJob {
    pub source: PathBuf,
    pub output: PathBuf,
    pub container: Container,
    pub audio: AudioAdaptation,
    /// `false` maps no subtitle stream at all (`-sn`) — `SubtitleDelivery::None`. `true` copies every
    /// subtitle stream through untouched — `SubtitleDelivery::InBand`. Anything else is refused by
    /// [`RemuxJob::from_plan`] before a job is ever built.
    pub include_subtitles: bool,
}

/// Why a [`PlaybackPlan`] cannot be handed to this Stage 1 executor as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanNotExecutable {
    /// `ContainerPlan::Original` (or the never-observable `Unavailable`) — Direct Play needs no
    /// remux at all; asking this crate to execute one would be a caller bug, not a real job.
    NothingToRemux,
    /// The plan calls for a real video re-encode, or dropping the video stream. Out of scope for a
    /// remux-only executor.
    RequiresVideoTranscode,
    /// The plan calls for a real audio re-encode (a lossy codec change or a channel-count reduction
    /// beyond what LPCM decode already covers).
    RequiresAudioTranscode,
    /// `CoreExtraction` was asked for a codec this executor does not yet know a verified,
    /// non-decoding ffmpeg bitstream filter for. Only `AudioCodec::Dts` (via `dca_core`) is
    /// implemented; claiming support for, say, a TrueHD-to-AC-3 core extraction without a filter this
    /// crate has actually verified exists would be worse than refusing.
    UnsupportedCoreExtraction(AudioCodec),
    /// Burning subtitles into the picture forces a video re-encode by definition.
    RequiresBurnIn,
    /// Out-of-band subtitle delivery needs a second extraction pass this Stage 1 executor does not
    /// yet perform -- the remux would have to either drop the subtitles or ship them in-band instead
    /// of what the plan actually asked for, and neither is an honest substitute.
    RequiresSubtitleExtraction,
}

impl PlanNotExecutable {
    pub fn explain(&self) -> &'static str {
        match self {
            Self::NothingToRemux => "the plan is Direct Play; there is nothing to remux",
            Self::RequiresVideoTranscode => {
                "the plan requires a video transcode, which this remux-only executor does not perform"
            }
            Self::RequiresAudioTranscode => {
                "the plan requires an audio transcode, which this remux-only executor does not perform"
            }
            Self::UnsupportedCoreExtraction(_) => {
                "no verified core-extraction bitstream filter is implemented for this codec yet"
            }
            Self::RequiresBurnIn => "burning in subtitles requires a video transcode",
            Self::RequiresSubtitleExtraction => {
                "out-of-band subtitle extraction is not yet implemented by this executor"
            }
        }
    }
}

impl std::fmt::Display for PlanNotExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.explain())
    }
}

impl std::error::Error for PlanNotExecutable {}

impl RemuxJob {
    /// Resolves `plan` into a job, or explains exactly why it cannot be executed yet.
    pub fn from_plan(
        plan: &PlaybackPlan,
        source: PathBuf,
        output: PathBuf,
    ) -> Result<Self, PlanNotExecutable> {
        let container = match plan.container {
            ContainerPlan::Remux(c) => c,
            ContainerPlan::Original | ContainerPlan::Unavailable => {
                return Err(PlanNotExecutable::NothingToRemux);
            }
        };
        if !matches!(plan.video, VideoPath::Copy) {
            return Err(PlanNotExecutable::RequiresVideoTranscode);
        }
        let include_subtitles = match plan.subtitle {
            SubtitleDelivery::None => false,
            SubtitleDelivery::InBand => true,
            SubtitleDelivery::OutOfBand { .. } => {
                return Err(PlanNotExecutable::RequiresSubtitleExtraction);
            }
            SubtitleDelivery::BurnedIn => return Err(PlanNotExecutable::RequiresBurnIn),
        };
        let audio = match &plan.audio {
            AudioPath::None | AudioPath::Passthrough | AudioPath::ExclusiveBitPerfect => {
                AudioAdaptation::Copy
            }
            AudioPath::DecodeToLpcm { channels, sample_rate, .. } => {
                AudioAdaptation::DecodeToLpcm { sample_rate: *sample_rate, channels: *channels }
            }
            AudioPath::CoreExtraction { core: AudioCodec::Dts } => AudioAdaptation::ExtractDtsCore,
            AudioPath::CoreExtraction { core } => {
                return Err(PlanNotExecutable::UnsupportedCoreExtraction(core.clone()));
            }
            AudioPath::Transcode { .. } => return Err(PlanNotExecutable::RequiresAudioTranscode),
        };
        Ok(Self { source, output, container, audio, include_subtitles })
    }
}

/// The `-f <name>` muxer and any format-specific flags this job's target container needs. `Err` for
/// any container this executor has not been taught a real, verified ffmpeg recipe for -- every remux
/// target `lumen-playback`'s own ladder can propose (`docs/13` §1.1: Matroska, fMP4, MP4, MPEG-TS,
/// WebM) is covered; nothing else is a valid `ContainerPlan::Remux` target today anyway.
pub(crate) fn ffmpeg_format(
    container: Container,
) -> Result<(&'static str, &'static [&'static str]), Container> {
    match container {
        Container::Matroska => Ok(("matroska", &[])),
        Container::WebM => Ok(("webm", &[])),
        // `faststart` moves `moov` to the front of the file -- `docs/13` §2's own "cheap win that is
        // frequently missed" -- essentially free to add here since the whole file is already being
        // rewritten.
        Container::Mp4 => Ok(("mp4", &["-movflags", "+faststart"])),
        Container::FragmentedMp4 => {
            Ok(("mp4", &["-movflags", "+frag_keyframe+empty_moov+default_base_moof"]))
        }
        Container::MpegTs => Ok(("mpegts", &[])),
        other => Err(other),
    }
}

/// Builds the full `ffmpeg` argument list for `job` -- pure and side-effect-free, so every case is
/// testable without a real `ffmpeg` binary anywhere on the machine running the tests.
pub fn build_command(job: &RemuxJob) -> Result<Vec<String>, Container> {
    let (format, extra_flags) = ffmpeg_format(job.container)?;

    let mut args = vec![
        // Overwrite without prompting: the caller owns `output`'s path (typically a cache location
        // it already decided to (re)write to), and a stalled ffmpeg waiting on a `y/N` prompt on its
        // own stdin would otherwise hang this process forever.
        "-y".to_string(),
        "-i".to_string(),
        job.source.to_string_lossy().into_owned(),
        // Map every stream from the one input -- stream *subsetting* (dropping specific unwanted
        // tracks) is a real `docs/13` §2 cheap win this Stage 1 executor does not yet perform; every
        // job today keeps everything the source has, modulo the subtitle on/off switch below.
        "-map".to_string(),
        "0".to_string(),
        "-c:v".to_string(),
        "copy".to_string(),
    ];

    match job.audio {
        AudioAdaptation::Copy => args.extend(["-c:a".to_string(), "copy".to_string()]),
        AudioAdaptation::ExtractDtsCore => {
            args.extend([
                "-c:a".to_string(),
                "copy".to_string(),
                "-bsf:a".to_string(),
                "dca_core".to_string(),
            ]);
        }
        AudioAdaptation::DecodeToLpcm { sample_rate, channels } => {
            args.extend([
                "-c:a".to_string(),
                "pcm_s24le".to_string(),
                "-ar".to_string(),
                sample_rate.to_string(),
                "-ac".to_string(),
                channels.to_string(),
            ]);
        }
    }

    if job.include_subtitles {
        args.extend(["-c:s".to_string(), "copy".to_string()]);
    } else {
        args.push("-sn".to_string());
    }

    args.extend(extra_flags.iter().map(|s| s.to_string()));
    args.extend(["-f".to_string(), format.to_string()]);
    args.push(job.output.to_string_lossy().into_owned());
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_model::SubtitleCodec;
    use lumen_playback::Tier;

    fn base_plan() -> PlaybackPlan {
        PlaybackPlan {
            container: ContainerPlan::Remux(Container::Matroska),
            video: VideoPath::Copy,
            audio: AudioPath::Passthrough,
            subtitle: SubtitleDelivery::InBand,
            tier: Tier::T1FullFidelity,
            rejections: Vec::new(),
        }
    }

    #[test]
    fn a_direct_play_plan_has_nothing_to_remux() {
        let mut plan = base_plan();
        plan.container = ContainerPlan::Original;
        let err = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap_err();
        assert_eq!(err, PlanNotExecutable::NothingToRemux);
    }

    #[test]
    fn a_plain_remux_copies_everything() {
        let job = RemuxJob::from_plan(&base_plan(), "in.mkv".into(), "out.mkv".into()).unwrap();
        assert_eq!(job.container, Container::Matroska);
        assert_eq!(job.audio, AudioAdaptation::Copy);
        assert!(job.include_subtitles);
    }

    #[test]
    fn a_video_transcode_is_refused_not_downgraded() {
        let mut plan = base_plan();
        plan.video = VideoPath::Drop;
        let err = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap_err();
        assert_eq!(err, PlanNotExecutable::RequiresVideoTranscode);
    }

    #[test]
    fn an_audio_transcode_is_refused() {
        let mut plan = base_plan();
        plan.audio = AudioPath::Transcode { codec: AudioCodec::Aac, channels: 2 };
        let err = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap_err();
        assert_eq!(err, PlanNotExecutable::RequiresAudioTranscode);
    }

    #[test]
    fn dts_core_extraction_is_supported_but_no_other_codec_is_claimed() {
        let mut plan = base_plan();
        plan.audio = AudioPath::CoreExtraction { core: AudioCodec::Dts };
        let job = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap();
        assert_eq!(job.audio, AudioAdaptation::ExtractDtsCore);

        let mut other = base_plan();
        other.audio = AudioPath::CoreExtraction { core: AudioCodec::Ac3 };
        let err = RemuxJob::from_plan(&other, "in.mkv".into(), "out.mkv".into()).unwrap_err();
        assert_eq!(err, PlanNotExecutable::UnsupportedCoreExtraction(AudioCodec::Ac3));
    }

    #[test]
    fn lpcm_decode_carries_the_planned_rate_and_channels_through() {
        let mut plan = base_plan();
        plan.audio = AudioPath::DecodeToLpcm {
            channels: 6,
            sample_rate: 48_000,
            resampled: false,
            objects_lost: true,
        };
        let job = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap();
        assert_eq!(job.audio, AudioAdaptation::DecodeToLpcm { sample_rate: 48_000, channels: 6 });
    }

    #[test]
    fn burn_in_and_out_of_band_subtitles_are_both_refused() {
        let mut burn = base_plan();
        burn.subtitle = SubtitleDelivery::BurnedIn;
        assert_eq!(
            RemuxJob::from_plan(&burn, "in.mkv".into(), "out.mkv".into()).unwrap_err(),
            PlanNotExecutable::RequiresBurnIn
        );

        let mut oob = base_plan();
        oob.subtitle = SubtitleDelivery::OutOfBand { as_format: SubtitleCodec::SubRip };
        assert_eq!(
            RemuxJob::from_plan(&oob, "in.mkv".into(), "out.mkv".into()).unwrap_err(),
            PlanNotExecutable::RequiresSubtitleExtraction
        );
    }

    #[test]
    fn no_subtitle_track_maps_to_the_sn_flag_not_a_copy_codec() {
        let mut plan = base_plan();
        plan.subtitle = SubtitleDelivery::None;
        let job = RemuxJob::from_plan(&plan, "in.mkv".into(), "out.mkv".into()).unwrap();
        assert!(!job.include_subtitles);
        let args = build_command(&job).unwrap();
        assert!(args.iter().any(|a| a == "-sn"));
        assert!(!args.windows(2).any(|w| w == ["-c:s", "copy"]));
    }

    #[test]
    fn build_command_produces_a_stream_copy_remux_into_matroska() {
        let job = RemuxJob {
            source: "in.mkv".into(),
            output: "out.mkv".into(),
            container: Container::Matroska,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        let args = build_command(&job).unwrap();
        assert_eq!(
            args,
            vec![
                "-y", "-i", "in.mkv", "-map", "0", "-c:v", "copy", "-c:a", "copy", "-c:s", "copy",
                "-f", "matroska", "out.mkv",
            ]
        );
    }

    #[test]
    fn build_command_extracts_the_dts_core_via_the_verified_bitstream_filter() {
        let job = RemuxJob {
            source: "in.mkv".into(),
            output: "out.mkv".into(),
            container: Container::Matroska,
            audio: AudioAdaptation::ExtractDtsCore,
            include_subtitles: false,
        };
        let args = build_command(&job).unwrap();
        assert!(args.windows(2).any(|w| w == ["-bsf:a", "dca_core"]));
        assert!(args.iter().any(|a| a == "-sn"));
    }

    #[test]
    fn build_command_decodes_to_lpcm_at_the_jobs_own_rate_and_channels() {
        let job = RemuxJob {
            source: "in.mkv".into(),
            output: "out.mkv".into(),
            container: Container::Matroska,
            audio: AudioAdaptation::DecodeToLpcm { sample_rate: 48_000, channels: 8 },
            include_subtitles: true,
        };
        let args = build_command(&job).unwrap();
        assert!(args.windows(2).any(|w| w == ["-c:a", "pcm_s24le"]));
        assert!(args.windows(2).any(|w| w == ["-ar", "48000"]));
        assert!(args.windows(2).any(|w| w == ["-ac", "8"]));
    }

    #[test]
    fn mp4_gets_a_faststart_rewrite_and_fragmented_mp4_gets_fragmentation_flags() {
        let mp4 = RemuxJob {
            source: "in.mkv".into(),
            output: "out.mp4".into(),
            container: Container::Mp4,
            audio: AudioAdaptation::Copy,
            include_subtitles: false,
        };
        let args = build_command(&mp4).unwrap();
        assert!(args.windows(2).any(|w| w == ["-movflags", "+faststart"]));
        assert!(args.iter().any(|a| a == "mp4"));

        let fmp4 = RemuxJob { container: Container::FragmentedMp4, ..mp4 };
        let args = build_command(&fmp4).unwrap();
        assert!(args.iter().any(|a| a.contains("frag_keyframe")));
    }

    #[test]
    fn a_container_the_ladder_never_proposes_as_a_remux_target_is_refused() {
        let job = RemuxJob {
            source: "in.avi".into(),
            output: "out.avi".into(),
            container: Container::Avi,
            audio: AudioAdaptation::Copy,
            include_subtitles: true,
        };
        assert_eq!(build_command(&job), Err(Container::Avi));
    }
}
