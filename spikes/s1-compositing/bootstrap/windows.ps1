# S1 compositing spike — Windows bootstrap.
#
# Installs what the harness needs and nothing else. Run from an ordinary PowerShell prompt:
#
#   powershell -ExecutionPolicy Bypass -File bootstrap\windows.ps1
#
# What it installs, and why each one is required rather than convenient:
#
#   mpv    — the thing being measured. Must be a build with the `gpu-next` video output; that is
#            the libplacebo renderer the product ships, and a measurement of the older `gpu` vo
#            measures a different pipeline.
#   Rust   — builds the harness. The harness has zero crate dependencies, so a stock toolchain and
#            no network access beyond this script is enough.
#
# It deliberately does NOT install Tauri or Node. Those belong to the shell in ui/, which is a
# separate decision — the baseline stage is worth running on its own first.

$ErrorActionPreference = 'Stop'

function Test-Cmd($name) { $null -ne (Get-Command $name -ErrorAction SilentlyContinue) }

Write-Host "== S1 bootstrap (Windows) ==" -ForegroundColor Cyan

# winget ships with Windows 11 and recent Windows 10. Without it, install by hand from the links
# printed below — the script will not silently do nothing.
$haveWinget = Test-Cmd winget

if (Test-Cmd mpv) {
    Write-Host "mpv: already present ($( (mpv --version | Select-Object -First 1) ))"
} elseif ($haveWinget) {
    Write-Host "mpv: installing via winget ..."
    winget install --id mpv.net --accept-source-agreements --accept-package-agreements
} else {
    Write-Warning "mpv not found and winget is unavailable. Install a recent build from https://mpv.io/installation/ and re-run."
}

if (Test-Cmd cargo) {
    Write-Host "rust: already present ($( (cargo --version) ))"
} elseif ($haveWinget) {
    Write-Host "rust: installing via winget ..."
    winget install --id Rustlang.Rustup --accept-source-agreements --accept-package-agreements
    Write-Warning "Open a new shell so cargo lands on PATH, then re-run this script."
} else {
    Write-Warning "cargo not found and winget is unavailable. Install from https://rustup.rs and re-run."
}

# --- Verification -------------------------------------------------------------------------------
# Installing is not the goal; being able to measure is. These checks are the ones that decide whether
# a later result means anything.

Write-Host ""
Write-Host "== verifying ==" -ForegroundColor Cyan

if (Test-Cmd mpv) {
    $vos = (mpv --vo=help) -join "`n"
    if ($vos -match 'gpu-next') {
        Write-Host "  gpu-next video output: present" -ForegroundColor Green
    } else {
        Write-Warning "  gpu-next video output: MISSING. This mpv build cannot measure the renderer the product ships. Get a build from https://mpv.io/installation/ (the shinchiro Windows builds include it)."
    }

    $hw = (mpv --hwdec=help) -join "`n"
    if ($hw -match 'd3d11va|nvdec|vulkan') {
        Write-Host "  hardware decoding: available" -ForegroundColor Green
    } else {
        Write-Warning "  hardware decoding: none reported. A struggling baseline would be software decoding, not compositing."
    }
} else {
    Write-Warning "  mpv is still not on PATH; the harness cannot run."
}

Write-Host ""
Write-Host "== display check ==" -ForegroundColor Cyan
Write-Host "Set the display to a refresh rate that is an integer multiple of your test clip's frame"
Write-Host "rate BEFORE measuring. 23.976 fps content on a 60 Hz panel judders on its own, and that"
Write-Host "judder looks exactly like a compositing failure. 120 Hz or 144 Hz suits film content."
Write-Host ""
Write-Host "Also: set the GPU driver's power mode to 'Prefer maximum performance' for the run, and"
Write-Host "close anything else using the GPU. A browser playing video in the background is a"
Write-Host "measurement of your browser, not of this harness."

Write-Host ""
Write-Host "== next ==" -ForegroundColor Cyan
Write-Host "  cargo run -p s1-compositing -- probe"
Write-Host "  cargo run -p s1-compositing -- run --profile spikes\s1-compositing\profiles\desktop.toml --clip <clip.mkv>"
