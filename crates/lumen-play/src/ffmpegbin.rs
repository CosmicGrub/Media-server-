//! Finding ffmpeg.
//!
//! A structural mirror of `mpvbin.rs`, for the same reason: `ffmpeg` is a separate process this
//! binary shells out to (for `lumen serve`'s HLS segmenting — see `remote::server::hls`), not a
//! library linked in, so "where is it" is a real question. The search order is the same one mpv
//! already uses: an explicit override, then beside the executable (what makes a release bundle
//! self-contained), then wherever the platform normally puts it.
//!
//! Not shared code with `mpvbin.rs` on purpose — two small, independent, easy-to-read modules read
//! better here than one generalised over "which binary", for a search this short.

use std::path::{Path, PathBuf};

/// The executable name on this platform.
pub const FFMPEG_EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// Where to look, in order. Split out from the filesystem check so the ordering is testable — see
/// `mpvbin::candidates`'s own doc comment for why every input is passed in rather than read from the
/// environment here.
pub fn candidates(
    override_path: Option<&Path>,
    exe_dir: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // 1. An explicit override always wins.
    if let Some(p) = override_path {
        out.push(p.to_path_buf());
    }

    // 2. Beside the executable, or in an `ffmpeg` folder next to it — what makes the release bundle
    //    self-contained, the same layout `package-windows.sh --with-mpv` already uses for mpv.
    if let Some(dir) = exe_dir {
        out.push(dir.join(FFMPEG_EXE));
        out.push(dir.join("ffmpeg").join(FFMPEG_EXE));
        out.push(dir.join("ffmpeg").join("bin").join(FFMPEG_EXE));
    }

    // 3. Wherever the platform normally puts it. Checked after the bundle so a deliberate local copy
    //    is never overridden by whatever happens to be installed.
    if cfg!(windows) {
        for var in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                out.push(base.join("Programs").join("ffmpeg").join("bin").join(FFMPEG_EXE));
                out.push(base.join("ffmpeg").join("bin").join(FFMPEG_EXE));
            }
        }
        if let Some(h) = home {
            // scoop and chocolatey, the two package managers most likely to have installed it.
            out.push(
                h.join("scoop")
                    .join("apps")
                    .join("ffmpeg")
                    .join("current")
                    .join("bin")
                    .join(FFMPEG_EXE),
            );
            out.push(h.join("scoop").join("shims").join(FFMPEG_EXE));
        }
        out.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin").join(FFMPEG_EXE));
    } else {
        for dir in ["/usr/local/bin", "/usr/bin", "/snap/bin"] {
            out.push(Path::new(dir).join(FFMPEG_EXE));
        }
    }
    out
}

/// The ffmpeg this run should use, or `None` if there is none.
///
/// Returns a bare `ffmpeg` when only `PATH` has it, so the OS does the lookup — resolving it here
/// would mean reimplementing `PATHEXT` handling on Windows for no benefit, the same reasoning
/// `mpvbin::find` already applies.
pub fn find() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let home = home_dir();
    let over =
        std::env::var_os("LUMEN_FFMPEG").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
    for c in candidates(over.as_deref(), exe_dir.as_deref(), home.as_deref()) {
        if c.is_file() {
            return Some(c);
        }
    }
    on_path().then(|| PathBuf::from("ffmpeg"))
}

/// Is ffmpeg on `PATH`? Answered by running it, which is the only reliable test.
pub fn on_path() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// What to tell someone whose `lumen serve` has no ffmpeg — surfaced in an HLS request's error
/// response and this crate's own logs, never a silent 500 with nothing to act on. Unlike mpv, there
/// is no `lumen setup` fetch path for ffmpeg (yet): this is honest about that rather than pointing at
/// a command that does not do what it would imply.
pub fn install_hint() -> &'static str {
    if cfg!(windows) {
        r"ffmpeg was not found, so HLS segmenting is unavailable (direct playback via /stream/ is
unaffected).

    1. Download a Windows build from https://www.gyan.dev/ffmpeg/builds/ (the release essentials or
       full build) or https://github.com/BtbN/FFmpeg-Builds/releases (the win64-gpl build).
    2. Extract it and copy ffmpeg.exe into this folder, beside lumen.exe.

Or install it system-wide:
    winget install ffmpeg

Or point at a copy you already have, without moving anything:
    set LUMEN_FFMPEG=D:\path\to\ffmpeg.exe"
    } else {
        r"ffmpeg was not found, so HLS segmenting is unavailable (direct playback via /stream/ is
unaffected). Install it with one of:
    apt install ffmpeg      dnf install ffmpeg      pacman -S ffmpeg      zypper install ffmpeg

Or point at a copy you already have:
    export LUMEN_FFMPEG=/path/to/ffmpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundled_ffmpeg_is_preferred_over_whatever_is_installed() {
        let exe_dir = Path::new("/opt/lumen");
        let list = candidates(None, Some(exe_dir), Some(Path::new("/home/u")));
        let beside = list.iter().position(|p| p == &exe_dir.join(FFMPEG_EXE)).expect("beside-exe");
        let system = list
            .iter()
            .position(|p| p.starts_with("/usr") || p.starts_with("C:"))
            .unwrap_or(usize::MAX);
        assert!(beside < system, "bundled must come first: {list:?}");
    }

    #[test]
    fn the_bundle_layouts_a_release_zip_might_use_are_all_searched() {
        let exe_dir = Path::new("/opt/lumen");
        let list = candidates(None, Some(exe_dir), None);
        for expected in [
            exe_dir.join(FFMPEG_EXE),
            exe_dir.join("ffmpeg").join(FFMPEG_EXE),
            exe_dir.join("ffmpeg").join("bin").join(FFMPEG_EXE),
        ] {
            assert!(list.contains(&expected), "{expected:?} missing from {list:?}");
        }
    }

    #[test]
    fn an_explicit_override_beats_everything() {
        let list =
            candidates(Some(Path::new("/custom/ffmpeg")), Some(Path::new("/opt/lumen")), None);
        assert_eq!(list.first(), Some(&PathBuf::from("/custom/ffmpeg")));
    }

    #[test]
    fn the_search_never_returns_an_empty_list() {
        assert!(!candidates(None, None, None).is_empty());
    }

    #[test]
    fn the_executable_name_matches_the_platform() {
        if cfg!(windows) {
            assert_eq!(FFMPEG_EXE, "ffmpeg.exe");
        } else {
            assert_eq!(FFMPEG_EXE, "ffmpeg");
        }
    }

    #[test]
    fn the_install_hint_names_the_override_so_nothing_has_to_be_moved() {
        assert!(install_hint().contains("LUMEN_FFMPEG"), "{}", install_hint());
    }

    #[test]
    fn the_install_hint_is_honest_that_direct_playback_still_works() {
        assert!(install_hint().contains("/stream/"), "{}", install_hint());
    }
}
