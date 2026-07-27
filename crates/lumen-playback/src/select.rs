//! Automatic track selection — `docs/12` §4.
//!
//! Getting this wrong reads to users as a compatibility failure even when every stream decoded
//! perfectly: "the player picked the wrong audio" and "my subtitles didn't come on" are reported the
//! same way as "it won't play".
//!
//! The forced-subtitle rule in particular — when the audio is already in your language, show only
//! the subtitles for the foreign dialogue — is one of the highest-value automatic behaviours in a
//! media player, and one almost nothing implements correctly.

use lumen_caps::ClientCapabilities;
use lumen_model::{AudioStream, Language, MediaSource, SubtitleStream};

use crate::ladder::Selection;

/// Per-user track preferences.
#[derive(Debug, Clone, Default)]
pub struct TrackPreferences {
    /// Ordered audio language preference, most-wanted first.
    pub audio_languages: Vec<Language>,
    /// Ordered subtitle language preference.
    pub subtitle_languages: Vec<Language>,
    /// Prefer the production's original language over a dub.
    pub prefer_original_audio: bool,
    /// Prefer subtitles for the deaf and hard of hearing.
    pub prefer_sdh: bool,
    /// Show full subtitles even when the audio is already in a preferred language.
    pub always_show_subtitles: bool,
    /// Explicit per-file overrides, persisted against file identity rather than path.
    pub forced_audio_index: Option<u32>,
    pub forced_subtitle_index: Option<u32>,
}

/// Choose video, audio, and subtitle tracks.
pub fn select(
    source: &MediaSource,
    prefs: &TrackPreferences,
    caps: &ClientCapabilities,
) -> Selection {
    let video = source.video.first().map(|v| v.index);
    let audio = select_audio(source, prefs, caps);
    let audio_language = audio
        .and_then(|i| source.audio.iter().find(|a| a.index == i))
        .map(|a| a.language.clone())
        .unwrap_or_default();
    let subtitle = select_subtitle(source, prefs, &audio_language);
    Selection { video, audio, subtitle }
}

fn language_rank(lang: &Language, chain: &[Language]) -> Option<usize> {
    chain.iter().position(|p| p.matches(lang))
}

fn select_audio(
    source: &MediaSource,
    prefs: &TrackPreferences,
    caps: &ClientCapabilities,
) -> Option<u32> {
    if let Some(forced) = prefs.forced_audio_index
        && source.audio.iter().any(|a| a.index == forced)
    {
        return Some(forced);
    }

    source.audio.iter().max_by_key(|a| audio_score(a, prefs, caps)).map(|a| a.index)
}

/// Higher is better. Ordered as a tuple so the comparison is lexicographic by priority, which keeps
/// the precedence explicit rather than hidden in weight arithmetic.
type AudioScore = (u8, u8, u8, u8, u8, u32, u32);

fn audio_score(a: &AudioStream, prefs: &TrackPreferences, caps: &ClientCapabilities) -> AudioScore {
    // Commentary and audio description are never auto-selected; they are deliberate choices.
    let not_special = u8::from(!a.flags.commentary && !a.flags.visual_impaired);

    let lang = match language_rank(&a.language, &prefs.audio_languages) {
        Some(pos) => {
            u8::try_from(prefs.audio_languages.len().saturating_sub(pos)).unwrap_or(u8::MAX)
        }
        None => 0,
    };
    let original = u8::from(prefs.prefer_original_audio && a.flags.original);

    // Fidelity, but only where the sink can use it: preferring a lossless 7.1 track on a stereo
    // laptop just forces a downmix of a bigger stream for no benefit.
    let sink_can_use = caps.audio_sink.can_passthrough(&a.codec)
        || a.layout.channels <= caps.audio_sink.max_pcm_channels;
    let fidelity = u8::from(a.codec.is_lossless() && sink_can_use);
    let deliverable_channels = u32::from(caps.audio_sink.deliverable_channels(a.layout));

    let default = u8::from(a.flags.default);
    let bitrate = u32::try_from(a.bitrate_bps.unwrap_or(0) / 1000).unwrap_or(u32::MAX);

    (not_special, lang, original, fidelity, default, deliverable_channels, bitrate)
}

fn select_subtitle(
    source: &MediaSource,
    prefs: &TrackPreferences,
    audio_language: &Language,
) -> Option<u32> {
    if let Some(forced) = prefs.forced_subtitle_index {
        return source.subtitles.iter().find(|s| s.index == forced).map(|s| s.index);
    }
    if source.subtitles.is_empty() {
        return None;
    }

    let audio_is_preferred = language_rank(audio_language, &prefs.subtitle_languages).is_some()
        || prefs.subtitle_languages.iter().any(|p| p.matches(audio_language));

    // The forced-subtitle rule: audio already in a language you read, so show only the translated
    // foreign dialogue. Anything else here is noise the user has to turn off manually.
    if audio_is_preferred && !prefs.always_show_subtitles {
        return source
            .subtitles
            .iter()
            .filter(|s| s.flags.forced && s.language.matches(audio_language))
            .max_by_key(|s| subtitle_score(s, prefs))
            .map(|s| s.index);
    }

    source
        .subtitles
        .iter()
        .filter(|s| language_rank(&s.language, &prefs.subtitle_languages).is_some())
        // A forced track alone is not a translation of the dialogue; when the audio is foreign the
        // user needs the full track.
        .filter(|s| !s.flags.forced)
        .max_by_key(|s| subtitle_score(s, prefs))
        .map(|s| s.index)
}

type SubtitleScore = (u8, u8, u8, u8);

fn subtitle_score(s: &SubtitleStream, prefs: &TrackPreferences) -> SubtitleScore {
    let lang = match language_rank(&s.language, &prefs.subtitle_languages) {
        Some(pos) => {
            u8::try_from(prefs.subtitle_languages.len().saturating_sub(pos)).unwrap_or(u8::MAX)
        }
        None => 0,
    };
    let sdh = u8::from(s.flags.hearing_impaired == prefs.prefer_sdh);
    // Text beats bitmap at equal language: it can be restyled, resized, and delivered out of band,
    // where PGS may end up burned in on a thin client.
    let text = u8::from(!s.codec.is_bitmap());
    let default = u8::from(s.flags.default);
    (lang, sdh, text, default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_model::{
        AudioCodec, ChannelLayout, Container, StreamFlags, SubtitleCodec, Transport,
    };

    fn prefs(audio: &[&str], subs: &[&str]) -> TrackPreferences {
        TrackPreferences {
            audio_languages: audio.iter().map(|s| Language::new(s)).collect(),
            subtitle_languages: subs.iter().map(|s| Language::new(s)).collect(),
            ..Default::default()
        }
    }

    fn audio_track(index: u32, lang: &str, codec: AudioCodec, ch: u8) -> AudioStream {
        AudioStream {
            index,
            codec,
            layout: ChannelLayout::new(ch),
            sample_rate: 48_000,
            bit_depth: Some(24),
            bitrate_bps: Some(640_000),
            language: Language::new(lang),
            title: None,
            flags: StreamFlags::enabled(),
            has_objects: false,
        }
    }

    fn sub_track(index: u32, lang: &str, codec: SubtitleCodec) -> SubtitleStream {
        SubtitleStream {
            index,
            codec,
            language: Language::new(lang),
            title: None,
            flags: StreamFlags::enabled(),
            external: false,
        }
    }

    fn source(audio: Vec<AudioStream>, subs: Vec<SubtitleStream>) -> MediaSource {
        let mut s = MediaSource::new(Container::Matroska, Transport::Local);
        s.audio = audio;
        s.subtitles = subs;
        s
    }

    #[test]
    fn forced_subtitle_selected_when_audio_is_already_preferred() {
        // The canonical case: English audio, English forced (alien dialogue) + English full.
        // Conformance vector `subtitles-forced-autoselect`.
        let mut forced = sub_track(10, "eng", SubtitleCodec::SubRip);
        forced.flags.forced = true;
        let full = sub_track(11, "eng", SubtitleCodec::SubRip);
        let src = source(vec![audio_track(1, "eng", AudioCodec::EAc3, 6)], vec![forced, full]);

        let sel = select(&src, &prefs(&["eng"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel.audio, Some(1));
        assert_eq!(sel.subtitle, Some(10), "forced track must win when audio is understood");
    }

    #[test]
    fn full_subtitle_selected_when_audio_is_foreign() {
        // Japanese audio, English subtitles: the user needs the whole dialogue, not just signs.
        let mut forced = sub_track(10, "eng", SubtitleCodec::SubRip);
        forced.flags.forced = true;
        let full = sub_track(11, "eng", SubtitleCodec::SubRip);
        let src = source(vec![audio_track(1, "jpn", AudioCodec::Flac, 2)], vec![forced, full]);

        let sel = select(&src, &prefs(&["jpn"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel.subtitle, Some(11), "full track must win when audio is foreign");
    }

    #[test]
    fn commentary_and_audio_description_are_never_auto_selected() {
        let mut commentary = audio_track(1, "eng", AudioCodec::TrueHd, 8);
        commentary.flags.commentary = true;
        commentary.flags.default = true; // even flagged default
        let mut described = audio_track(2, "eng", AudioCodec::TrueHd, 8);
        described.flags.visual_impaired = true;
        let normal = audio_track(3, "eng", AudioCodec::EAc3, 6);

        let src = source(vec![commentary, described, normal], vec![]);
        let sel = select(&src, &prefs(&["eng"], &[]), &ClientCapabilities::reference_native());
        assert_eq!(sel.audio, Some(3));
    }

    #[test]
    fn language_preference_beats_fidelity() {
        // A lossless dub must not beat a lossy track in the language the user asked for.
        let dub = audio_track(1, "eng", AudioCodec::TrueHd, 8);
        let wanted = audio_track(2, "jpn", AudioCodec::Aac, 2);
        let src = source(vec![dub, wanted], vec![]);
        let sel =
            select(&src, &prefs(&["jpn", "eng"], &[]), &ClientCapabilities::reference_native());
        assert_eq!(sel.audio, Some(2));
    }

    #[test]
    fn fidelity_preference_respects_what_the_sink_can_use() {
        // Same language, one lossless 7.1 and one lossy 5.1. On an AVR the lossless track wins.
        let lossless = audio_track(1, "eng", AudioCodec::TrueHd, 8);
        let lossy = audio_track(2, "eng", AudioCodec::EAc3, 6);
        let src = source(vec![lossless, lossy], vec![]);
        let p = prefs(&["eng"], &[]);
        assert_eq!(select(&src, &p, &ClientCapabilities::reference_native()).audio, Some(1));
    }

    #[test]
    fn text_subtitles_beat_bitmap_at_equal_language() {
        // PGS can end up burned in on a thin client; SRT never does. docs/12 §4.
        let pgs = sub_track(10, "eng", SubtitleCodec::Pgs);
        let srt = sub_track(11, "eng", SubtitleCodec::SubRip);
        let src = source(vec![audio_track(1, "jpn", AudioCodec::Flac, 2)], vec![pgs, srt]);
        let sel = select(&src, &prefs(&["jpn"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel.subtitle, Some(11));
    }

    #[test]
    fn explicit_user_override_wins_over_everything() {
        let src = source(
            vec![
                audio_track(1, "eng", AudioCodec::TrueHd, 8),
                audio_track(2, "fra", AudioCodec::Aac, 2),
            ],
            vec![sub_track(10, "eng", SubtitleCodec::SubRip)],
        );
        let p = TrackPreferences {
            forced_audio_index: Some(2),
            forced_subtitle_index: Some(10),
            ..prefs(&["eng"], &["eng"])
        };
        let sel = select(&src, &p, &ClientCapabilities::reference_native());
        assert_eq!(sel.audio, Some(2));
        assert_eq!(sel.subtitle, Some(10));
    }

    #[test]
    fn no_subtitle_when_nothing_matches_the_preference() {
        let src = source(
            vec![audio_track(1, "jpn", AudioCodec::Flac, 2)],
            vec![sub_track(10, "fra", SubtitleCodec::SubRip)],
        );
        let sel = select(&src, &prefs(&["jpn"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel.subtitle, None, "must not silently show an unwanted language");
    }

    #[test]
    fn unlabelled_tracks_do_not_satisfy_a_language_preference() {
        // A track with no language must never be treated as matching, or files with unlabelled
        // streams get arbitrary selections that look like bugs.
        let src = source(
            vec![audio_track(1, "und", AudioCodec::Aac, 2)],
            vec![sub_track(10, "", SubtitleCodec::SubRip)],
        );
        let sel = select(&src, &prefs(&["eng"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel.audio, Some(1), "still plays the only audio track");
        assert_eq!(sel.subtitle, None);
    }

    #[test]
    fn empty_source_selects_nothing_without_panicking() {
        let src = source(vec![], vec![]);
        let sel = select(&src, &prefs(&["eng"], &["eng"]), &ClientCapabilities::reference_native());
        assert_eq!(sel, Selection::default());
    }
}
