//! Finding mpv.
//!
//! mpv is a separate process, not a library linked into this binary, so "where is it" is a real
//! question with a wrong answer that looks like a missing feature. The search order below is what
//! makes a portable folder work: drop `mpv.exe` beside `lumen.exe` and the pair runs from a USB
//! stick with nothing installed.
//!
//! Beside-the-executable is checked **before** `PATH` deliberately. A user who put a specific mpv
//! next to this binary chose that one; silently preferring an older system install would be the
//! wrong answer and an invisible one — the version difference only shows up as a file that fails to
//! play.

use std::path::{Path, PathBuf};

/// The executable name on this platform.
pub const MPV_EXE: &str = if cfg!(windows) { "mpv.exe" } else { "mpv" };

/// Where to look, in order. Split out from the filesystem check so the ordering is testable.
///
/// Every input is passed in rather than read from the environment here: a test that had to set
/// `LUMEN_MPV` would be mutating process-wide state shared with every other test in the binary.
pub fn candidates(
    override_path: Option<&Path>,
    exe_dir: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // 1. An explicit override always wins. The escape hatch for when the search order is wrong on a
    //    particular machine, usable without a code change.
    if let Some(p) = override_path {
        out.push(p.to_path_buf());
    }

    // 2. Beside the executable, or in an `mpv` folder next to it. This is what makes the release
    //    bundle self-contained.
    if let Some(dir) = exe_dir {
        out.push(dir.join(MPV_EXE));
        out.push(dir.join("mpv").join(MPV_EXE));
        out.push(dir.join("mpv").join("bin").join(MPV_EXE));
    }

    // 3. Wherever the platform normally puts it. Checked after the bundle so a deliberate local copy
    //    is never overridden by whatever happens to be installed.
    if cfg!(windows) {
        for var in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                out.push(base.join("Programs").join("mpv").join(MPV_EXE));
                out.push(base.join("mpv").join(MPV_EXE));
                out.push(base.join("mpv.net").join(MPV_EXE));
            }
        }
        if let Some(h) = home {
            // scoop and chocolatey, the two package managers most likely to have installed it.
            out.push(h.join("scoop").join("apps").join("mpv").join("current").join(MPV_EXE));
            out.push(h.join("scoop").join("shims").join(MPV_EXE));
        }
        out.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin").join(MPV_EXE));
    } else {
        for dir in ["/usr/local/bin", "/usr/bin", "/snap/bin"] {
            out.push(Path::new(dir).join(MPV_EXE));
        }
        // The flatpak is a wrapper rather than a binary, so it is not listed: launching it would
        // work but the sandbox cannot see the user's media paths, which fails confusingly.
    }
    out
}

/// The mpv this run should use, or `None` if there is none.
///
/// Returns a bare `mpv` when only `PATH` has it, so the OS does the lookup — resolving it here would
/// mean reimplementing `PATHEXT` handling on Windows for no benefit.
pub fn find() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let home = home_dir();
    let over =
        std::env::var_os("LUMEN_MPV").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
    for c in candidates(over.as_deref(), exe_dir.as_deref(), home.as_deref()) {
        if c.is_file() {
            return Some(c);
        }
    }
    on_path().then(|| PathBuf::from("mpv"))
}

/// Is mpv on `PATH`? Answered by running it, which is the only reliable test.
pub fn on_path() -> bool {
    std::process::Command::new("mpv")
        .arg("--version")
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

/// Ask a specific mpv for a list — `--vo=help`, `--hwdec=help` — as bare identifiers.
///
/// Deduplicated, order preserved. `--hwdec=help` prints one line per decoder *and codec* pair, so a
/// modern build answers with over a hundred entries of which a dozen are distinct. Printing the raw
/// list buries the useful answer in repetition.
pub fn list(mpv: &Path, arg: &str) -> Vec<String> {
    let raw = std::process::Command::new(mpv)
        .arg(arg)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let mut seen = std::collections::HashSet::new();
    raw.lines()
        .skip(1) // the header line
        .filter_map(|l| l.split_whitespace().next())
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .filter(|s| seen.insert(s.to_string()))
        .map(str::to_string)
        .collect()
}

pub fn version(mpv: &Path) -> Option<String> {
    let out = std::process::Command::new(mpv).arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| {
            String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string()
        })
        .filter(|s| !s.is_empty())
}

/// What to tell someone who has no mpv.
///
/// Raw strings, not `\`-continued ones: a continuation strips the leading whitespace of the next
/// line, which silently flattens the numbered steps into an unreadable block. The indentation here
/// is the formatting.
pub fn install_hint() -> &'static str {
    if cfg!(windows) {
        r"mpv was not found.

Easiest fix — let this program fetch it:
    lumen setup

Or do it by hand, which always works:
    1. Download a Windows build from https://mpv.io/installation/
       (the shinchiro builds; take the x86_64 one)
    2. Extract it and copy mpv.exe into this folder, beside lumen.exe.

Or install it system-wide:
    winget install mpv.net

Or point at a copy you already have, without moving anything:
    set LUMEN_MPV=D:\path\to\mpv.exe"
    } else {
        r"mpv was not found. Install it with one of:
    apt install mpv      dnf install mpv      pacman -S mpv      zypper install mpv

Or point at a copy you already have:
    export LUMEN_MPV=/path/to/mpv"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundled_mpv_is_preferred_over_whatever_is_installed() {
        // The whole point of the portable folder: a user who put a specific mpv next to this binary
        // chose that one. Preferring a system install would be wrong *and* invisible — the version
        // difference only surfaces as a file that mysteriously fails to play.
        let exe_dir = Path::new("/opt/lumen");
        let list = candidates(None, Some(exe_dir), Some(Path::new("/home/u")));
        let beside = list.iter().position(|p| p == &exe_dir.join(MPV_EXE)).expect("beside-exe");
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
            exe_dir.join(MPV_EXE),
            exe_dir.join("mpv").join(MPV_EXE),
            exe_dir.join("mpv").join("bin").join(MPV_EXE),
        ] {
            assert!(list.contains(&expected), "{expected:?} missing from {list:?}");
        }
    }

    #[test]
    fn an_explicit_override_beats_everything() {
        let list = candidates(Some(Path::new("/custom/mpv")), Some(Path::new("/opt/lumen")), None);
        assert_eq!(list.first(), Some(&PathBuf::from("/custom/mpv")));
    }

    #[test]
    fn the_search_never_returns_an_empty_list() {
        // Even with nothing known about the machine there are platform defaults to try, and an empty
        // list would turn "not installed" into "did not look".
        assert!(!candidates(None, None, None).is_empty());
    }

    #[test]
    fn the_executable_name_matches_the_platform() {
        if cfg!(windows) {
            assert_eq!(MPV_EXE, "mpv.exe");
        } else {
            assert_eq!(MPV_EXE, "mpv");
        }
    }

    #[test]
    fn duplicate_entries_are_collapsed_but_order_is_kept() {
        // `--hwdec=help` prints one line per decoder *and* codec pair — a modern build answers with
        // over a hundred entries of which a dozen are distinct. The raw list buries the answer.
        let mut seen = std::collections::HashSet::new();
        let parsed: Vec<String> =
            "Available:\n  d3d11va (h264)\n  d3d11va (hevc)\n  vulkan (av1)\n  nvdec (h264)\n"
                .lines()
                .skip(1)
                .filter_map(|l| l.split_whitespace().next())
                .filter(|s| !s.is_empty() && !s.starts_with('-'))
                .filter(|s| seen.insert(s.to_string()))
                .map(str::to_string)
                .collect();
        assert_eq!(parsed, vec!["d3d11va", "vulkan", "nvdec"]);
    }

    #[test]
    fn the_install_hint_names_the_override_so_nothing_has_to_be_moved() {
        assert!(install_hint().contains("LUMEN_MPV"), "{}", install_hint());
    }
}
