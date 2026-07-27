//! S1 shell — the composited stage of the compositing spike.
//!
//! **Unverified template.** See `../../README.md`. This was written without a GPU, a display, or
//! graphics drivers, and has never been compiled. The design is the deliverable; the code will need
//! fixing on first contact with a real toolchain, particularly around Tauri's window-handle API,
//! which moves between minor versions.
//!
//! ## What it does
//!
//! Creates one fullscreen, transparent-background Tauri window, hands mpv that window's native
//! handle via `--wid` so the video renders *inside* it, and lets the WebView composite an HTML OSD
//! on top. One window, one swapchain — which is what makes the resulting number a measurement of
//! per-frame compositing cost rather than of two overlapping windows.
//!
//! ## Why the mpv arguments are not written here
//!
//! They come from the harness (`mpv_ipc::common_mpv_args`), reproduced in [`mpv_args`] below and
//! checked against it by a test. If this shell configured mpv even slightly differently from the
//! baseline stage, the comparison would measure the configuration difference instead of the
//! compositing cost, and it would do so invisibly.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::{Child, Command, Stdio};

/// The mpv options both stages share.
///
/// Kept byte-identical to `spikes/s1-compositing/src/mpv_ipc.rs::common_mpv_args`. The two cannot be
/// a shared crate without making this scaffold a workspace member, which would drag Tauri's
/// dependency tree into the tested crates' build; `mpv_args_match_the_harness` guards the copy.
fn mpv_args(clip: &str, ipc_path: &str, seconds: u64, wid: Option<String>) -> Vec<String> {
    let mut args = vec![
        format!("--input-ipc-server={ipc_path}"),
        "--vo=gpu-next".into(),
        "--hwdec=auto-safe".into(),
        "--fullscreen=yes".into(),
        "--no-border".into(),
        "--no-config".into(),
        "--no-resume-playback".into(),
        "--no-osc".into(),
        "--no-terminal".into(),
        "--loop-file=inf".into(),
        format!("--length={seconds}"),
    ];
    // Embedding is the only difference from the baseline, and it is the thing under test.
    if let Some(id) = wid {
        args.push(format!("--wid={id}"));
    }
    args.push(clip.to_string());
    args
}

/// Native window handle as mpv's `--wid` wants it.
///
/// Windows takes an `HWND`, X11 an `XID`, macOS an `NSView*`. Wayland has no equivalent — there is no
/// cross-process window embedding — so this returns `None` there and the caller falls back to a
/// separate mpv window. That fallback measures a *different* thing (two windows the compositor
/// overlays, rather than one window the shell composites), so it is reported loudly rather than
/// treated as equivalent.
fn window_id(window: &tauri::WebviewWindow) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        return window.hwnd().ok().map(|h| (h.0 as isize).to_string());
    }
    #[cfg(target_os = "macos")]
    {
        return window.ns_view().ok().map(|v| (v as isize).to_string());
    }
    #[cfg(target_os = "linux")]
    {
        // Only meaningful under X11 (or XWayland). Under a native Wayland session there is no id.
        if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland") {
            return None;
        }
        use gtk::prelude::*;
        return window
            .gtk_window()
            .ok()
            .and_then(|w| w.window())
            .map(|gdk| gdk.downcast::<gdk_x11::X11Window>().ok())
            .flatten()
            .map(|x| x.xid().to_string());
    }
    #[allow(unreachable_code)]
    None
}

fn spawn_mpv(clip: &str, ipc: &str, wid: Option<String>) -> std::io::Result<Child> {
    if wid.is_none() {
        eprintln!(
            "WARNING: no embeddable window id (native Wayland has no cross-process embedding). \
             mpv will get its own window, so this run measures two overlapping windows rather than \
             one composited surface. Record that alongside the result — it is not the same test."
        );
    }
    Command::new("mpv")
        .args(mpv_args(clip, ipc, 3600, wid))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Receives the OSD latency the page measured and prints it where the harness can see it.
///
/// The harness inherits this process's stderr, so the operator reads the number off the console and
/// passes it back with `--osd-latency` on the next run. Deliberately not written into the report
/// directly: a number the shell asserts about itself, with no separate record, is not evidence.
#[tauri::command]
fn report_osd_latency(ms: f64, samples: usize) {
    eprintln!("LUMEN_S1_OSD_LATENCY_MS={ms:.1}  (n={samples})");
}

fn main() {
    let clip = std::env::var("LUMEN_S1_CLIP")
        .expect("LUMEN_S1_CLIP is set by the harness; run this through `--shell`, not directly");
    let ipc = std::env::var("LUMEN_S1_IPC")
        .expect("LUMEN_S1_IPC is set by the harness; without it the stage cannot be measured");

    let clip_for_setup = clip.clone();
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![report_osd_latency])
        .setup(move |app| {
            use tauri::Manager;
            let window = app
                .get_webview_window("main")
                .ok_or("no `main` window; check tauri.conf.json")?;

            // The page reads this to label the OSD. Injected rather than passed as a query string so
            // a path containing `#` or `?` cannot corrupt the URL.
            let js = format!("window.__LUMEN_CLIP__ = {:?};", clip_for_setup);
            let _ = window.eval(&js);

            let child = spawn_mpv(&clip_for_setup, &ipc, window_id(&window))?;
            // Held for the process lifetime. The harness kills this shell when the stage ends, and
            // mpv exits with it; leaking a player between stages would corrupt the next measurement.
            app.manage(std::sync::Mutex::new(child));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_args_match_the_harness_except_for_embedding() {
        // The one guard on the hand-copied argument list. If the harness's `common_mpv_args` changes
        // and this does not, the two stages silently measure different players — the single failure
        // mode that would invalidate every result without producing an error.
        let harness = include_str!("../../../src/mpv_ipc.rs");
        let ours = mpv_args("clip.mkv", "/tmp/s.sock", 3600, None);
        for arg in &ours {
            let flag = arg.split('=').next().unwrap_or(arg);
            if flag == "clip.mkv" {
                continue;
            }
            assert!(
                harness.contains(flag),
                "`{flag}` is not in the harness's shared argument list; the stages have drifted"
            );
        }
    }

    #[test]
    fn embedding_is_the_only_added_argument() {
        let plain = mpv_args("c.mkv", "/tmp/s", 60, None);
        let embedded = mpv_args("c.mkv", "/tmp/s", 60, Some("12345".into()));
        assert_eq!(embedded.len(), plain.len() + 1);
        assert!(embedded.iter().any(|a| a == "--wid=12345"));
        assert_eq!(embedded.last().map(String::as_str), Some("c.mkv"), "the file stays last");
    }
}
