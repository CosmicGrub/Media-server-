//! Minimal ISOBMFF (MP4/MOV) structural reader — `docs/12` §3.
//!
//! Answers the structural questions that decide how an MP4 must be opened. Each maps to a documented
//! failure in other players:
//!
//! | Question | Why it matters |
//! |---|---|
//! | Is `moov` before `mdat`? | Non-faststart over HTTP needs a **tail range request**; downloading the whole file to start is the classic "takes 10 minutes to begin streaming" bug (§3.1) |
//! | `mdat` with `size == 0`? | Legal and common on streaming-written files; extends to EOF (§3.1) |
//! | Fragmented (`moof`/`sidx`/`styp`)? | Different seek strategy entirely (§3.4) |
//! | Encrypted (`pssh`/`sinf`/`senc`)? | Must produce a named scheme, never garbage decode (§3.6) |
//! | Edit lists present? | The #1 A/V-sync bug source in MP4 (§3.2) |
//! | Is `moov` missing entirely? | Recovery rung 4: reconstruct sample tables by scanning `mdat` — irreplaceable phone/camera files (§3.7) |
//!
//! Bounded and total: malformed input yields partial findings, never a panic.

/// Content-protection scheme, identified so the message can name it (`docs/11` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionScheme {
    /// ISO Common Encryption, AES-CTR full-sample.
    Cenc,
    /// CENC AES-CBC full-sample.
    Cbc1,
    /// CENC AES-CTR pattern.
    Cens,
    /// CENC AES-CBC pattern — the scheme FairPlay Streaming uses.
    Cbcs,
    /// PIFF, signalled by a `uuid` box.
    Piff,
    /// Protection boxes present but the scheme could not be identified.
    Unknown,
}

impl EncryptionScheme {
    fn from_fourcc(cc: &[u8]) -> Self {
        match cc {
            b"cenc" => Self::Cenc,
            b"cbc1" => Self::Cbc1,
            b"cens" => Self::Cens,
            b"cbcs" => Self::Cbcs,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cenc => "cenc",
            Self::Cbc1 => "cbc1",
            Self::Cens => "cens",
            Self::Cbcs => "cbcs",
            Self::Piff => "PIFF",
            Self::Unknown => "unknown",
        }
    }
}

/// Where the metadata sits relative to the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MoovPlacement {
    /// Faststart: `moov` precedes `mdat`. Streams immediately.
    BeforeMdat,
    /// `mdat` first. Locally this is a seek; over HTTP it needs a tail range request (§3.1).
    AfterMdat,
    /// No `moov` at all — an interrupted recording. Recovery rung 4 reconstructs the sample tables
    /// by scanning `mdat` for codec sync patterns (§3.7). Also the correct default for an empty
    /// buffer: nothing has been found yet.
    #[default]
    Absent,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IsobmffLayout {
    /// `ftyp` major brand plus compatible brands, in file order. Empty when `ftyp` is absent, which
    /// is legal for QuickTime (§3.5).
    pub brands: Vec<String>,
    pub has_ftyp: bool,
    pub moov: MoovPlacement,
    /// Fragmented: `moof` present, or a `styp`-led bare media segment.
    pub fragmented: bool,
    pub has_sidx: bool,
    pub has_mfra: bool,
    /// A `mdat` declared `size == 0`, meaning "to end of file".
    pub mdat_extends_to_eof: bool,
    pub encryption: Option<EncryptionScheme>,
    /// `elst` present on at least one track. Must be honoured or audio drifts (§3.2).
    pub has_edit_list: bool,
    /// More than one `stsd` entry: resolution or codec changes mid-file, and the renderer must
    /// reconfigure without stopping (§3.3).
    pub multiple_sample_descriptions: bool,
    pub track_count: usize,
    /// Chapter mechanisms found. Different tools write different ones and all must be readable (§3.5).
    pub has_nero_chapters: bool,
    pub has_chapter_track_ref: bool,
    /// 64-bit chunk offsets: the file is or was over 4 GB.
    pub has_co64: bool,
    /// A second top-level `ftyp`/`moov` pair — two files concatenated (§3.7).
    pub concatenated: bool,
    /// The buffer ran out mid-structure. Findings so far remain valid.
    pub truncated: bool,
}

impl IsobmffLayout {
    /// Over HTTP, a non-faststart file needs the tail fetched before playback can begin. Reading
    /// forward until `moov` appears is what makes other players take minutes to start.
    pub fn needs_tail_fetch(&self) -> bool {
        self.moov == MoovPlacement::AfterMdat
    }

    /// No `moov`: sample tables must be reconstructed by scanning `mdat` (recovery rung 4). These are
    /// interrupted recordings — irreplaceable user files, and the highest-value recovery case.
    pub fn needs_moov_reconstruction(&self) -> bool {
        self.moov == MoovPlacement::Absent && !self.fragmented
    }

    pub fn is_protected(&self) -> bool {
        self.encryption.is_some()
    }
}

/// PIFF sample-encryption box UUID.
const PIFF_SENC_UUID: [u8; 16] = [
    0xA2, 0x39, 0x4F, 0x52, 0x5A, 0x9B, 0x4F, 0x14, 0xA2, 0x44, 0x6C, 0x42, 0x7C, 0x64, 0x8D, 0xF4,
];

const MAX_DEPTH: u8 = 10;

/// Boxes worth descending into. Everything else is skipped by size — §1 Rule 2.
fn is_container_box(t: &[u8; 4]) -> bool {
    matches!(
        t,
        b"moov"
            | b"trak"
            | b"mdia"
            | b"minf"
            | b"stbl"
            | b"edts"
            | b"moof"
            | b"traf"
            | b"mvex"
            | b"udta"
            | b"meta"
            | b"ilst"
            | b"sinf"
            | b"schi"
            | b"stsd"
            | b"wave"
            | b"tref"
    )
}

/// Analyse the structure of an ISOBMFF buffer.
///
/// Returns `None` only when the buffer does not look like ISOBMFF at all. A truncated or damaged
/// file yields partial findings with [`IsobmffLayout::truncated`] set.
pub fn analyze(buf: &[u8]) -> Option<IsobmffLayout> {
    if buf.len() < 8 {
        return None;
    }
    let first_type: [u8; 4] = buf[4..8].try_into().ok()?;
    let plausible = matches!(
        &first_type,
        b"ftyp" | b"styp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot" | b"moof"
    );
    if !plausible {
        return None;
    }

    let mut layout = IsobmffLayout::default();
    let mut saw_mdat = false;
    walk(buf, &mut layout, &mut saw_mdat, 0, true);
    Some(layout)
}

/// Walk boxes at one level. `top_level` enables order-sensitive findings that only make sense there.
fn walk(buf: &[u8], layout: &mut IsobmffLayout, saw_mdat: &mut bool, depth: u8, top_level: bool) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut pos = 0usize;

    while pos + 8 <= buf.len() {
        let size32 = u32::from_be_bytes(buf[pos..pos + 4].try_into().expect("4 bytes"));
        let box_type: [u8; 4] = buf[pos + 4..pos + 8].try_into().expect("4 bytes");
        let mut header = 8usize;

        // `size == 1` means a 64-bit `largesize` follows; `size == 0` means the box runs to EOF —
        // both legal, and both required for files over 4 GB and streaming-written output (§3.1).
        let body_len = match size32 {
            1 => {
                if pos + 16 > buf.len() {
                    layout.truncated = true;
                    return;
                }
                let large = u64::from_be_bytes(buf[pos + 8..pos + 16].try_into().expect("8 bytes"));
                header = 16;
                match usize::try_from(large.saturating_sub(16)) {
                    Ok(n) => n,
                    Err(_) => {
                        layout.truncated = true;
                        return;
                    }
                }
            }
            0 => {
                if &box_type == b"mdat" {
                    layout.mdat_extends_to_eof = true;
                }
                buf.len() - pos - header
            }
            n if (n as usize) < header => {
                // A size smaller than its own header is corrupt. Resync rather than loop forever.
                layout.truncated = true;
                return;
            }
            n => (n as usize) - header,
        };

        let body_start = pos + header;
        let body_end = (body_start + body_len).min(buf.len());
        if body_start + body_len > buf.len() {
            layout.truncated = true;
        }
        let body = &buf[body_start.min(buf.len())..body_end];

        match &box_type {
            b"ftyp" | b"styp" => {
                if layout.has_ftyp {
                    layout.concatenated = true;
                }
                layout.has_ftyp = true;
                if &box_type == b"styp" {
                    layout.fragmented = true;
                }
                for chunk in body.chunks(4).take(16) {
                    if chunk.len() == 4 {
                        layout.brands.push(String::from_utf8_lossy(chunk).trim().to_string());
                    }
                }
                if layout.brands.iter().any(|b| {
                    matches!(
                        b.as_str(),
                        "dash" | "cmfc" | "cmf2" | "msdh" | "msix" | "iso5" | "iso6"
                    )
                }) {
                    layout.fragmented = true;
                }
            }
            b"mdat" => *saw_mdat = true,
            b"moov" if top_level => {
                if layout.moov != MoovPlacement::Absent {
                    layout.concatenated = true;
                }
                layout.moov =
                    if *saw_mdat { MoovPlacement::AfterMdat } else { MoovPlacement::BeforeMdat };
                walk(body, layout, saw_mdat, depth + 1, false);
            }
            b"moof" => {
                layout.fragmented = true;
                walk(body, layout, saw_mdat, depth + 1, false);
            }
            b"sidx" | b"ssix" => layout.has_sidx = true,
            b"mfra" => layout.has_mfra = true,
            b"trak" => {
                layout.track_count += 1;
                walk(body, layout, saw_mdat, depth + 1, false);
            }
            b"elst" => layout.has_edit_list = true,
            b"co64" => layout.has_co64 = true,
            b"chpl" => layout.has_nero_chapters = true,
            b"chap" => layout.has_chapter_track_ref = true,
            b"pssh" => {
                layout.encryption = layout.encryption.or(Some(EncryptionScheme::Unknown));
            }
            b"schm" => {
                // `schm` is a full box: 4 bytes version/flags, then the scheme type.
                if body.len() >= 8 {
                    layout.encryption = Some(EncryptionScheme::from_fourcc(&body[4..8]));
                }
            }
            b"senc" | b"saiz" | b"saio" | b"sinf" | b"tenc" => {
                layout.encryption = layout.encryption.or(Some(EncryptionScheme::Unknown));
                if is_container_box(&box_type) {
                    walk(body, layout, saw_mdat, depth + 1, false);
                }
            }
            b"uuid" => {
                if body.len() >= 16 && body[..16] == PIFF_SENC_UUID {
                    layout.encryption = Some(EncryptionScheme::Piff);
                }
            }
            b"stsd" => {
                // Entry count is a u32 after 4 bytes of version/flags. More than one entry means the
                // stream's parameters change mid-file (§3.3).
                if body.len() >= 8 {
                    let count = u32::from_be_bytes(body[4..8].try_into().expect("4 bytes"));
                    if count > 1 {
                        layout.multiple_sample_descriptions = true;
                    }
                }
            }
            t if is_container_box(t) => walk(body, layout, saw_mdat, depth + 1, false),
            _ => {}
        }

        let next = body_end.max(pos + header);
        if next <= pos {
            return; // no forward progress
        }
        pos = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(t: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(body);
        v
    }

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut body = major.to_vec();
        body.extend_from_slice(&[0, 0, 2, 0]); // minor version
        for c in compatible {
            body.extend_from_slice(*c);
        }
        bx(b"ftyp", &body)
    }

    fn moov(inner: &[u8]) -> Vec<u8> {
        bx(b"moov", inner)
    }

    fn trak(inner: &[u8]) -> Vec<u8> {
        bx(b"trak", inner)
    }

    fn stsd(entry_count: u32) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        body.extend_from_slice(&entry_count.to_be_bytes());
        bx(b"stsd", &body)
    }

    #[test]
    fn faststart_layout_needs_no_tail_fetch() {
        let mut f = ftyp(b"isom", &[b"iso2", b"avc1", b"mp41"]);
        f.extend(moov(&trak(&[])));
        f.extend(bx(b"mdat", &[0u8; 64]));
        let l = analyze(&f).expect("ISOBMFF");
        assert_eq!(l.moov, MoovPlacement::BeforeMdat);
        assert!(!l.needs_tail_fetch());
        assert_eq!(l.track_count, 1);
        assert!(l.brands.contains(&"isom".to_string()));
    }

    #[test]
    fn non_faststart_layout_is_detected_so_http_can_range_fetch_the_tail() {
        // docs/12 §3.1: the classic "takes minutes to start streaming" bug. Detecting this is what
        // lets an HTTP client fetch the tail instead of the whole file.
        let mut f = ftyp(b"isom", &[]);
        f.extend(bx(b"mdat", &[0u8; 128]));
        f.extend(moov(&trak(&[])));
        let l = analyze(&f).unwrap();
        assert_eq!(l.moov, MoovPlacement::AfterMdat);
        assert!(l.needs_tail_fetch());
    }

    #[test]
    fn missing_moov_triggers_reconstruction_not_failure() {
        // docs/12 §3.7: an interrupted recording. These are irreplaceable user files and the
        // highest-value recovery case in the whole ladder.
        let mut f = ftyp(b"isom", &[]);
        f.extend(bx(b"mdat", &[0u8; 256]));
        let l = analyze(&f).unwrap();
        assert_eq!(l.moov, MoovPlacement::Absent);
        assert!(l.needs_moov_reconstruction());
    }

    #[test]
    fn mdat_with_size_zero_extends_to_eof() {
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&[]));
        f.extend_from_slice(&0u32.to_be_bytes()); // size == 0
        f.extend_from_slice(b"mdat");
        f.extend_from_slice(&[0u8; 100]);
        let l = analyze(&f).unwrap();
        assert!(l.mdat_extends_to_eof);
        assert_eq!(l.moov, MoovPlacement::BeforeMdat);
    }

    #[test]
    fn sixty_four_bit_largesize_is_handled() {
        let mut f = ftyp(b"isom", &[]);
        let payload = [0u8; 32];
        f.extend_from_slice(&1u32.to_be_bytes()); // size == 1 -> largesize follows
        f.extend_from_slice(b"mdat");
        f.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
        f.extend_from_slice(&payload);
        f.extend(moov(&[]));
        let l = analyze(&f).unwrap();
        assert_eq!(l.moov, MoovPlacement::AfterMdat, "largesize box was traversed correctly");
    }

    #[test]
    fn quicktime_without_ftyp_still_parses() {
        // docs/12 §3.5: `ftyp` is absent on many QuickTime files. Refusing them is a compat failure.
        let mut f = moov(&trak(&[]));
        f.extend(bx(b"mdat", &[0u8; 16]));
        let l = analyze(&f).unwrap();
        assert!(!l.has_ftyp);
        assert_eq!(l.moov, MoovPlacement::BeforeMdat);
        assert_eq!(l.track_count, 1);
    }

    #[test]
    fn fragmented_detected_from_moof_from_styp_and_from_brand() {
        let mut by_moof = ftyp(b"isom", &[]);
        by_moof.extend(bx(b"moof", &bx(b"traf", &[])));
        assert!(analyze(&by_moof).unwrap().fragmented);

        let mut by_styp = bx(b"styp", b"msdh\0\0\0\0msix");
        by_styp.extend(bx(b"moof", &[]));
        assert!(analyze(&by_styp).unwrap().fragmented);

        let by_brand = ftyp(b"iso6", &[b"dash"]);
        assert!(analyze(&by_brand).unwrap().fragmented);
    }

    #[test]
    fn fragmented_file_without_moov_does_not_ask_for_reconstruction() {
        // A bare media segment legitimately has no moov; reconstructing would be wrong.
        let mut f = bx(b"styp", b"msdh\0\0\0\0");
        f.extend(bx(b"moof", &[]));
        f.extend(bx(b"mdat", &[0u8; 32]));
        let l = analyze(&f).unwrap();
        assert_eq!(l.moov, MoovPlacement::Absent);
        assert!(!l.needs_moov_reconstruction(), "fragmented segments have no moov by design");
    }

    #[test]
    fn sidx_and_mfra_are_found_for_seeking() {
        let mut f = ftyp(b"iso6", &[b"dash"]);
        f.extend(bx(b"sidx", &[0u8; 12]));
        f.extend(bx(b"moof", &[]));
        f.extend(bx(b"mfra", &[0u8; 8]));
        let l = analyze(&f).unwrap();
        assert!(l.has_sidx && l.has_mfra);
    }

    #[test]
    fn cenc_scheme_is_named_not_left_unknown() {
        // docs/12 §3.6: the message must identify the scheme. "It didn't work" is not acceptable.
        let mut schm = vec![0, 0, 0, 0];
        schm.extend_from_slice(b"cenc");
        schm.extend_from_slice(&[0, 1, 0, 0]);
        let sinf = bx(b"sinf", &bx(b"schm", &schm));
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&sinf)));
        let l = analyze(&f).unwrap();
        assert_eq!(l.encryption, Some(EncryptionScheme::Cenc));
        assert!(l.is_protected());
        assert_eq!(l.encryption.unwrap().as_str(), "cenc");
    }

    #[test]
    fn cbcs_fairplay_scheme_is_distinguished() {
        let mut schm = vec![0, 0, 0, 0];
        schm.extend_from_slice(b"cbcs");
        schm.extend_from_slice(&[0, 1, 0, 0]);
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&bx(b"sinf", &bx(b"schm", &schm)))));
        assert_eq!(analyze(&f).unwrap().encryption, Some(EncryptionScheme::Cbcs));
    }

    #[test]
    fn pssh_alone_marks_protection_even_without_a_scheme_box() {
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&bx(b"pssh", &[0u8; 20])));
        assert_eq!(analyze(&f).unwrap().encryption, Some(EncryptionScheme::Unknown));
    }

    #[test]
    fn piff_uuid_box_is_recognised() {
        let mut body = PIFF_SENC_UUID.to_vec();
        body.extend_from_slice(&[0u8; 8]);
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&bx(b"uuid", &body)));
        assert_eq!(analyze(&f).unwrap().encryption, Some(EncryptionScheme::Piff));
    }

    #[test]
    fn unprotected_file_reports_no_encryption() {
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&stsd(1))));
        assert!(!analyze(&f).unwrap().is_protected());
    }

    #[test]
    fn edit_list_is_detected_deep_in_the_track_tree() {
        // docs/12 §3.2: ignoring `elst` is the #1 A/V-sync bug in MP4.
        let edts = bx(b"edts", &bx(b"elst", &[0u8; 20]));
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&edts)));
        assert!(analyze(&f).unwrap().has_edit_list);
    }

    #[test]
    fn multiple_sample_descriptions_are_flagged() {
        // docs/12 §3.3: parameters change mid-file; the renderer must reconfigure without stopping.
        let stbl = bx(b"stbl", &stsd(2));
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &minf);
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&mdia)));
        assert!(analyze(&f).unwrap().multiple_sample_descriptions);

        let single = bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stsd(1))));
        let mut g = ftyp(b"isom", &[]);
        g.extend(moov(&trak(&single)));
        assert!(!analyze(&g).unwrap().multiple_sample_descriptions);
    }

    #[test]
    fn co64_marks_a_large_file() {
        let stbl = bx(b"stbl", &bx(b"co64", &[0u8; 16]));
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&trak(&bx(b"mdia", &bx(b"minf", &stbl)))));
        assert!(analyze(&f).unwrap().has_co64);
    }

    #[test]
    fn nero_chapters_and_chapter_track_refs_are_both_found() {
        // docs/12 §3.5: different tools write different mechanisms; all must be readable.
        let udta = bx(b"udta", &bx(b"chpl", &[0u8; 12]));
        let tref = bx(b"tref", &bx(b"chap", &[0, 0, 0, 2]));
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&{
            let mut inner = trak(&tref);
            inner.extend(udta);
            inner
        }));
        let l = analyze(&f).unwrap();
        assert!(l.has_nero_chapters);
        assert!(l.has_chapter_track_ref);
    }

    #[test]
    fn concatenated_files_are_detected() {
        // docs/12 §3.7: two complete MP4s cat'd together.
        let mut one = ftyp(b"isom", &[]);
        one.extend(moov(&[]));
        one.extend(bx(b"mdat", &[0u8; 8]));
        let mut both = one.clone();
        both.extend(one);
        assert!(analyze(&both).unwrap().concatenated);
    }

    #[test]
    fn unknown_boxes_are_skipped_not_fatal() {
        let mut f = ftyp(b"isom", &[]);
        f.extend(bx(b"zzzz", &[0xAA; 32])); // vendor extension
        f.extend(moov(&trak(&[])));
        let l = analyze(&f).unwrap();
        assert_eq!(l.moov, MoovPlacement::BeforeMdat, "parse continued past the unknown box");
        assert_eq!(l.track_count, 1);
    }

    #[test]
    fn a_box_size_smaller_than_its_header_does_not_loop_forever() {
        let mut f = ftyp(b"isom", &[]);
        f.extend_from_slice(&4u32.to_be_bytes()); // impossible size
        f.extend_from_slice(b"junk");
        f.extend(moov(&[]));
        let l = analyze(&f).unwrap();
        assert!(l.truncated, "corrupt size reported rather than spun on");
    }

    #[test]
    fn truncation_at_every_offset_never_panics() {
        let mut f = ftyp(b"isom", &[b"iso2"]);
        f.extend(moov(&trak(&bx(b"edts", &bx(b"elst", &[0u8; 20])))));
        f.extend(bx(b"mdat", &[0u8; 64]));
        for cut in 0..f.len() {
            let _ = analyze(&f[..cut]);
        }
    }

    #[test]
    fn non_isobmff_input_is_rejected_cleanly() {
        assert!(analyze(b"").is_none());
        assert!(analyze(b"short").is_none());
        assert!(analyze(&[0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x02, 0x03, 0x04]).is_none());
    }

    #[test]
    fn deep_nesting_terminates() {
        let mut body = vec![0u8; 4];
        for _ in 0..40 {
            body = bx(b"trak", &body);
        }
        let mut f = ftyp(b"isom", &[]);
        f.extend(moov(&body));
        assert!(analyze(&f).is_some());
    }
}
