#!/usr/bin/env bash
# Blocking CI gate for ADR-0002: the native AV stack must stay LGPL-only and dynamically linked.
#
# One `--enable-gpl` (for libx264/libx265) makes the entire combined work GPL, which forecloses App
# Store distribution and any non-GPL licensing of our own code, permanently. Catching that in CI is a
# one-line check; discovering it after shipping is a rewrite.
#
# Usage:
#   ci/license-gate.sh                      # scan build recipes only
#   FFMPEG_BIN=/path/to/ffmpeg ci/license-gate.sh   # also verify a produced binary
#
# Exit codes: 0 clean, 1 violation found, 2 could not run a required check.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILURES=0
CHECKS=0

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
info()  { printf '  %s\n' "$*"; }

fail() { red "FAIL  $*"; FAILURES=$((FAILURES + 1)); }
pass() { green "ok    $*"; }
check() { CHECKS=$((CHECKS + 1)); }

# Configure flags that pull in GPL-only or non-distributable code.
#   --enable-gpl        : whole build becomes GPLv2+
#   --enable-version3   : GPLv3/LGPLv3, additional app-store friction
#   --enable-nonfree    : result may not be distributed at all
#   libx264 / libx265   : GPL encoders. Not needed — hardware encoders (NVENC/QSV/VAAPI/AMF/
#                         VideoToolbox) and permissive software encoders (SVT-AV1, libaom, libvpx)
#                         cover the transcoding requirement. See docs/13 §3.1.
#   libfdk-aac          : non-distributable
BANNED_FLAGS='--enable-gpl|--enable-version3|--enable-nonfree|--enable-libx264|--enable-libx265|--enable-libfdk-aac'

# mpv's GPL build. `-Dgpl=false` is what makes an LGPLv2.1+ libmpv, which ADR-0002 requires.
BANNED_MPV='-Dgpl=true|--enable-gpl'

echo "== license gate (ADR-0002) =="

# ── 1. Build recipes must not request GPL or non-free components ─────────────────────────────────
# Scan only files that feed a build, and only their effective (non-comment) lines. Prose that
# *names* a banned flag in order to forbid it — this script, and native/README.md — must not trip
# the gate, or the rule becomes undocumentable.
scan_recipes() {
  local pattern="$1" label="$2" hits=""
  while IFS= read -r -d '' file; do
    local matched
    matched=$(sed -e 's/[[:space:]]*#.*$//' "$file" | grep -nE -- "$pattern" || true)
    [[ -n "$matched" ]] && hits+="$(sed "s|^|      ${file#"$ROOT"/}:|" <<<"$matched")"$'\n'
  done < <(find "$ROOT/native" -type f \( -name '*.config' -o -name '*.sh' -o -name '*.cmake' \
             -o -name 'meson*' -o -name 'Makefile*' \) -print0 2>/dev/null)

  if [[ -n "$hits" ]]; then
    fail "$label"
    printf '%s' "$hits"
  else
    pass "$label — none found"
  fi
}

check
if [[ -d "$ROOT/native" ]]; then
  scan_recipes "$BANNED_FLAGS" "GPL/non-free configure flags in build recipes"
else
  info "native/ not present; skipping recipe scan"
fi

check
if [[ -d "$ROOT/native" ]]; then
  scan_recipes "$BANNED_MPV" "GPL mpv configuration (ADR-0002 requires -Dgpl=false)"
fi

# ── 2. A produced binary must report LGPL ────────────────────────────────────────────────────────
# Recipes can be right while a vendored or system FFmpeg is GPL, so verify the artifact when one is
# available. Skipped rather than failed when absent, so the gate still runs on doc-only changes.
if [[ -n "${FFMPEG_BIN:-}" ]]; then
  check
  if [[ ! -x "$FFMPEG_BIN" ]]; then
    red "FAIL  FFMPEG_BIN=$FFMPEG_BIN is not executable"
    exit 2
  fi
  version_output="$("$FFMPEG_BIN" -version 2>&1)"

  if grep -Eq 'enable-gpl|enable-nonfree|enable-version3' <<<"$version_output"; then
    fail "built FFmpeg reports a GPL/non-free configuration"
    grep -Eo -- '--enable-(gpl|nonfree|version3)' <<<"$version_output" | sort -u | sed 's/^/      /'
  else
    pass "built FFmpeg configuration is clean"
  fi

  check
  if grep -q 'libavutil *license: *LGPL' <<<"$version_output"; then
    pass "libavutil reports LGPL"
  else
    fail "libavutil does not report LGPL"
    grep -i 'license' <<<"$version_output" | sed 's/^/      /' || true
  fi

  # LGPL §6 requires users be able to relink against their own build, which static linking defeats.
  check
  if command -v ldd >/dev/null 2>&1; then
    if ldd "$FFMPEG_BIN" 2>/dev/null | grep -qE 'libav(codec|format|util)'; then
      pass "FFmpeg libraries are dynamically linked"
    else
      fail "FFmpeg libraries appear statically linked (LGPL §6 requires relinkability)"
    fi
  else
    info "ldd unavailable; skipping dynamic-linking check"
  fi
else
  info "FFMPEG_BIN unset; skipping built-artifact verification"
fi

# ── 3. Rust dependency licences ──────────────────────────────────────────────────────────────────
check
if command -v cargo-deny >/dev/null 2>&1 || cargo deny --version >/dev/null 2>&1; then
  if (cd "$ROOT" && cargo deny check licenses 2>&1 | tail -20); then
    pass "cargo-deny licence policy satisfied"
  else
    fail "cargo-deny found a licence policy violation"
  fi
else
  info "cargo-deny not installed; skipping (install with: cargo install cargo-deny)"
fi

echo
if [[ $FAILURES -gt 0 ]]; then
  red "license gate FAILED — $FAILURES of $CHECKS checks"
  red "See docs/adr/0002-lgpl-only-build.md and docs/08-legal-licensing.md §1."
  exit 1
fi
green "license gate passed — $CHECKS checks"
