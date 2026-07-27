#!/usr/bin/env python3
"""Report conformance-corpus coverage: defined vs planned vs target, per group.

Makes gaps visible rather than implicit. A group sitting below target is a known hole in the
compatibility proof from docs/11-13, not a silent omission.

Usage:
    conformance/runner/coverage.py              # table
    conformance/runner/coverage.py --markdown   # table for pasting into docs
    conformance/runner/coverage.py --check      # exit 1 if any blocking vector lacks a source
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "corpus.yaml"
SCHEMA = ROOT / "schema.json"

# Coverage the corpus must reach before the compatibility claim in docs/11 is fully proven.
TARGETS = {
    "mkv-core": 24,
    "mkv-chapters": 12,
    "mkv-damage": 10,
    "mp4-core": 23,
    "mp4-fragmented": 9,
    "mp4-damage": 8,
    "codec-video": 38,
    "codec-audio": 26,
    "subtitles": 14,
    "spectrum": 18,
}


def load_corpus() -> dict:
    try:
        import yaml
    except ImportError:
        sys.exit("pyyaml is required: pip install pyyaml")
    with CORPUS.open() as f:
        return yaml.safe_load(f)


def validate(corpus: dict) -> list[str]:
    """Schema-validate the manifest. A malformed manifest must fail before the runner starts."""
    try:
        import jsonschema
    except ImportError:
        return ["(jsonschema not installed — schema validation skipped)"]
    schema = json.loads(SCHEMA.read_text())
    jsonschema.Draft202012Validator.check_schema(schema)
    validator = jsonschema.Draft202012Validator(schema)
    return [
        f"{'.'.join(str(p) for p in e.path)}: {e.message}"
        for e in sorted(validator.iter_errors(corpus), key=lambda e: list(e.path))
    ]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--markdown", action="store_true", help="emit a markdown table")
    ap.add_argument("--check", action="store_true", help="exit 1 on structural problems")
    args = ap.parse_args()

    corpus = load_corpus()
    vectors = corpus["vectors"]

    problems = validate(corpus)
    hard_errors = [p for p in problems if not p.startswith("(")]

    ids = [v["id"] for v in vectors]
    dupes = [i for i, n in Counter(ids).items() if n > 1]
    if dupes:
        hard_errors.append(f"duplicate vector ids: {dupes}")

    defined = Counter(v["group"] for v in vectors if v.get("status", "defined") == "defined")
    listed = Counter(v["group"] for v in vectors)

    # A defined vector with no reproducible acquisition method cannot be run by CI.
    unsourced = [
        v["id"]
        for v in vectors
        if v.get("status", "defined") == "defined"
        and not any(k in v.get("source", {}) for k in ("file", "generate", "origin", "reuse"))
    ]
    if unsourced:
        hard_errors.append(f"defined vectors with no acquisition method: {unsourced}")

    rows = []
    for group in sorted(TARGETS, key=lambda g: (-TARGETS[g], g)):
        d, li, t = defined[group], listed[group], TARGETS[group]
        rows.append((group, d, li, t, 100.0 * d / t if t else 0.0))

    td, tl, tt = sum(defined.values()), sum(listed.values()), sum(TARGETS.values())

    if args.markdown:
        print("| Group | Defined | Listed | Target | % |")
        print("|---|---:|---:|---:|---:|")
        for g, d, li, t, pct in rows:
            print(f"| `{g}` | {d} | {li} | {t} | {pct:.0f}% |")
        print(f"| **Total** | **{td}** | **{tl}** | **{tt}** | **{100.0 * td / tt:.0f}%** |")
    else:
        print(f"{'group':<18}{'defined':>9}{'listed':>8}{'target':>8}{'':>3}coverage")
        print("-" * 60)
        for g, d, li, t, pct in rows:
            bar = "#" * int(pct / 5) + "." * (20 - int(pct / 5))
            print(f"{g:<18}{d:>9}{li:>8}{t:>8}   {bar} {pct:>3.0f}%")
        print("-" * 60)
        print(f"{'TOTAL':<18}{td:>9}{tl:>8}{tt:>8}   {100.0 * td / tt:>23.0f}%")

    if problems:
        print("\nnotes:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)

    if args.check and hard_errors:
        print(f"\n{len(hard_errors)} structural problem(s) — corpus is not runnable", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
