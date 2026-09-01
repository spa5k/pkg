#!/usr/bin/env python3
"""Compares measured quality debt against the per-file baseline.

Reads one compact diagnostic object per line on stdin (report.py output).

The baseline maps {lint: {file: count}}. Three guarantees, weakest first:

1. Global ratchet: no lint total may grow beyond the baseline total.
2. Per-file ratchet: no file may grow beyond its baseline for any lint.
   This makes "new debt is forbidden" precise: existing debt is tolerated
   exactly where it already lives; adding one more site anywhere fails.
3. Touched-files rule (FULL_TOUCHED=1): every file changed against the base
   ref must carry zero diagnostics. The aspirational mode; run with
   `just lint-strict` and enable in CI once the touched-file debt is paid.
"""
import json
import os
import sys

baseline_path = sys.argv[1]
baseline = json.load(open(baseline_path))
changed = set(os.environ.get("CHANGED_FILES", "").splitlines())
full_touched = os.environ.get("FULL_TOUCHED") == "1"

current_lints = {}
current_files = {}
for line in sys.stdin:
    item = json.loads(line)
    lint = item["lint"]
    current_lints[lint] = current_lints.get(lint, 0) + 1
    per = current_files.setdefault(lint, {})
    per[item["file"]] = per.get(item["file"], 0) + 1

problems = 0
for lint, per_file in sorted(current_files.items()):
    base_per = baseline.get(lint, {})
    base_total = sum(base_per.values())
    total = sum(per_file.values())
    if total > base_total:
        problems += 1
        print(f"RATCHET: {lint} grew {base_total} -> {total}", file=sys.stderr)
    for file, count in sorted(per_file.items()):
        base = base_per.get(file, 0)
        if count > base:
            problems += 1
            print(f"RATCHET-FILE: {file} grew {lint} {base} -> {count}", file=sys.stderr)

touched_bad = 0
if full_touched:
    for lint, per_file in sorted(current_files.items()):
        for file, count in sorted(per_file.items()):
            if file in changed and count > 0:
                touched_bad += 1
                print(f"TOUCHED {file} {lint}: {count} site(s)", file=sys.stderr)

total = sum(current_lints.values())
print(f"quality: {total} debt sites across {len(current_lints)} lints",
      file=sys.stderr)
print(f"ratchet: {'FAIL' if problems else 'ok'}", file=sys.stderr)
if full_touched:
    print(f"touched files: {'FAIL' if touched_bad else 'ok'} ({len(changed)} changed)",
          file=sys.stderr)
if problems or touched_bad:
    sys.exit(1)
