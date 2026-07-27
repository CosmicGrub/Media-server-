#!/usr/bin/env bash
# S1 compositing spike — Linux bootstrap.
#
#   ./bootstrap/linux.sh
#
# Installs mpv (the thing being measured) and Rust (which builds the harness), then verifies the two
# things that decide whether a later result means anything: that this mpv has the `gpu-next` video
# output, and that hardware decoding is actually available.
#
# `gpu-next` is not optional. It is the libplacebo renderer the product ships; measuring the older
# `gpu` output measures a different pipeline and answers a different question.

set -uo pipefail

say()  { printf '\033[36m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m%s\033[0m\n' "$*"; }
warn() { printf '  \033[33m! %s\033[0m\n' "$*"; }

say "== S1 bootstrap (Linux) =="

have() { command -v "$1" >/dev/null 2>&1; }

install_mpv() {
    if have apt-get;   then sudo apt-get update && sudo apt-get install -y mpv; return; fi
    if have dnf;       then sudo dnf install -y mpv; return; fi
    if have pacman;    then sudo pacman -S --needed --noconfirm mpv; return; fi
    if have zypper;    then sudo zypper install -y mpv; return; fi
    warn "no known package manager; install mpv by hand and re-run"
}

if have mpv; then
    echo "mpv: already present ($(mpv --version | head -1))"
else
    echo "mpv: installing ..."
    install_mpv
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
        warn "gpu-next video output: MISSING. Distribution mpv packages are sometimes built without"
        warn "libplacebo. A result from the older \`gpu\` output measures a different pipeline than the"
        warn "one that will ship. Build mpv with libplacebo, or use the flatpak (io.mpv.Mpv)."
    fi

    if mpv --hwdec=help 2>/dev/null | grep -Eq 'vaapi|nvdec|vulkan|vdpau'; then
        ok "hardware decoding: available"
    else
        warn "hardware decoding: none reported. Install the VA-API or NVDEC driver for your GPU"
        warn "(mesa-va-drivers / intel-media-va-driver / libva-nvidia-driver). Without it a"
        warn "struggling baseline is software decoding, and says nothing about compositing."
    fi
else
    warn "mpv is still not on PATH; the harness cannot run"
fi

# The compositor matters here in a way it does not on the other platforms. On X11 a compositor with
# unredirect-fullscreen enabled hands mpv the scanout buffer directly, which is a different code path
# from the composited one under test — and it is exactly what the shell will NOT get. Recording which
# session type is in use keeps two runs from being compared across that boundary.
echo
say "== session =="
echo "  session type   ${XDG_SESSION_TYPE:-unknown}"
echo "  desktop        ${XDG_CURRENT_DESKTOP:-unknown}"
if [ "${XDG_SESSION_TYPE:-}" = "x11" ]; then
    warn "X11: if your compositor unredirects fullscreen windows, the baseline stage bypasses"
    warn "compositing entirely while the shell stage does not. That inflates the measured cost."
    warn "Either disable unredirect for the run, or prefer a Wayland session for this spike."
fi

echo
say "== display check =="
echo "  Set the refresh rate to an integer multiple of the clip's frame rate before measuring."
echo "  23.976 fps on a 60 Hz panel judders with no compositor involved, and that judder is easily"
echo "  misread as a compositing failure. Close anything else using the GPU."

echo
say "== next =="
echo "  cargo run -p s1-compositing -- probe"
echo "  cargo run -p s1-compositing -- run --profile spikes/s1-compositing/profiles/desktop.toml --clip <clip.mkv>"
