//! A concrete playable source: one file, disc structure, or stream.

use crate::container::Container;
use crate::stream::{AudioStream, SubtitleStream, VideoStream};

/// How the bytes reach the player. Drives read-ahead sizing (`docs/11` §6.5) and which recovery
/// strategies are affordable — range-requesting an MP4 tail is cheap over HTTP and free locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transport {
    Local,
    /// SMB, NFS, WebDAV, SFTP — LAN-class latency and bandwidth.
    NetworkShare,
    /// HTTP(S) with range support.
    Http,
    /// High-latency or lossy: WAN, cellular, or a relay.
    RemoteHighLatency,
}

impl Transport {
    /// Target seconds of buffered content, from `docs/11` §6.5. Multiplied by measured bitrate to
    /// size read-ahead, which is what makes 100 Mbps remuxes play without stutter.
    pub fn readahead_target_seconds(self) -> u32 {
        match self {
            Self::Local => 5,
            Self::NetworkShare | Self::Http => 20,
            Self::RemoteHighLatency => 45,
        }
    }
}

/// How much the source had to be reconstructed to open, and whether content was lost doing it.
///
/// This distinction matters for tier assignment (`docs/11` §1.1). A Matroska file with no `Cues`
/// needs index reconstruction but every byte of content is present, so it can still reach T1. A
/// truncated file, or one whose `moov` was rebuilt by scanning `mdat`, has uncertain or missing
/// content and is T4 however cleanly it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Integrity {
    #[default]
    Intact,
    /// Recovery was needed to open, but the content is complete: missing/broken index, damaged
    /// header, wrong extension, garbage between clusters.
    RecoveredComplete,
    /// Recovery produced a playable but incomplete or uncertain picture: truncation, corrupt
    /// packets, reconstructed sample tables, dropped tracks.
    RecoveredLossy,
}

impl Integrity {
    pub fn is_recovered(self) -> bool {
        !matches!(self, Self::Intact)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSource {
    pub container: Container,
    pub transport: Transport,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleStream>,
    pub duration_ns: Option<i64>,
    /// Overall bitrate when known. `None` on live streams and on files whose duration could not be
    /// established (a T4 recovery outcome), where read-ahead must be sized from measurement instead.
    pub bitrate_bps: Option<u64>,
    /// Outcome of the recovery ladder (`docs/12` §5).
    pub integrity: Integrity,
}

impl MediaSource {
    pub fn new(container: Container, transport: Transport) -> Self {
        Self {
            container,
            transport,
            video: Vec::new(),
            audio: Vec::new(),
            subtitles: Vec::new(),
            duration_ns: None,
            bitrate_bps: None,
            integrity: Integrity::Intact,
        }
    }

    /// Read-ahead in bytes, clamped per `docs/11` §6.5. `measured_bps` overrides the container's
    /// declared bitrate because declared values are frequently absent or wrong on remuxes.
    pub fn readahead_bytes(&self, measured_bps: Option<u64>, cap_bytes: u64) -> u64 {
        const MIN: u64 = 8 * 1024 * 1024;
        let bps = measured_bps.or(self.bitrate_bps).unwrap_or(0);
        let target = u64::from(self.transport.readahead_target_seconds());
        let want = bps / 8 * target;
        want.clamp(MIN, cap_bytes.max(MIN))
    }

    /// A remux-class source: very high bitrate with at least one lossless audio track. These are the
    /// files the product exists to play correctly, and they get the most conservative treatment.
    pub fn is_remux_class(&self) -> bool {
        let high_bitrate = self.bitrate_bps.is_some_and(|b| b >= 40_000_000);
        let lossless_audio = self.audio.iter().any(|a| a.codec.is_lossless());
        high_bitrate && lossless_audio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Language;
    use crate::codec::AudioCodec;
    use crate::stream::{ChannelLayout, StreamFlags};

    fn audio(codec: AudioCodec) -> AudioStream {
        AudioStream {
            index: 1,
            codec,
            layout: ChannelLayout::SURROUND_7_1,
            sample_rate: 48_000,
            bit_depth: Some(24),
            bitrate_bps: None,
            language: Language::new("eng"),
            title: None,
            flags: StreamFlags::enabled(),
            has_objects: false,
        }
    }

    #[test]
    fn readahead_scales_with_bitrate_and_transport() {
        let mut s = MediaSource::new(Container::Matroska, Transport::NetworkShare);
        s.bitrate_bps = Some(92_000_000); // a UHD remux
        let cap = 1024 * 1024 * 1024;
        // 92 Mbps / 8 * 20 s = 230 MB
        assert_eq!(s.readahead_bytes(None, cap), 92_000_000 / 8 * 20);

        // Same file locally needs far less.
        let local = MediaSource { transport: Transport::Local, ..s.clone() };
        assert!(local.readahead_bytes(None, cap) < s.readahead_bytes(None, cap));
    }

    #[test]
    fn readahead_respects_floor_and_cap() {
        let mut s = MediaSource::new(Container::Mp4, Transport::Http);
        s.bitrate_bps = Some(100_000); // a tiny 3GP clip
        assert_eq!(s.readahead_bytes(None, 1 << 30), 8 * 1024 * 1024, "floor applies");

        s.bitrate_bps = Some(1_500_000_000); // uncompressed mastering source
        assert_eq!(s.readahead_bytes(None, 64 * 1024 * 1024), 64 * 1024 * 1024, "cap applies");
    }

    #[test]
    fn readahead_survives_unknown_bitrate() {
        // Live streams and recovered files have no declared bitrate; must not panic or return 0.
        let s = MediaSource::new(Container::Matroska, Transport::Http);
        assert_eq!(s.readahead_bytes(None, 1 << 30), 8 * 1024 * 1024);
    }

    #[test]
    fn measured_bitrate_overrides_declared() {
        let mut s = MediaSource::new(Container::Matroska, Transport::Local);
        s.bitrate_bps = Some(1_000_000); // declared, wrong
        let measured = 80_000_000;
        assert_eq!(s.readahead_bytes(Some(measured), 1 << 30), measured / 8 * 5);
    }

    #[test]
    fn remux_class_needs_both_bitrate_and_lossless_audio() {
        let mut s = MediaSource::new(Container::Matroska, Transport::NetworkShare);
        s.bitrate_bps = Some(92_000_000);
        s.audio.push(audio(AudioCodec::TrueHd));
        assert!(s.is_remux_class());

        s.audio = vec![audio(AudioCodec::EAc3)];
        assert!(!s.is_remux_class(), "high bitrate alone is a big encode, not a remux");

        s.audio = vec![audio(AudioCodec::TrueHd)];
        s.bitrate_bps = Some(6_000_000);
        assert!(!s.is_remux_class(), "lossless audio alone is a music file or a light encode");
    }
}
