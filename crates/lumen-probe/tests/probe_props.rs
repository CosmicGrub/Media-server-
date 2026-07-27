//! Property tests for the probe layer.
//!
//! These parsers are the first code to touch a file, and the bytes are entirely attacker-controlled:
//! anyone who can get a file into a watched folder — a download client, a network share, a shared
//! library — controls their content. A panic here is a denial of service on a server that indexes
//! other people's private files, and an abort on a client is a crash on someone's TV.
//!
//! So the properties are deliberately blunt:
//!
//! 1. **No input panics.** Random bytes, valid files, truncated files, and bit-flipped files.
//! 2. **No input hangs.** Every parser makes forward progress or returns.
//! 3. **Sniffing always yields a candidate.** Guarantee G2 forbids "unsupported format".
//! 4. **Extension never overrides content.** Rule 1 from `docs/12` §1.
//!
//! `docs/04` §9 lists fuzzing these parsers as a Phase 1 requirement; this is the in-tree property
//! layer beneath that.

use lumen_model::Container;
use lumen_probe::{ebml, isobmff, magic};
use proptest::prelude::*;

// ── Structured builders, so the fuzzer spends its budget on plausible files ───────────────────────

fn matroska_prefix() -> Vec<u8> {
    // EBML header declaring DocType "matroska", then a Segment with a 4-byte unknown-ish size.
    let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x10, 0x00, 0x00, 0x0E];
    v.extend_from_slice(&[0x42, 0x82, 0x10, 0x00, 0x00, 0x08]);
    v.extend_from_slice(b"matroska");
    v.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x10, 0x00, 0x00, 0x10]);
    v
}

fn isobmff_prefix() -> Vec<u8> {
    let mut v = vec![0, 0, 0, 0x18];
    v.extend_from_slice(b"ftypisom");
    v.extend_from_slice(b"\0\0\x02\0isomiso2mp41");
    v
}

/// A plausible file: a real header followed by arbitrary bytes. Far more likely to reach deep code
/// paths than uniform random input.
fn plausible_file() -> impl Strategy<Value = Vec<u8>> {
    let prefix = prop_oneof![
        Just(matroska_prefix()),
        Just(isobmff_prefix()),
        Just(vec![0x47u8; 188 * 5]),
        Just(b"RIFF\x00\x00\x00\x00AVI LIST".to_vec()),
        Just(Vec::new()),
    ];
    (prefix, proptest::collection::vec(any::<u8>(), 0..2048)).prop_map(|(mut p, tail)| {
        p.extend(tail);
        p
    })
}

/// Run every parser over `data`. Returning at all is the assertion.
fn parse_all(data: &[u8]) {
    let mut candidates = magic::sniff(data);
    magic::rank_with_extension(&mut candidates, Some("mkv"));
    let _ = ebml::analyze(data);
    let _ = isobmff::analyze(data);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Invariant 1 and 2, over uniform random bytes.
    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        parse_all(&data);
    }

    /// The same, over inputs that actually reach the structural parsers.
    #[test]
    fn plausible_files_never_panic(data in plausible_file()) {
        parse_all(&data);
    }

    /// Truncation at an arbitrary offset must be handled — partial downloads and interrupted
    /// recordings are the single most common damage class in a real library (`docs/12` §2.8, §3.7).
    #[test]
    fn truncation_at_any_offset_never_panics(
        data in plausible_file(),
        cut in 0usize..4096,
    ) {
        let end = cut.min(data.len());
        parse_all(&data[..end]);
    }

    /// A single bit flip must not turn a valid file into a crash. This is the shape of real disk rot
    /// and of a truncated network read landing mid-structure.
    #[test]
    fn bit_flips_never_panic(
        data in plausible_file(),
        index in 0usize..4096,
        bit in 0u8..8,
    ) {
        let mut mutated = data;
        if !mutated.is_empty() {
            let i = index % mutated.len();
            mutated[i] ^= 1 << bit;
        }
        parse_all(&mutated);
    }

    /// Guarantee G2: the player never says "unsupported format", so there is always something to try.
    #[test]
    fn sniff_always_offers_a_candidate(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let got = magic::sniff(&data);
        prop_assert!(!got.is_empty());
        prop_assert!(
            got.iter().any(|c| c.container == Container::RawElementaryStream),
            "the headerless-stream fallback must always be present"
        );
    }

    /// Candidates come back ordered by confidence, because the caller tries them in order and a
    /// mis-ordered list means a weak guess is attempted before a certain match.
    #[test]
    fn candidates_are_ordered_by_descending_confidence(
        data in plausible_file(),
        ext in proptest::option::of(prop_oneof![
            Just("mkv"), Just("mp4"), Just("ts"), Just("avi"), Just("bogus")
        ]),
    ) {
        let mut got = magic::sniff(&data);
        magic::rank_with_extension(&mut got, ext);
        for pair in got.windows(2) {
            prop_assert!(
                pair[0].confidence >= pair[1].confidence,
                "confidence went up across {:?} -> {:?}", pair[0], pair[1]
            );
        }
    }

    /// Rule 1 (`docs/12` §1): the extension may reorder equally-confident candidates and nothing
    /// more. It can never add one, remove one, or promote a weaker match — otherwise a mislabelled
    /// file becomes unplayable, which is the exact failure the rule exists to prevent.
    #[test]
    fn extension_cannot_change_the_candidate_set_or_the_top_confidence(
        data in plausible_file(),
        ext in prop_oneof![Just("mkv"), Just("mp4"), Just("ts"), Just("webm"), Just("avi")],
    ) {
        let before = magic::sniff(&data);
        let mut after = before.clone();
        magic::rank_with_extension(&mut after, Some(ext));

        prop_assert_eq!(before.len(), after.len(), "extension changed the candidate count");
        for c in &before {
            prop_assert!(
                after.iter().any(|d| d.container == c.container),
                "extension removed candidate {:?}", c.container
            );
        }
        prop_assert_eq!(
            before[0].confidence, after[0].confidence,
            "extension promoted a weaker candidate to the front"
        );
    }

    /// Whatever the parsers conclude, the derived questions must be answerable without panicking —
    /// these accessors are called on the playback path.
    #[test]
    fn derived_questions_are_total(data in plausible_file()) {
        if let Some(l) = ebml::analyze(&data) {
            let scale = l.effective_timestamp_scale();
            prop_assert!(scale > 0, "a zero timestamp scale would divide by zero downstream");
            let _ = l.needs_header_stripping();
            let _ = l.needs_link_resolution();
            let _ = l.needs_tail_fetch_for_seeking();
        }
        if let Some(l) = isobmff::analyze(&data) {
            let _ = l.needs_tail_fetch();
            let _ = l.needs_moov_reconstruction();
            let _ = l.is_protected();
            // A fragmented segment legitimately has no `moov`; asking to reconstruct one would send
            // the recovery ladder up to rung 4 for a perfectly healthy file.
            if l.fragmented {
                prop_assert!(!l.needs_moov_reconstruction());
            }
        }
    }

    /// A buffer cannot be both well-formed Matroska and well-formed ISOBMFF. Overlapping detection
    /// would make the demuxer ranking meaningless.
    #[test]
    fn matroska_and_isobmff_detection_are_mutually_exclusive(data in plausible_file()) {
        let mkv = ebml::analyze(&data).is_some();
        let mp4 = isobmff::analyze(&data).is_some();
        prop_assert!(!(mkv && mp4), "both parsers claimed the same buffer");
    }
}

// ── Regression corpus: inputs that once broke something, kept forever ─────────────────────────────

/// Shapes that are cheap to get wrong and expensive to get wrong in production. Each is a real
/// class, not a synthetic curiosity.
#[test]
fn known_awkward_inputs_are_handled() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("one byte", vec![0x1A]),
        ("EBML magic only", vec![0x1A, 0x45, 0xDF, 0xA3]),
        ("all zeros", vec![0u8; 1024]),
        ("all ones", vec![0xFFu8; 1024]),
        // An 8-byte vint size is how unknown-size elements are written, and computing its data mask
        // naively shifts a u8 by 8, which panics in debug builds.
        ("unknown-size segment", {
            let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x81, b'x'];
            v.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
            v.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
            v
        }),
        // A box declaring a size smaller than its own header: naive arithmetic underflows or spins.
        ("mp4 box size below header", {
            let mut v = vec![0, 0, 0, 0x10];
            v.extend_from_slice(b"ftypisom\0\0\0\0");
            v.extend_from_slice(&[0, 0, 0, 4]);
            v.extend_from_slice(b"junk");
            v
        }),
        // `size == 0` means "to EOF"; treating it as an empty box makes the walker spin forever.
        ("mp4 zero-size mdat", {
            let mut v = vec![0, 0, 0, 0x10];
            v.extend_from_slice(b"ftypisom\0\0\0\0");
            v.extend_from_slice(&[0, 0, 0, 0]);
            v.extend_from_slice(b"mdat");
            v.extend_from_slice(&[0xAA; 64]);
            v
        }),
        // A largesize claiming more than the buffer holds.
        ("mp4 largesize beyond eof", {
            let mut v = vec![0, 0, 0, 1];
            v.extend_from_slice(b"mdat");
            v.extend_from_slice(&u64::MAX.to_be_bytes());
            v
        }),
        ("text file", b"This is plainly not a media file.\n".to_vec()),
        ("html", b"<!DOCTYPE html><html><body>404</body></html>".to_vec()),
    ];

    for (name, data) in cases {
        let candidates = magic::sniff(&data);
        assert!(!candidates.is_empty(), "{name}: no candidate offered");
        let _ = ebml::analyze(&data);
        let _ = isobmff::analyze(&data);
    }
}
