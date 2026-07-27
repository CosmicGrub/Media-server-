//! Release-token vocabulary.
//!
//! Everything a scene, P2P, or self-rip filename puts *after* the title. Recognising these is what
//! lets the parser find where the title ends — the title is whatever precedes the first token that
//! is unambiguously technical.
//!
//! The vocabulary is data, not logic, so adding a newly-fashionable tag is a one-line change and the
//! labelled corpus in `tests/` immediately says whether it broke anything.

/// Vertical resolution as written in filenames, not as measured. `2160p` and `4K` mean the same
/// thing to a release group and must classify identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resolution {
    Sd,
    P480,
    P576,
    P720,
    P1080,
    P1440,
    P2160,
    P4320,
}

/// Where the bits came from. Ordered by fidelity, because a release labelled both `BluRay` and
/// `WEBRip` (they exist) should be read as the better of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Camcorder, telesync, screener — the bottom of the quality ladder.
    Cam,
    VhsRip,
    Sdtv,
    Hdtv,
    DvdRip,
    HdRip,
    WebRip,
    WebDl,
    BluRayRip,
    /// Bit-exact copy of the disc streams. The product's headline case.
    Remux,
}

/// A cut or release variant. Multiple may apply at once (`IMAX Extended Remastered`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Edition {
    Theatrical,
    DirectorsCut,
    Extended,
    Unrated,
    Uncut,
    FinalCut,
    Imax,
    OpenMatte,
    Remastered,
    Criterion,
    SpecialEdition,
    UltimateEdition,
    AnniversaryEdition,
    Redux,
    Despecialized,
    Hybrid,
}

/// HDR system as tagged in the filename. Advisory only — the authoritative answer comes from the
/// probe (`lumen-probe`), and filename tags are wrong often enough that they must never override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HdrTag {
    Sdr,
    Hdr10,
    Hdr10Plus,
    Hlg,
    DolbyVision,
}

/// What a single token turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenClass {
    Resolution(Resolution),
    Source(Source),
    Edition(Edition),
    Hdr(HdrTag),
    VideoCodec(&'static str),
    AudioCodec(&'static str),
    ChannelLayout(&'static str),
    Language(&'static str),
    Container(&'static str),
    /// Status flags carrying no matching signal: `PROPER`, `REPACK`, `iNTERNAL`.
    Flag(&'static str),
    /// A CRC32 or release hash, usually in trailing brackets on anime releases.
    Hash,
    /// Part of the title, or something we do not recognise.
    Unknown,
}

impl TokenClass {
    /// True when this token is unambiguously technical, so the title must have ended before it.
    ///
    /// `Language` is deliberately excluded: `The German Doctor` and `French Kiss` are titles, and
    /// treating a language word as a boundary truncates them.
    pub fn is_title_boundary(&self) -> bool {
        matches!(
            self,
            Self::Resolution(_)
                | Self::Source(_)
                | Self::Hdr(_)
                | Self::VideoCodec(_)
                | Self::AudioCodec(_)
                | Self::ChannelLayout(_)
                | Self::Flag(_)
                | Self::Hash
        )
    }
}

/// Classify one lowercased token.
pub fn classify(token: &str) -> TokenClass {
    use Edition as E;
    use HdrTag as H;
    use Resolution as R;
    use Source as S;

    match token {
        // ── Resolution ────────────────────────────────────────────────────────────────────────────
        "480p" | "480i" => TokenClass::Resolution(R::P480),
        "576p" | "576i" => TokenClass::Resolution(R::P576),
        "720p" | "720i" | "hd" => TokenClass::Resolution(R::P720),
        "1080p" | "1080i" | "fullhd" | "fhd" => TokenClass::Resolution(R::P1080),
        "1440p" | "2k" | "qhd" => TokenClass::Resolution(R::P1440),
        "2160p" | "4k" | "uhd" => TokenClass::Resolution(R::P2160),
        "4320p" | "8k" => TokenClass::Resolution(R::P4320),
        "sd" => TokenClass::Resolution(R::Sd),

        // ── Source ────────────────────────────────────────────────────────────────────────────────
        "remux" | "bdremux" | "uhdremux" => TokenClass::Source(S::Remux),
        "bluray" | "blu" | "bdrip" | "brrip" | "bd" | "bdmv" | "uhdbluray" | "bluray2160p" => {
            TokenClass::Source(S::BluRayRip)
        }
        "webdl" | "web" | "amzn" | "nf" | "dsnp" | "hmax" | "atvp" | "hulu" | "pcok" | "stan" => {
            TokenClass::Source(S::WebDl)
        }
        "webrip" => TokenClass::Source(S::WebRip),
        "hdtv" | "pdtv" | "dsr" | "tvrip" => TokenClass::Source(S::Hdtv),
        "sdtv" => TokenClass::Source(S::Sdtv),
        "dvdrip" | "dvd" | "dvd5" | "dvd9" | "dvdr" | "ntsc" | "pal" => {
            TokenClass::Source(S::DvdRip)
        }
        "hdrip" | "hdlight" => TokenClass::Source(S::HdRip),
        "cam" | "camrip" | "ts" | "telesync" | "tc" | "telecine" | "scr" | "screener"
        | "dvdscr" | "r5" | "workprint" | "hdcam" => TokenClass::Source(S::Cam),
        "vhsrip" | "vhs" | "ldrip" | "laserdisc" => TokenClass::Source(S::VhsRip),

        // ── Edition ───────────────────────────────────────────────────────────────────────────────
        "theatrical" => TokenClass::Edition(E::Theatrical),
        "dc" | "directorscut" | "directors" => TokenClass::Edition(E::DirectorsCut),
        "extended" | "ext" => TokenClass::Edition(E::Extended),
        "unrated" => TokenClass::Edition(E::Unrated),
        "uncut" => TokenClass::Edition(E::Uncut),
        "finalcut" => TokenClass::Edition(E::FinalCut),
        "imax" => TokenClass::Edition(E::Imax),
        "openmatte" => TokenClass::Edition(E::OpenMatte),
        "remastered" | "restored" | "4krestoration" => TokenClass::Edition(E::Remastered),
        "criterion" => TokenClass::Edition(E::Criterion),
        "se" | "specialedition" => TokenClass::Edition(E::SpecialEdition),
        "ultimate" | "ultimateedition" => TokenClass::Edition(E::UltimateEdition),
        "anniversary" => TokenClass::Edition(E::AnniversaryEdition),
        "redux" => TokenClass::Edition(E::Redux),
        "despecialized" => TokenClass::Edition(E::Despecialized),
        "hybrid" => TokenClass::Edition(E::Hybrid),

        // ── HDR ───────────────────────────────────────────────────────────────────────────────────
        "hdr" | "hdr10" => TokenClass::Hdr(H::Hdr10),
        "hdr10plus" | "hdr10+" => TokenClass::Hdr(H::Hdr10Plus),
        "hlg" => TokenClass::Hdr(H::Hlg),
        "dv" | "dovi" | "dolbyvision" => TokenClass::Hdr(H::DolbyVision),
        "sdr" => TokenClass::Hdr(H::Sdr),

        // ── Video codec ───────────────────────────────────────────────────────────────────────────
        "x264" | "h264" | "avc" | "h" => TokenClass::VideoCodec("h264"),
        "x265" | "h265" | "hevc" => TokenClass::VideoCodec("hevc"),
        "av1" => TokenClass::VideoCodec("av1"),
        "vp9" => TokenClass::VideoCodec("vp9"),
        "vc1" => TokenClass::VideoCodec("vc1"),
        "mpeg2" | "mpeg" => TokenClass::VideoCodec("mpeg2"),
        "xvid" | "divx" => TokenClass::VideoCodec("mpeg4"),
        "hi10p" | "10bit" | "10bits" => TokenClass::VideoCodec("h264-10bit"),
        "8bit" | "12bit" => TokenClass::VideoCodec("bitdepth"),

        // ── Audio ─────────────────────────────────────────────────────────────────────────────────
        "truehd" => TokenClass::AudioCodec("truehd"),
        "atmos" => TokenClass::AudioCodec("atmos"),
        "dtsx" | "dts-x" => TokenClass::AudioCodec("dtsx"),
        "dtshd" | "dtshdma" | "dtsma" => TokenClass::AudioCodec("dtshdma"),
        "dts" => TokenClass::AudioCodec("dts"),
        "eac3" | "ddp" | "ddplus" | "dd+" => TokenClass::AudioCodec("eac3"),
        "ac3" | "dd" => TokenClass::AudioCodec("ac3"),
        "ac4" => TokenClass::AudioCodec("ac4"),
        "aac" | "aacx2" => TokenClass::AudioCodec("aac"),
        "flac" | "flacx2" => TokenClass::AudioCodec("flac"),
        "opus" => TokenClass::AudioCodec("opus"),
        "mp3" => TokenClass::AudioCodec("mp3"),
        "pcm" | "lpcm" => TokenClass::AudioCodec("pcm"),
        "1" | "2" | "5" | "7" => TokenClass::Unknown, // bare channel-count fragments; see below
        "2.0" | "20" => TokenClass::ChannelLayout("2.0"),
        "5.1" | "51" => TokenClass::ChannelLayout("5.1"),
        "6.1" | "61" => TokenClass::ChannelLayout("6.1"),
        "7.1" | "71" => TokenClass::ChannelLayout("7.1"),
        "ddp5" | "dd5" | "ddp51" | "dd51" => TokenClass::AudioCodec("eac3"),

        // ── Language ──────────────────────────────────────────────────────────────────────────────
        "multi" | "dual" | "dualaudio" => TokenClass::Language("mul"),
        "eng" | "english" => TokenClass::Language("eng"),
        "fre" | "french" | "vf" | "vff" | "vfq" | "vostfr" | "truefrench" => {
            TokenClass::Language("fra")
        }
        "ger" | "deu" | "german" => TokenClass::Language("deu"),
        "spa" | "esp" | "spanish" | "castellano" | "latino" => TokenClass::Language("spa"),
        "ita" | "italian" => TokenClass::Language("ita"),
        "jpn" | "jap" | "japanese" => TokenClass::Language("jpn"),
        "kor" | "korean" => TokenClass::Language("kor"),
        "chi" | "cht" | "chs" | "chinese" | "mandarin" | "cantonese" => TokenClass::Language("zho"),
        "rus" | "russian" => TokenClass::Language("rus"),
        "por" | "portuguese" | "ptbr" => TokenClass::Language("por"),
        "nld" | "dutch" => TokenClass::Language("nld"),
        "hin" | "hindi" | "tamil" | "telugu" => TokenClass::Language("hin"),
        "nordic" | "swe" | "swedish" | "nor" | "dan" | "fin" => TokenClass::Language("nordic"),
        "subbed" | "subs" | "softsubs" | "hardsubs" | "sub" => TokenClass::Language("subs"),
        "dubbed" | "dub" => TokenClass::Language("dub"),

        // ── Container ─────────────────────────────────────────────────────────────────────────────
        "mkv" | "mp4" | "avi" | "m2ts" | "iso" | "wmv" | "webm" | "mov" | "flv" => {
            TokenClass::Container("container")
        }

        // ── Status flags: no matching signal, but they mark the end of the title ──────────────────
        "proper" | "repack" | "rerip" | "real" | "internal" | "limited" | "festival"
        | "complete" | "retail" | "readnfo" | "nfo" | "extras" | "sample" | "untouched" | "fs"
        | "ws" | "unrated_extended" | "final" => TokenClass::Flag("flag"),

        other => {
            // An 8-hex-digit token is a CRC32; anime groups append one in brackets.
            if other.len() == 8 && other.chars().all(|c| c.is_ascii_hexdigit()) {
                return TokenClass::Hash;
            }
            TokenClass::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_synonyms_agree() {
        assert_eq!(classify("2160p"), classify("4k"));
        assert_eq!(classify("2160p"), classify("uhd"));
        assert_eq!(classify("1080p"), TokenClass::Resolution(Resolution::P1080));
    }

    #[test]
    fn source_fidelity_ordering_lets_the_better_tag_win() {
        // Releases labelled with two sources exist; the ladder must read as the better one.
        assert!(Source::Remux > Source::BluRayRip);
        assert!(Source::BluRayRip > Source::WebDl);
        assert!(Source::WebDl > Source::WebRip);
        assert!(Source::Hdtv > Source::Cam);
    }

    #[test]
    fn streaming_service_tags_are_web_sources() {
        for tag in ["amzn", "nf", "dsnp", "hmax", "atvp"] {
            assert_eq!(classify(tag), TokenClass::Source(Source::WebDl), "{tag}");
        }
    }

    #[test]
    fn crc32_hashes_are_recognised() {
        assert_eq!(classify("a1b2c3d4"), TokenClass::Hash);
        assert_eq!(classify("DEADBEEF".to_lowercase().as_str()), TokenClass::Hash);
        // Eight characters that are not all hex is not a hash.
        assert_eq!(classify("subsplea"), TokenClass::Unknown);
        // Nor is a seven- or nine-digit hex run.
        assert_eq!(classify("a1b2c3d"), TokenClass::Unknown);
    }

    #[test]
    fn language_words_are_not_title_boundaries() {
        // `The German Doctor` and `French Kiss` are titles. Treating a language word as a boundary
        // would truncate them, which is a whole class of silent mismatches.
        assert!(!classify("german").is_title_boundary());
        assert!(!classify("french").is_title_boundary());
        assert!(!classify("english").is_title_boundary());
    }

    #[test]
    fn technical_tokens_are_title_boundaries() {
        for tok in ["1080p", "bluray", "x265", "truehd", "hdr", "5.1", "proper", "a1b2c3d4"] {
            assert!(classify(tok).is_title_boundary(), "{tok} should end the title");
        }
    }

    #[test]
    fn unknown_tokens_default_to_title_material() {
        assert_eq!(classify("interstellar"), TokenClass::Unknown);
        assert!(!classify("interstellar").is_title_boundary());
    }

    #[test]
    fn editions_are_distinguished_from_each_other() {
        assert_eq!(classify("imax"), TokenClass::Edition(Edition::Imax));
        assert_eq!(classify("extended"), TokenClass::Edition(Edition::Extended));
        assert_eq!(classify("dc"), TokenClass::Edition(Edition::DirectorsCut));
        assert_ne!(classify("theatrical"), classify("extended"));
    }

    #[test]
    fn editions_do_not_end_the_title() {
        // "Extended" can appear inside a title, and editions are usually written after the year
        // anyway, so they carry no boundary information worth the risk.
        assert!(!classify("extended").is_title_boundary());
        assert!(!classify("remastered").is_title_boundary());
    }
}
