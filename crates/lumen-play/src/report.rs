//! Console and JSON output.
//!
//! The console view is what you read while the run happens. The JSON is the artefact: a per-file
//! record of what your library actually did, which is the point of running this against a real
//! collection rather than a test corpus.

use std::collections::BTreeMap;

use crate::json::quote;
use crate::scan::{MediaKind, Scan, ScannedFile, group};
use crate::session::{FileResult, Outcome, SessionReport};

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

fn human_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "—".into();
    }
    let total = seconds as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Truncate to `width` display columns, with an ellipsis when cut.
fn ellipsize(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
}

/// What the scan found, before anything is played.
pub fn render_scan(scan: &Scan) -> String {
    let mut s = String::new();
    let items = group(scan);
    let videos = scan.files.iter().filter(|f| f.kind == MediaKind::Video).count();
    let audio = scan.files.iter().filter(|f| f.kind == MediaKind::Audio).count();
    let subs = scan.subtitles().count();
    let bytes: u64 = scan.playable().map(|f| f.size).sum();

    s.push_str(&format!(
        "library\n  {videos} video, {audio} audio, {subs} sidecar subtitle files\n  \
         {} logical items, {} total\n",
        items.len(),
        human_size(bytes)
    ));
    if scan.skipped_samples > 0 {
        s.push_str(&format!(
            "  {} sample clips skipped (--include-samples to keep them)\n",
            scan.skipped_samples
        ));
    }
    if scan.truncated {
        s.push_str("  NOTE: --limit cut this scan short; this is not the whole library\n");
    }

    // Containers actually present, by content rather than by extension. On a real collection this is
    // usually the first genuinely surprising line.
    let mut containers: BTreeMap<String, usize> = BTreeMap::new();
    for f in scan.playable() {
        let key = f.container.map_or_else(|| "unidentified".to_string(), |c| format!("{c:?}"));
        *containers.entry(key).or_default() += 1;
    }
    if !containers.is_empty() {
        let mut parts: Vec<(String, usize)> = containers.into_iter().collect();
        parts.sort_by(|a, b| b.1.cmp(&a.1));
        let text: Vec<String> = parts.iter().map(|(k, v)| format!("{k} {v}")).collect();
        s.push_str(&format!("  containers: {}\n", text.join(", ")));
    }

    let flagged: Vec<&ScannedFile> = scan.files.iter().filter(|f| !f.notes().is_empty()).collect();
    if !flagged.is_empty() {
        s.push_str(&format!("\nneeds a look ({})\n", flagged.len()));
        for f in flagged.iter().take(40) {
            s.push_str(&format!("  {}\n", ellipsize(&f.file_name(), 76)));
            for n in f.notes() {
                s.push_str(&format!("      {n}\n"));
            }
        }
        if flagged.len() > 40 {
            s.push_str(&format!("  ... and {} more (--json for all)\n", flagged.len() - 40));
        }
    }

    if !scan.unreadable.is_empty() {
        s.push_str(&format!("\nunreadable ({})\n", scan.unreadable.len()));
        for (p, e) in scan.unreadable.iter().take(10) {
            s.push_str(&format!("  {} — {e}\n", p.display()));
        }
    }
    s
}

/// The collection, grouped.
pub fn render_items(scan: &Scan) -> String {
    let items = group(scan);
    let mut s = format!("items ({})\n", items.len());
    for it in &items {
        let mut line = it.title.clone();
        if let Some(y) = it.year {
            line.push_str(&format!(" ({y})"));
        }
        if let Some(season) = it.season {
            line.push_str(&format!(" — season {season}, {} episodes", it.files.len()));
        } else if it.files.len() > 1 {
            line.push_str(&format!(" — {} files", it.files.len()));
        }
        s.push_str(&format!("  {}\n", ellipsize(&line, 90)));
    }
    s
}

/// One line per file as it plays.
pub fn render_progress(r: &FileResult, n: usize, total: usize) -> String {
    match &r.outcome {
        Outcome::Failed(why) => {
            format!("[{n}/{total}] FAILED  {}\n          {why}", ellipsize(&r.label, 60))
        }
        _ => {
            let spec = [
                r.resolution(),
                r.video_codec.clone(),
                r.audio_codec.clone(),
                r.is_hdr().then(|| r.gamma.clone().unwrap_or_default().to_uppercase()),
                r.hwdec.as_ref().filter(|h| *h != "no").map(|h| format!("hw:{h}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            format!("[{n}/{total}] {}  {spec}", ellipsize(&r.label, 50))
        }
    }
}

/// The verdict on a whole run.
pub fn render_session(rep: &SessionReport) -> String {
    let total = rep.results.len();
    let played = rep.played();
    let failed: Vec<&FileResult> = rep.failed().collect();
    let sw: Vec<&FileResult> = rep.software_decoded().collect();

    let mut s = format!(
        "\nresult\n  {played}/{total} played, {} failed, {} not reached, {} elapsed\n",
        failed.len(),
        rep.not_reached(),
        human_duration(rep.elapsed_s)
    );
    if let Some(v) = &rep.mpv_version {
        s.push_str(&format!(
            "  {v}{}\n",
            rep.vo_used.as_ref().map_or(String::new(), |vo| format!("  vo={vo}"))
        ));
    }
    if rep.ended_early {
        s.push_str("  NOTE: mpv exited before the playlist finished; the remainder is untested\n");
    }

    if !failed.is_empty() {
        s.push_str(&format!("\nfailed ({})\n", failed.len()));
        for f in &failed {
            s.push_str(&format!("  {}\n", f.path.display()));
            if let Outcome::Failed(why) = &f.outcome {
                s.push_str(&format!("      {why}\n"));
            }
        }
    }

    // Software decoding is not a failure, but a library that plays only because the CPU is carrying
    // it will not play on a weaker device — which is exactly what a multi-platform product cares
    // about. Worth its own section rather than a footnote.
    if !sw.is_empty() {
        s.push_str(&format!(
            "\nsoftware decoded ({}) — played, but with no hardware decoder\n",
            sw.len()
        ));
        for f in sw.iter().take(20) {
            s.push_str(&format!(
                "  {}  {}\n",
                ellipsize(&f.label, 56),
                [f.resolution(), f.video_codec.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if sw.len() > 20 {
            s.push_str(&format!("  ... and {} more\n", sw.len() - 20));
        }
    }

    // Plays forward but cannot be navigated: a lost Matroska Cues element or an MP4 whose `moov`
    // is unusable. A play-through test would never notice, because playing forward still works —
    // the user finds out the first time they try to skip.
    let unseekable: Vec<&FileResult> = rep.unseekable().collect();
    if !unseekable.is_empty() {
        s.push_str(&format!(
            "\nnot seekable ({}) — plays, but cannot be navigated (missing or broken index)\n",
            unseekable.len()
        ));
        for f in unseekable.iter().take(20) {
            s.push_str(&format!("  {}\n", ellipsize(&f.label, 66)));
        }
        if unseekable.len() > 20 {
            s.push_str(&format!("  ... and {} more\n", unseekable.len() - 20));
        }
    }

    let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
    for r in rep.results.iter().filter(|r| r.outcome == Outcome::Played) {
        if let Some(c) = &r.video_codec {
            *codecs.entry(c.clone()).or_default() += 1;
        }
    }
    if !codecs.is_empty() {
        let mut v: Vec<(String, usize)> = codecs.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let text: Vec<String> = v.iter().map(|(k, n)| format!("{k} {n}")).collect();
        s.push_str(&format!("\nvideo codecs played: {}\n", text.join(", ")));
    }

    let stutter: Vec<&FileResult> =
        rep.results.iter().filter(|r| r.delayed_frames.is_some_and(|d| d > 10)).collect();
    if !stutter.is_empty() {
        s.push_str(&format!("\nlate frames ({} files)\n", stutter.len()));
        for f in stutter.iter().take(15) {
            s.push_str(&format!(
                "  {}  {} late over {}\n",
                ellipsize(&f.label, 50),
                f.delayed_frames.unwrap_or(0),
                human_duration(f.seconds_played)
            ));
        }
    }
    s
}

fn opt_str(v: Option<&str>) -> String {
    v.map_or_else(|| "null".into(), quote)
}

fn opt_num<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".into(), |x| x.to_string())
}

fn f64_or_null(v: Option<f64>) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x:.3}"),
        _ => "null".into(),
    }
}

/// The full machine-readable record of a scan and, if one ran, a playback session.
pub fn render_json(scan: &Scan, session: Option<&SessionReport>) -> String {
    let mut s = String::from("{\n  \"tool\": \"lumen-play\",\n  \"schema\": 1,\n");

    s.push_str("  \"files\": [\n");
    let entries: Vec<String> = scan
        .files
        .iter()
        .map(|f| {
            format!(
                "    {{\"path\":{},\"size\":{},\"kind\":{:?},\"container\":{},\"confidence\":{},\
                 \"evidence\":{},\"extension_mismatch\":{},\"unidentified\":{},\"title\":{},\"year\":{},\
                 \"notes\":[{}]}}",
                quote(&f.path.to_string_lossy()),
                f.size,
                format!("{:?}", f.kind),
                opt_str(f.container.map(|c| format!("{c:?}")).as_deref()),
                opt_str(f.confidence.map(|c| format!("{c:?}")).as_deref()),
                opt_str(f.evidence),
                f.extension_mismatch,
                f.unidentified,
                quote(&f.parsed.title),
                opt_num(f.parsed.year),
                f.notes().iter().map(|n| quote(n)).collect::<Vec<_>>().join(",")
            )
        })
        .collect();
    s.push_str(&entries.join(",\n"));
    s.push_str("\n  ],\n");

    s.push_str(&format!(
        "  \"unreadable\": [{}],\n  \"skipped_samples\": {},\n  \"truncated\": {},\n",
        scan.unreadable
            .iter()
            .map(|(p, e)| format!(
                "{{\"path\":{},\"error\":{}}}",
                quote(&p.to_string_lossy()),
                quote(e)
            ))
            .collect::<Vec<_>>()
            .join(","),
        scan.skipped_samples,
        scan.truncated
    ));

    match session {
        None => s.push_str("  \"session\": null\n"),
        Some(rep) => {
            let results: Vec<String> = rep
                .results
                .iter()
                .map(|r| {
                    let (outcome, detail) = match &r.outcome {
                        Outcome::Played => ("played", None),
                        Outcome::Failed(w) => ("failed", Some(w.clone())),
                        Outcome::NotReached => ("not_reached", None),
                    };
                    format!(
                        "      {{\"path\":{},\"outcome\":{},\"error\":{},\"seconds_played\":{},\
                         \"file_format\":{},\"video_codec\":{},\"audio_codec\":{},\"width\":{},\
                         \"height\":{},\"fps\":{},\"duration\":{},\"hwdec\":{},\"seekable\":{},\
                         \"pixel_format\":{},\"primaries\":{},\"gamma\":{},\"hdr\":{},\
                         \"tracks\":{{\"video\":{},\"audio\":{},\"subtitle\":{}}},\
                         \"delayed_frames\":{},\"dropped_frames\":{}}}",
                        quote(&r.path.to_string_lossy()),
                        quote(outcome),
                        opt_str(detail.as_deref()),
                        f64_or_null(Some(r.seconds_played)),
                        opt_str(r.file_format.as_deref()),
                        opt_str(r.video_codec.as_deref()),
                        opt_str(r.audio_codec.as_deref()),
                        opt_num(r.width),
                        opt_num(r.height),
                        f64_or_null(r.fps),
                        f64_or_null(r.duration),
                        opt_str(r.hwdec.as_deref()),
                        r.seekable.map_or_else(|| "null".to_string(), |b| b.to_string()),
                        opt_str(r.pixel_format.as_deref()),
                        opt_str(r.primaries.as_deref()),
                        opt_str(r.gamma.as_deref()),
                        r.is_hdr(),
                        r.track_counts.video,
                        r.track_counts.audio,
                        r.track_counts.subtitle,
                        opt_num(r.delayed_frames),
                        opt_num(r.dropped_frames)
                    )
                })
                .collect();
            s.push_str(&format!(
                "  \"session\": {{\n    \"mpv_version\": {},\n    \"vo\": {},\n    \
                 \"elapsed_s\": {},\n    \"ended_early\": {},\n    \"results\": [\n{}\n    ]\n  }}\n",
                opt_str(rep.mpv_version.as_deref()),
                opt_str(rep.vo_used.as_deref()),
                f64_or_null(Some(rep.elapsed_s)),
                rep.ended_early,
                results.join(",\n")
            ));
        }
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;
    use crate::scan::{ScanOptions, scan};
    use std::io::Write;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let anchor = 0u8;
            let d = std::env::temp_dir().join(format!(
                "lumen-rep-{tag}-{}-{:x}",
                std::process::id(),
                std::ptr::from_ref(&anchor) as usize
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn file(&self, rel: &str, bytes: &[u8]) {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mkv() -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3];
        v.extend(std::iter::repeat_n(0u8, 2_000_000));
        v
    }

    fn sample_scan(tag: &str) -> (TempDir, Scan) {
        let d = TempDir::new(tag);
        d.file("Arrival (2016) 2160p BluRay x265.mkv", &mkv());
        d.file("Show.S01E01.1080p.mkv", &mkv());
        d.file("Show.S01E02.1080p.mkv", &mkv());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        (d, s)
    }

    #[test]
    fn a_scan_report_names_what_was_found() {
        let (_d, s) = sample_scan("scanrep");
        let text = render_scan(&s);
        assert!(text.contains("3 video"), "{text}");
        assert!(text.contains("Matroska"), "containers come from content: {text}");
    }

    #[test]
    fn a_truncated_scan_says_so_rather_than_implying_the_library_ends() {
        let d = TempDir::new("trunc");
        for n in ["a.mkv", "b.mkv", "c.mkv"] {
            d.file(n, &mkv());
        }
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions { limit: Some(1), ..Default::default() });
        assert!(render_scan(&s).contains("not the whole library"), "{}", render_scan(&s));
    }

    #[test]
    fn items_group_a_season_into_one_line() {
        let (_d, s) = sample_scan("items");
        let text = render_items(&s);
        assert!(text.contains("season 1, 2 episodes"), "{text}");
        assert!(text.contains("Arrival"), "{text}");
    }

    #[test]
    fn the_session_report_separates_failure_from_never_reached() {
        // Counting unattempted files as failures would report a library as broken when the run was
        // simply cut short.
        let rep = SessionReport {
            results: vec![
                FileResult {
                    path: "/a.mkv".into(),
                    label: "A".into(),
                    outcome: Outcome::Played,
                    ..blank()
                },
                FileResult {
                    path: "/b.mkv".into(),
                    label: "B".into(),
                    outcome: Outcome::Failed("Unrecognized file format".into()),
                    ..blank()
                },
                FileResult {
                    path: "/c.mkv".into(),
                    label: "C".into(),
                    outcome: Outcome::NotReached,
                    ..blank()
                },
            ],
            elapsed_s: 65.0,
            ..Default::default()
        };
        let text = render_session(&rep);
        assert!(text.contains("1/3 played, 1 failed, 1 not reached"), "{text}");
        assert!(text.contains("Unrecognized file format"), "the reason is the useful part: {text}");
    }

    #[test]
    fn software_decoded_files_get_their_own_section() {
        // Not a failure, but a library that plays only because the CPU carries it will not play on a
        // weaker device — which is the whole question for a multi-platform product.
        let rep = SessionReport {
            results: vec![FileResult {
                path: "/a.mkv".into(),
                label: "A".into(),
                outcome: Outcome::Played,
                hwdec: Some("no".into()),
                video_codec: Some("av1".into()),
                ..blank()
            }],
            ..Default::default()
        };
        let text = render_session(&rep);
        assert!(text.contains("software decoded (1)"), "{text}");
        assert!(!text.contains("failed (1)"), "software decoding is not a failure: {text}");
    }

    #[test]
    fn the_json_report_parses_back() {
        let (_d, s) = sample_scan("json");
        let rep = SessionReport {
            results: vec![FileResult {
                path: "/m/Film \"Cut\" (2019).mkv".into(),
                label: "Film".into(),
                outcome: Outcome::Failed("no such codec: \"x\"".into()),
                video_codec: Some("hevc".into()),
                width: Some(3840),
                height: Some(2160),
                ..blank()
            }],
            mpv_version: Some("mpv 0.38.0".into()),
            elapsed_s: 12.5,
            ..Default::default()
        };
        let text = render_json(&s, Some(&rep));
        let v = parse(&text).expect("the report must be valid JSON");
        assert_eq!(v.get("schema").and_then(crate::json::Value::as_f64), Some(1.0));
        let results = v
            .get("session")
            .and_then(|s| s.get("results"))
            .and_then(crate::json::Value::as_array)
            .unwrap();
        assert_eq!(results[0].get("outcome").and_then(crate::json::Value::as_str), Some("failed"));
        // Quotes inside a path and inside an error message are the classic way hand-emitted JSON
        // breaks, and both are ordinary here.
        assert_eq!(
            results[0].get("path").and_then(crate::json::Value::as_str),
            Some(r#"/m/Film "Cut" (2019).mkv"#)
        );
    }

    #[test]
    fn a_scan_only_report_has_a_null_session_rather_than_fabricated_results() {
        let (_d, s) = sample_scan("nosession");
        let text = render_json(&s, None);
        let v = parse(&text).unwrap();
        assert_eq!(v.get("session"), Some(&crate::json::Value::Null));
        assert_eq!(v.get("files").and_then(crate::json::Value::as_array).map(<[_]>::len), Some(3));
    }

    #[test]
    fn non_finite_numbers_do_not_break_the_json() {
        // A file with no duration yields NaN from a division upstream; emitting it would make the
        // whole report unparseable instead of one field unknown.
        assert_eq!(f64_or_null(Some(f64::NAN)), "null");
        assert_eq!(f64_or_null(Some(f64::INFINITY)), "null");
        assert_eq!(f64_or_null(None), "null");
    }

    #[test]
    fn sizes_and_durations_read_the_way_a_person_writes_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MiB");
        assert_eq!(human_duration(0.0), "—");
        assert_eq!(human_duration(65.0), "1:05");
        assert_eq!(human_duration(3725.0), "1:02:05");
    }

    #[test]
    fn long_titles_are_cut_without_breaking_a_character() {
        // Truncating by byte index inside a multi-byte character would panic, and non-ASCII titles
        // are completely ordinary.
        let s = ellipsize("映画のタイトルがとても長い場合", 5);
        assert_eq!(s.chars().count(), 5);
        assert!(s.ends_with('…'));
        assert_eq!(ellipsize("short", 20), "short");
    }

    fn blank() -> FileResult {
        FileResult {
            path: PathBuf::new(),
            label: String::new(),
            outcome: Outcome::NotReached,
            seconds_played: 0.0,
            file_format: None,
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            fps: None,
            duration: None,
            hwdec: None,
            pixel_format: None,
            primaries: None,
            gamma: None,
            seekable: None,
            audio_channels: None,
            track_counts: crate::session::TrackCounts::default(),
            delayed_frames: None,
            dropped_frames: None,
        }
    }
}
