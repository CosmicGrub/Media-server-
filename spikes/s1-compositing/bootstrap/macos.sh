#!/usr/bin/env bash
# S1 compositing spike — macOS bootstrap.
#
#   ./bootstrap/macos.sh
#
# Installs mpv and Rust via Homebrew, then verifies that this mpv has the `gpu-next` video output —
# the libplacebo renderer the product ships. A result from the older `gpu` output measures a
# different pipeline.

set -uo pipefail

say()  { printf '\033[36m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m%s\033[0m\n' "$*"; }
warn() { printf '  \033[33m! %s\033[0m\n' "$*"; }

say "== S1 bootstrap (macOS) =="

have() { command -v "$1" >/dev/null 2>&1; }

if ! have brew; then
    warn "Homebrew not found. Install it from https://brew.sh and re-run, or install mpv and rustup"
    warn "by hand."
fi

if have mpv; then
    echo "mpv: already present ($(mpv --version | head -1))"
elif have brew; then
    echo "mpv: installing ..."
    brew install mpv
fi

if have cargo; then
    echo "rust: already present ($(cargo --version))"
else
    echo "rust: installing via rustup ..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env" 2>/dev/null || warn "open a new shell so cargo lands on PATH"
fi

echo
say "== verifying =="

if have mpv; then
    if mpv --vo=help 2>/dev/null | grep -q 'gpu-next'; then
        ok "gpu-next video output: present"
    else
        warn "gpu-next video output: MISSING — this build cannot measure the shipping renderer."
    fi
    if mpv --hwdec=help 2>/dev/null | grep -Eq 'videotoolbox'; then
        ok "hardware decoding: VideoToolbox available"
    else
        warn "hardware decoding: VideoToolbox not reported, which is unexpected on macOS."
    fi
else
    warn "mpv is still not on PATH; the harness cannot run"
fi

echo
say "== read this before trusting a macOS result =="
# Two macOS-specific effects will corrupt an S1 measurement if left unrecorded, and neither shows up
# as a late frame.
echo "  1. ProMotion / variable refresh. A MacBook Pro display will drop to a low refresh rate on"
echo "     its own. The harness records the rate it saw at startup, not what the panel did during"
echo "     the run. For a controlled measurement, pin the rate in System Settings > Displays."
echo "  2. Low Power Mode throttles the GPU and is on by default below 20% battery. Check it before"
echo "     an unplugged run, or the battery finding will be about Low Power Mode instead."
echo
echo "  Also: App Nap and Stage Manager both interfere with a background window's frame delivery."
echo "  Keep the player window frontmost and do not switch spaces during a run."

echo
say "== next =="
echo "  cargo run -p s1-compositing -- probe"
echo "  cargo run -p s1-compositing -- run --profile spikes/s1-compositing/profiles/laptop.toml --clip <clip.mkv>"
