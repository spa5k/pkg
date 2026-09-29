#!/usr/bin/env python3
"""Offline checker: compare one actual raw-tap catalog with expectations.

Reads the expected record for one source from public-sources.json and one
actual schema-3 catalog produced by the real importer (the parent runs the
import). Without network, Ruby, or import execution it verifies, over ALL
catalog entries (not only this source's rows):

Raw-tap catalog contract (final backend contract):

- raw input: inputs[source].kind equals "raw-ruby-tap"; catalog.targets
  is exactly [system] (a raw tap catalog lists only the imported native
  system; the ordinary official JSON catalog keeps both targets and is
  out of scope here); every entry's targets map has exactly that one
  native key;
- provenance: inputs[source].url equals the canonical approved repository
  URL (https://github.com/<owner>/<repo>); inputs[source].raw.archiveUrl
  equals the exact codeload URL for the pinned revision; raw.archiveSha256
  and raw.captureSha256 are recorded as 64-hex values. Actual provenance
  is recorded as evidence; url and archiveUrl are checked independently,
  never assumed equal;
- input revision: inputs[source].revision equals the pinned expected
  40-hex revision exactly;
- inventory: the exact token set equals expected_tokens; a same-count
  substituted token fails;
- structure: every entry row is a dict; entries under any other source
  or non-dict rows are errors; every key equals source/token with a
  nonempty token; every origin.path equals Casks/token.rb;
- status: the single native target has status "eligible" or "excluded";
  "excluded" must carry a nonempty reason string;
- selected source-derived status expectations (record-actual entries are
  recorded, not enforced).

This file is a checker test harness input consumer, not import proof:
imports stay UNRUN until the parent runs them. Negative-catalog checker
tests are documented in README.md.

Actual statuses, reasons, and provenance are emitted as JSON evidence.
Exit code is 1 on any problem.

Usage: check_sources.py --source S --system SYSTEM --catalog FILE
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCHEMA = "pkg-cask-catalog/3"
KIND = "raw-ruby-tap"
STATUSES = {"eligible", "excluded"}
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")


def expected_record(matrix: dict, source: str) -> dict:
    for record in matrix["sources"]:
        if record["source"] == source:
            return record
    raise SystemExit(f"FAIL: source not in public-sources.json: {source}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Offline actual-catalog checker")
    parser.add_argument(
        "--source", required=True, help="owner/tap from public-sources.json"
    )
    parser.add_argument(
        "--system", required=True, help="aarch64-darwin or x86_64-linux"
    )
    parser.add_argument(
        "--catalog", type=Path, required=True, help="actual schema-3 catalog.json"
    )
    args = parser.parse_args()

    ps = json.loads((HERE / "public-sources.json").read_text(encoding="utf-8"))
    if args.system not in ps["policy"]["targets"]:
        raise SystemExit(f"FAIL: unknown system: {args.system}")
    exp = expected_record(ps, args.source)
    repository = exp["repository"]
    if not HEX40.fullmatch(exp.get("revision", "")):
        raise SystemExit(f"FAIL: expected revision is not 40-hex: {exp['source']}")

    catalog = json.loads(args.catalog.read_text(encoding="utf-8"))
    if catalog.get("schema") != SCHEMA:
        raise SystemExit(
            f"FAIL: catalog schema {catalog.get('schema')!r} != {SCHEMA!r}"
        )
    entries = catalog.get("entries")
    if not isinstance(entries, dict):
        raise SystemExit("FAIL: catalog has no entries object")

    problems: list[str] = []

    # Raw-tap catalog targets: exactly the one imported native system.
    catalog_targets = catalog.get("targets")
    if catalog_targets != [args.system]:
        problems.append(
            f"targets: catalog targets {catalog_targets!r} != [{args.system!r}]"
        )

    # Validate every catalog entry, not only rows for this source.
    mine: dict[str, dict] = {}
    for key, row in entries.items():
        if not isinstance(row, dict):
            problems.append(f"structure: entry {key!r} is not an object")
            continue
        token = row.get("token", "")
        if not isinstance(token, str) or not token:
            problems.append(f"identity: entry {key!r} has no nonempty token")
            continue
        if key != f"{row.get('source')}/{token}":
            problems.append(f"identity: key {key!r} != {row.get('source')!r}/{token!r}")
        origin = row.get("origin")
        path = origin.get("path") if isinstance(origin, dict) else None
        if path != f"Casks/{token}.rb":
            problems.append(f"origin: {key} path {path!r} != Casks/{token}.rb")
        row_targets = row.get("targets")
        if not isinstance(row_targets, dict) or set(row_targets) != {args.system}:
            problems.append(
                f"targets: {key} target keys {sorted(row_targets or [])!r} "
                f"!= [{args.system!r}]"
            )
        if row.get("source") != args.source:
            problems.append(f"wrongsource: entry {key!r} is not from {args.source}")
            continue
        mine[key] = row

    inputs = catalog.get("inputs")
    src_input = inputs.get(args.source) if isinstance(inputs, dict) else None
    if not isinstance(src_input, dict):
        raise SystemExit(f"FAIL: catalog inputs has no object for {args.source}")

    kind = src_input.get("kind")
    if kind != KIND:
        problems.append(f"kind: input kind {kind!r} != {KIND!r}")

    expected_revision = exp["revision"]
    actual_revision = src_input.get("revision")
    if actual_revision != expected_revision:
        problems.append(
            f"revision: actual {actual_revision!r} != expected {expected_revision!r}"
        )

    # Provenance: canonical repo URL and exact codeload archive URL are
    # checked independently; url is never assumed to equal archiveUrl.
    canonical_url = f"https://github.com/{repository}"
    actual_url = src_input.get("url")
    if actual_url != canonical_url:
        problems.append(
            f"provenance: url {actual_url!r} != canonical repo URL {canonical_url!r}"
        )
    raw = src_input.get("raw")
    if not isinstance(raw, dict):
        problems.append("provenance: inputs[source].raw is not an object")
        raw = {}
    expected_archive_url = (
        f"https://codeload.github.com/{repository}/tar.gz/{expected_revision}"
    )
    actual_archive_url = raw.get("archiveUrl")
    if actual_archive_url != expected_archive_url:
        problems.append(
            f"provenance: raw.archiveUrl {actual_archive_url!r} != exact codeload "
            f"URL {expected_archive_url!r}"
        )
    for field in ("archiveSha256", "captureSha256"):
        value = raw.get(field)
        if not (isinstance(value, str) and HEX64.fullmatch(value)):
            problems.append(f"provenance: raw.{field} is not a recorded 64-hex value")

    expected_tokens = exp["expected_tokens"]
    actual_tokens = sorted(r["token"] for r in mine.values())
    if set(actual_tokens) != set(expected_tokens):
        problems.append(
            f"inventory: tokens {actual_tokens} != expected_tokens {expected_tokens}"
        )
    if len(actual_tokens) != exp["expected_cask_count"]:
        problems.append(f"inventory: count {len(actual_tokens)} != expected count")

    evidence_rows = []
    for key, row in sorted(mine.items()):
        token = row["token"]
        target = row.get("targets", {}).get(args.system)
        status = target.get("status") if isinstance(target, dict) else None
        reason = target.get("reason") if isinstance(target, dict) else None
        if status not in STATUSES:
            problems.append(f"status: {token} invalid status {status!r}")
        elif status == "excluded" and not (isinstance(reason, str) and reason.strip()):
            problems.append(f"status: {token} excluded without reason string")
        evidence_rows.append(
            {"identity": key, "token": token, "status": status, "reason": reason}
        )

    named = {e["token"]: e for e in exp.get("entries", [])}
    for token, entry in sorted(named.items()):
        row = next((r for r in evidence_rows if r["token"] == token), None)
        if row is None:
            problems.append(f"missing: expected entry {token} absent from catalog")
            continue
        want = entry.get("expect", {}).get(args.system)
        if want in STATUSES and row["status"] != want:
            problems.append(f"status: {token} expected {want}, actual {row['status']}")

    evidence = {
        "source": args.source,
        "system": args.system,
        "catalog_schema": catalog.get("schema"),
        "catalog_targets": catalog_targets,
        "input_kind": kind,
        "actual_revision": actual_revision,
        "expected_revision": expected_revision,
        "actual_url": actual_url,
        "actual_raw": raw,
        "expected_count": exp["expected_cask_count"],
        "actual_count": len(mine),
        "rows": evidence_rows,
        "problems": problems,
    }
    print(json.dumps(evidence, indent=2, sort_keys=True))
    if problems:
        for problem in problems:
            print(f"FAIL: {problem}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
