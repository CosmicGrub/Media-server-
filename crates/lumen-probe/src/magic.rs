//! Content sniffing — `docs/12` §1 Rule 1: **the file extension is a hint, never a decision.**
//!
//! A `.mkv` that is really an MP4 plays. A `.mp4` that is really Matroska plays. A file named
//! `movie` plays. A file named `movie.txt` plays. The extension only reorders equally-scored
//! candidates; it can never introduce or eliminate one.

use lumen_model::Container;

/// How strongly the bytes identify a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Extension-only guess with no supporting bytes. Always tried last.
    Weak,
    /// A signature matched, but at a variable offset or with a short pattern that collides.
    Probable,
    /// An unambiguous magic sequence at a known offset.
    Certain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub container: Container,
    pub confidence: Confidence,
    /// Why this candidate was proposed, for the diagnostics bundle.
    pub evidence: &'static str,
}

/// Signatures anchored at offset 0.
const AT_ZERO: &[(&[u8], Container, &str)] = &[
    (&[0x1A, 0x45, 0xDF, 0xA3], Container::Matroska, "EBML header"),
    (b"RIFF", Container::Avi, "RIFF (AVI or WAV)"),
    (b"OggS", Container::Ogg, "Ogg page"),
    (b"FLV\x01", Container::Flv, "FLV header"),
    (&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11], Container::Asf, "ASF GUID"),
    (&[0x00, 0x00, 0x01, 0xBA], Container::MpegPs, "MPEG-PS pack header"),
    (&[0x00, 0x00, 0x01, 0xB3], Container::RawElementaryStream, "MPEG-2 sequence header"),
    (&[0x00, 0x00, 0x00, 0x01], Container::RawElementaryStream, "AnnexB start code"),
    (&[0x00, 0x00, 0x01], Container::RawElementaryStream, "AnnexB 3-byte start code"),
    (b"#EXTM3U", Container::Matroska, "HLS/M3U playlist"),
    (b"\x1FVP8", Container::WebM, "IVF/VP8"),
    (b"DKIF", Container::WebM, "IVF"),
    (b"\xFF\xD8\xFF", Container::RawElementaryStream, "JPEG"),
    (b"ID3", Container::RawElementaryStream, "ID3-tagged elementary stream"),
    (b"fLaC", Container::RawElementaryStream, "FLAC stream"),
    (b"\x0BwvpK", Container::RawElementaryStream, "WavPack"),
    (b"MAC ", Container::RawElementaryStream, "Monkey's Audio"),
    (b"DSD ", Container::RawElementaryStream, "DSF"),
    (b"FRM8", Container::RawElementaryStream, "DFF"),
];

/// ISOBMFF `ftyp` brands that identify a *specific* flavour worth distinguishing.
const FRAGMENTED_BRANDS: &[&[u8; 4]] =
    &[b"dash", b"cmfc", b"cmf2", b"msdh", b"msix", b"iso5", b"iso6"];

/// MPEG-TS packet size variants: 188 bare, 192 with a 4-byte M2TS timecode prefix, 204 with FEC.
const TS_PACKET_SIZES: [usize; 3] = [188, 192, 204];

/// Identify candidate containers from the leading bytes, best first.
///
/// Never returns an empty list: the last resort is always a raw elementary stream, because
/// guarantee **G2** forbids "unsupported format" (`docs/11` §1).
pub fn sniff(head: &[u8]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();

    // ISOBMFF is checked before the offset-0 table: `ftyp` sits at offset 4, and a QuickTime file
    // may legally have no `ftyp` at all (`docs/12` §3.5), so it needs bespoke handling.
    if let Some(c) = sniff_isobmff(head) {
        out.push(c);
    }

    for (sig, container, evidence) in AT_ZERO {
        if head.starts_with(sig) {
            let confidence =
                if sig.len() >= 4 { Confidence::Certain } else { Confidence::Probable };
            push_unique(&mut out, *container, confidence, evidence);
        }
    }

    if let Some(c) = sniff_mpegts(head) {
        push_unique(&mut out, c.container, c.confidence, c.evidence);
    }

    // RIFF needs its form type to distinguish AVI from WAV; both are handled, but the demuxer
    // ranking differs.
    if head.starts_with(b"RIFF") && head.len() >= 12 && &head[8..12] != b"AVI " {
        push_unique(
            &mut out,
            Container::RawElementaryStream,
            Confidence::Probable,
            "RIFF but not AVI (WAV or other)",
        );
    }

    // Guarantee G2: something is always attempted. A headerless elementary stream is the universal
    // last resort (recovery rung 5).
    push_unique(
        &mut out,
        Container::RawElementaryStream,
        Confidence::Weak,
        "fallback: probe as a headerless elementary stream",
    );

    out.sort_by(|a, b| b.confidence.cmp(&a.confidence));
    out
}

fn push_unique(
    out: &mut Vec<Candidate>,
    container: Container,
    confidence: Confidence,
    evidence: &'static str,
) {
    if out.iter().any(|c| c.container == container) {
        return;
    }
    out.push(Candidate { container, confidence, evidence });
}

fn sniff_isobmff(head: &[u8]) -> Option<Candidate> {
    if head.len() < 12 {
        return None;
    }
    // A top-level box whose type is one of the ISOBMFF set. `ftyp` is the normal case; `styp`
    // appears on a bare DASH/CMAF media segment; `moov`/`mdat`/`free`/`skip`/`wide` appear on
    // QuickTime files with no `ftyp` and on files whose header was stripped.
    let box_type = &head[4..8];
    let is_isobmff_box = matches!(
        box_type,
        b"ftyp" | b"styp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot" | b"moof"
    );
    if !is_isobmff_box {
        return None;
    }

    if box_type == b"ftyp" || box_type == b"styp" {
        let brand = &head[8..12];
        if brand == b"qt  " {
            return Some(Candidate {
                container: Container::Mp4,
                confidence: Confidence::Certain,
                evidence: "ISOBMFF ftyp, QuickTime brand",
            });
        }
        // Compatible brands live from offset 16 onward, but a truncated header may not reach it —
        // `head` is only guaranteed to be 12 bytes here, so this must not index blindly.
        let compatible = head.get(16..).unwrap_or(&[]);
        let fragmented = FRAGMENTED_BRANDS.iter().any(|b| brand == b.as_slice())
            || box_type == b"styp"
            || compatible
                .chunks_exact(4)
                .take(8)
                .any(|c| FRAGMENTED_BRANDS.iter().any(|b| c == b.as_slice()));
        return Some(Candidate {
            container: if fragmented { Container::FragmentedMp4 } else { Container::Mp4 },
            confidence: Confidence::Certain,
            evidence: if fragmented {
                "ISOBMFF ftyp/styp, fragmented brand"
            } else {
                "ISOBMFF ftyp"
            },
        });
    }

    Some(Candidate {
        container: Container::Mp4,
        confidence: Confidence::Probable,
        evidence: "ISOBMFF top-level box, no ftyp",
    })
}

/// MPEG-TS has no magic sequence — it is identified by the 0x47 sync byte recurring at a fixed
/// stride. Real captures start mid-stream, so the search is offset-tolerant.
fn sniff_mpegts(head: &[u8]) -> Option<Candidate> {
    for size in TS_PACKET_SIZES {
        for start in 0..size.min(head.len()) {
            if head.get(start) != Some(&0x47) {
                continue;
            }
            let syncs = (0..8)
                .filter_map(|i| head.get(start + i * size))
                .take_while(|b| **b == 0x47)
                .count();
            // Four consecutive syncs at a fixed stride is far beyond coincidence.
            if syncs >= 4 {
                return Some(Candidate {
                    container: Container::MpegTs,
                    confidence: if start == 0 { Confidence::Certain } else { Confidence::Probable },
                    evidence: "MPEG-TS sync bytes at a fixed stride",
                });
            }
        }
    }
    None
}

/// Reorder equally-confident candidates so the extension's implication is tried first.
///
/// This is the *only* influence an extension has. It cannot add a candidate, remove one, or promote
/// a weak match above a strong one — otherwise a mislabelled file becomes unplayable, which is
/// exactly the failure Rule 1 exists to prevent.
pub fn rank_with_extension(candidates: &mut [Candidate], extension: Option<&str>) {
    let Some(ext) = extension else { return };
    let implied = container_for_extension(&ext.to_ascii_lowercase());
    let Some(implied) = implied else { return };
    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| (b.container == implied).cmp(&(a.container == implied)))
    });
}

/// What an extension *suggests*. Advisory only.
pub fn container_for_extension(ext: &str) -> Option<Container> {
    Some(match ext.trim_start_matches('.') {
        "mkv" | "mka" | "mks" | "mk3d" => Container::Matroska,
        "webm" => Container::WebM,
        "mp4" | "m4v" | "m4a" | "m4b" | "m4r" | "mp4v" | "mov" | "qt" | "3gp" | "3g2" => {
            Container::Mp4
        }
        "m4s" | "cmfv" | "cmfa" => Container::FragmentedMp4,
        "ts" | "m2ts" | "mts" | "m2t" | "tp" | "tsv" => Container::MpegTs,
        "mpg" | "mpeg" | "vob" | "evo" | "mod" | "tod" => Container::MpegPs,
        "avi" | "divx" => Container::Avi,
        "wmv" | "wma" | "asf" => Container::Asf,
        "flv" | "f4v" => Container::Flv,
        "ogg" | "ogv" | "oga" | "opus" | "spx" | "ogm" => Container::Ogg,
        "iso" => Container::DiscStructure,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(head: &[u8]) -> Container {
        sniff(head)[0].container
    }

    #[test]
    fn matroska_identified_by_ebml_magic() {
        let mut head = vec![0x1A, 0x45, 0xDF, 0xA3];
        head.extend_from_slice(&[0u8; 64]);
        assert_eq!(best(&head), Container::Matroska);
        assert_eq!(sniff(&head)[0].confidence, Confidence::Certain);
    }

    #[test]
    fn isobmff_identified_by_ftyp_at_offset_four() {
        let mut head = vec![0, 0, 0, 0x18];
        head.extend_from_slice(b"ftypisom");
        head.extend_from_slice(b"\0\0\x02\0isomiso2avc1mp41");
        assert_eq!(best(&head), Container::Mp4);
    }

    #[test]
    fn fragmented_mp4_distinguished_from_plain_mp4() {
        let mut head = vec![0, 0, 0, 0x18];
        head.extend_from_slice(b"ftypdash");
        head.extend_from_slice(b"\0\0\0\0dashiso6mp41");
        assert_eq!(best(&head), Container::FragmentedMp4);

        // A bare DASH media segment starts with `styp`, not `ftyp`.
        let mut seg = vec![0, 0, 0, 0x18];
        seg.extend_from_slice(b"stypmsdh");
        seg.extend_from_slice(&[0u8; 16]);
        assert_eq!(best(&seg), Container::FragmentedMp4);
    }

    #[test]
    fn quicktime_without_ftyp_is_still_recognised() {
        // docs/12 §3.5: many QuickTime files have no ftyp. Refusing them is a compatibility failure.
        let mut head = vec![0, 0, 0x10, 0x00];
        head.extend_from_slice(b"moov");
        head.extend_from_slice(&[0u8; 32]);
        assert_eq!(best(&head), Container::Mp4);
    }

    #[test]
    fn mpegts_recognised_at_all_three_packet_sizes() {
        for size in TS_PACKET_SIZES {
            let mut head = vec![0u8; size * 8];
            for i in 0..8 {
                head[i * size] = 0x47;
            }
            assert_eq!(best(&head), Container::MpegTs, "packet size {size}");
        }
    }

    #[test]
    fn mpegts_recognised_when_the_capture_starts_mid_packet() {
        // Partial captures from a tuner routinely begin mid-packet.
        let mut head = vec![0xAAu8; 188 * 9];
        for i in 0..8 {
            head[57 + i * 188] = 0x47;
        }
        let c = &sniff(&head)[0];
        assert_eq!(c.container, Container::MpegTs);
        assert_eq!(c.confidence, Confidence::Probable, "offset match is probable, not certain");
    }

    #[test]
    fn a_single_stray_sync_byte_is_not_mpegts() {
        let mut head = vec![0u8; 2048];
        head[100] = 0x47;
        assert_ne!(best(&head), Container::MpegTs);
    }

    #[test]
    fn a_truncated_ftyp_header_does_not_panic() {
        // Found by `truncation_at_any_offset_never_panics`: the compatible-brand list starts at
        // offset 16, but a 12-byte header is already enough to identify ISOBMFF. Indexing blindly
        // crashed the probe on any 12-15 byte file — a denial of service on a watched folder.
        for len in 8..24 {
            let mut head = vec![0, 0, 0, 0x18];
            head.extend_from_slice(b"ftypisom");
            head.extend_from_slice(b"\0\0\x02\0dashiso6");
            head.truncate(len);
            let _ = sniff(&head);
        }
    }

    #[test]
    fn sniff_never_returns_empty_even_for_garbage() {
        // Guarantee G2: the player never says "unsupported format".
        for head in [vec![], vec![0u8; 1], vec![0xFFu8; 512], b"this is a text file".to_vec()] {
            let got = sniff(&head);
            assert!(!got.is_empty(), "empty candidate list for {} bytes", head.len());
            assert_eq!(got.last().unwrap().container, Container::RawElementaryStream);
        }
    }

    #[test]
    fn extension_cannot_override_content() {
        // The headline case from docs/12 Rule 1: an MP4 named .mkv must play as an MP4.
        let mut head = vec![0, 0, 0, 0x18];
        head.extend_from_slice(b"ftypisom");
        head.extend_from_slice(&[0u8; 16]);
        let mut got = sniff(&head);
        rank_with_extension(&mut got, Some("mkv"));
        assert_eq!(got[0].container, Container::Mp4, "content must win over extension");
    }

    #[test]
    fn extension_only_reorders_within_a_confidence_level() {
        // Two Probable candidates: the extension picks between them, and cannot promote a Weak one.
        let mut got = vec![
            Candidate {
                container: Container::Avi,
                confidence: Confidence::Probable,
                evidence: "a",
            },
            Candidate {
                container: Container::Asf,
                confidence: Confidence::Probable,
                evidence: "b",
            },
            Candidate {
                container: Container::Matroska,
                confidence: Confidence::Certain,
                evidence: "c",
            },
        ];
        rank_with_extension(&mut got, Some("wmv"));
        assert_eq!(got[0].container, Container::Matroska, "Certain still leads");
        assert_eq!(got[1].container, Container::Asf, "extension reorders the Probable pair");
    }

    #[test]
    fn unknown_extension_is_harmless() {
        let mut head = vec![0x1A, 0x45, 0xDF, 0xA3];
        head.extend_from_slice(&[0u8; 32]);
        let mut got = sniff(&head);
        let before = got.clone();
        rank_with_extension(&mut got, Some("xyzzy"));
        assert_eq!(got, before);
        rank_with_extension(&mut got, None);
        assert_eq!(got, before);
    }

    #[test]
    fn no_extension_at_all_still_probes() {
        assert_eq!(container_for_extension("nonsense"), None);
        let mut head = vec![0x1A, 0x45, 0xDF, 0xA3];
        head.extend_from_slice(&[0u8; 32]);
        assert_eq!(best(&head), Container::Matroska);
    }
}
