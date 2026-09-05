//! Building HLS media and master playlists (RFC 8216).
//!
//! **VOD only.** Every playlist built here ends with `#EXT-X-ENDLIST` and carries a fixed
//! `#EXT-X-MEDIA-SEQUENCE:0` -- live HLS (a playlist that grows while it is being served, sequence
//! numbers that advance, segments that age out) is real, separate future work this module does not
//! attempt.

/// One segment entry in a media playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Wall-clock duration, seconds. `ffmpeg`'s actual output may differ slightly (see
    /// [`crate::plan`]'s own doc comment on keyframe-aligned boundaries) -- this is whatever value
    /// the caller has, ideally read back from the real produced segment, not necessarily the planned
    /// one.
    pub duration_secs: f64,
    /// Relative to the playlist's own URL, matching every other HLS/DASH implementation's convention
    /// and letting the playlist move without its segment references breaking.
    pub uri: String,
}

/// A CMAF/fMP4 initialization segment, referenced by `#EXT-X-MAP` before the first media segment.
/// `None` for MPEG-TS segments, which need no separate init section.
#[derive(Debug, Clone, PartialEq)]
pub struct InitSegment {
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    pub segments: Vec<Segment>,
    pub init: Option<InitSegment>,
}

impl MediaPlaylist {
    /// `#EXT-X-TARGETDURATION` per RFC 8216 §4.3.3.1: the ceiling of the largest segment duration
    /// actually present, not the target this playlist's segments were planned against -- a real
    /// segment can run slightly over its planned length (keyframe alignment, see [`crate::plan`]),
    /// and a target lower than the true maximum is a spec violation many clients enforce strictly.
    fn target_duration_secs(&self) -> u32 {
        self.segments.iter().map(|s| s.duration_secs.ceil() as u32).max().unwrap_or(0)
    }

    /// Renders the full playlist text. Always well-formed even for zero segments (a real, if useless,
    /// empty VOD playlist), rather than a special case the caller has to avoid triggering.
    pub fn to_m3u8(&self) -> String {
        let mut out = String::new();
        out.push_str("#EXTM3U\n");
        out.push_str("#EXT-X-VERSION:7\n");
        out.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", self.target_duration_secs()));
        out.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
        out.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
        if let Some(init) = &self.init {
            out.push_str(&format!("#EXT-X-MAP:URI=\"{}\"\n", init.uri));
        }
        for seg in &self.segments {
            // Three decimal places: far finer than any real GOP-aligned boundary needs, but precise
            // enough that summing every `#EXTINF` back never visibly drifts from the true duration.
            out.push_str(&format!("#EXTINF:{:.3},\n", seg.duration_secs));
            out.push_str(&seg.uri);
            out.push('\n');
        }
        out.push_str("#EXT-X-ENDLIST\n");
        out
    }
}

/// One quality rendition in a master playlist -- almost always the *only* rendition in this crate's
/// current form, since building more than one means transcoding to a second bitrate/resolution, which
/// is exactly the transcode engine `lumen-exec`'s own module doc already names as separate, larger
/// future work. A single-rendition master playlist is still a real, valid, playable HLS asset (every
/// player accepts it) -- not a stub.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub bandwidth_bps: u64,
    /// RFC 6381 codec strings, comma-joined (e.g. `"avc1.640028,mp4a.40.2"`) -- left to the caller to
    /// supply, since deriving one correctly needs the exact profile/level/object-type triple this
    /// crate has no access to on its own.
    pub codecs: String,
    pub resolution: Option<(u32, u32)>,
    /// Relative to the master playlist's own URL, same convention as [`Segment::uri`].
    pub playlist_uri: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MasterPlaylist {
    pub renditions: Vec<Rendition>,
}

impl MasterPlaylist {
    pub fn to_m3u8(&self) -> String {
        let mut out = String::new();
        out.push_str("#EXTM3U\n");
        out.push_str("#EXT-X-VERSION:7\n");
        for r in &self.renditions {
            let mut attrs = format!("BANDWIDTH={},CODECS=\"{}\"", r.bandwidth_bps, r.codecs);
            if let Some((w, h)) = r.resolution {
                attrs.push_str(&format!(",RESOLUTION={w}x{h}"));
            }
            out.push_str(&format!("#EXT-X-STREAM-INF:{attrs}\n"));
            out.push_str(&r.playlist_uri);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ts_media_playlist_has_no_map_tag() {
        let playlist = MediaPlaylist {
            segments: vec![
                Segment { duration_secs: 6.0, uri: "seg_00000.ts".into() },
                Segment { duration_secs: 4.5, uri: "seg_00001.ts".into() },
            ],
            init: None,
        };
        let text = playlist.to_m3u8();
        assert!(text.starts_with("#EXTM3U\n"));
        assert!(!text.contains("EXT-X-MAP"));
        assert!(text.contains("#EXTINF:6.000,\nseg_00000.ts\n"));
        assert!(text.contains("#EXTINF:4.500,\nseg_00001.ts\n"));
        assert!(text.trim_end().ends_with("#EXT-X-ENDLIST"));
    }

    #[test]
    fn target_duration_is_the_ceiling_of_the_longest_segment_not_the_shortest() {
        let playlist = MediaPlaylist {
            segments: vec![
                Segment { duration_secs: 5.9, uri: "a.ts".into() },
                Segment { duration_secs: 6.2, uri: "b.ts".into() },
            ],
            init: None,
        };
        assert!(playlist.to_m3u8().contains("#EXT-X-TARGETDURATION:7\n"));
    }

    #[test]
    fn an_fmp4_playlist_references_its_init_segment_before_any_media_segment() {
        let playlist = MediaPlaylist {
            segments: vec![Segment { duration_secs: 6.0, uri: "seg_00000.m4s".into() }],
            init: Some(InitSegment { uri: "init.mp4".into() }),
        };
        let text = playlist.to_m3u8();
        let map_pos =
            text.find("EXT-X-MAP:URI=\"init.mp4\"").expect("must reference the init segment");
        let seg_pos = text.find("seg_00000.m4s").unwrap();
        assert!(
            map_pos < seg_pos,
            "the init segment must be declared before the first media segment"
        );
    }

    #[test]
    fn an_empty_playlist_is_still_well_formed() {
        let playlist = MediaPlaylist { segments: Vec::new(), init: None };
        let text = playlist.to_m3u8();
        assert!(text.contains("#EXT-X-TARGETDURATION:0\n"));
        assert!(text.trim_end().ends_with("#EXT-X-ENDLIST"));
    }

    #[test]
    fn a_master_playlist_lists_bandwidth_codecs_and_optional_resolution() {
        let master = MasterPlaylist {
            renditions: vec![Rendition {
                bandwidth_bps: 8_000_000,
                codecs: "avc1.640028,mp4a.40.2".into(),
                resolution: Some((1920, 1080)),
                playlist_uri: "stream.m3u8".into(),
            }],
        };
        let text = master.to_m3u8();
        assert!(text.contains("BANDWIDTH=8000000"));
        assert!(text.contains("CODECS=\"avc1.640028,mp4a.40.2\""));
        assert!(text.contains("RESOLUTION=1920x1080"));
        assert!(text.trim_end().ends_with("stream.m3u8"));
    }

    #[test]
    fn resolution_is_omitted_entirely_when_not_supplied() {
        let master = MasterPlaylist {
            renditions: vec![Rendition {
                bandwidth_bps: 128_000,
                codecs: "mp4a.40.2".into(),
                resolution: None,
                playlist_uri: "audio.m3u8".into(),
            }],
        };
        assert!(!master.to_m3u8().contains("RESOLUTION"));
    }
}
