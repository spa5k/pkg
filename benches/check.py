#!/usr/bin/env python3
"""Fail G-PERF when Criterion results exceed a ceiling or pinned baseline."""

from __future__ import annotations

import argparse
import json
import math
import platform as host_platform
import sys
from pathlib import Path

SCHEMA_VERSION = 1
EXPECTED_BENCHMARKS = {
    "index_build_tiny",
    "search_ripgrep",
    "info_requests",
}


class GateError(ValueError):
    """A malformed input or failed performance budget."""


def _object(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise GateError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{path} must contain a JSON object")
    return value


def _positive_number(value: object, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise GateError(f"{field} must be a positive number")
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise GateError(f"{field} must be a positive finite number")
    return number


def _validate_baseline(
    document: dict[str, object], platform: str, runner: str
) -> dict[str, object]:
    expected_top = {
        "schemaVersion",
        "platform",
        "maxRegressionPercent",
        "provenance",
        "benchmarks",
    }
    if set(document) != expected_top:
        raise GateError("baseline has missing or unknown top-level fields")
    if document["schemaVersion"] != SCHEMA_VERSION:
        raise GateError("unsupported baseline schemaVersion")
    if document["platform"] != platform:
        raise GateError(
            f"baseline platform {document['platform']!r} does not match {platform!r}"
        )
    regression = _positive_number(
        document["maxRegressionPercent"], "maxRegressionPercent"
    )
    if regression != 25:
        raise GateError("maxRegressionPercent must remain 25; re-baseline needs sign-off")

    provenance = document["provenance"]
    if not isinstance(provenance, dict) or provenance.get("evidenceClass") != "native":
        raise GateError("baseline provenance must declare native evidence")
    required_provenance = {"evidenceClass", "runner", "rust", "collectedAt", "command"}
    if set(provenance) != required_provenance or not all(
        isinstance(provenance[field], str) and provenance[field]
        for field in required_provenance
    ):
        raise GateError("baseline provenance is incomplete or has unknown fields")
    if provenance["runner"] != runner:
        raise GateError(
            f"baseline runner {provenance['runner']!r} does not match {runner!r}"
        )

    benchmarks = document["benchmarks"]
    if not isinstance(benchmarks, dict) or set(benchmarks) != EXPECTED_BENCHMARKS:
        raise GateError("baseline benchmark set must exactly match the V1 benchmark set")
    for name, budget in benchmarks.items():
        if not isinstance(budget, dict) or set(budget) != {
            "baselineNs",
            "absoluteCeilingNs",
        }:
            raise GateError(f"{name}: budget fields are incomplete or unknown")
        _positive_number(budget["baselineNs"], f"{name}.baselineNs")
        _positive_number(budget["absoluteCeilingNs"], f"{name}.absoluteCeilingNs")
    return benchmarks


def evaluate(
    baseline_path: Path, criterion_dir: Path, platform: str, runner: str
) -> list[str]:
    """Return human-readable passing measurements or raise ``GateError``."""
    baseline = _object(baseline_path)
    budgets = _validate_baseline(baseline, platform, runner)
    regression = float(baseline["maxRegressionPercent"])
    reports: list[str] = []
    failures: list[str] = []

    for name in sorted(EXPECTED_BENCHMARKS):
        estimates_path = criterion_dir / name / "new" / "estimates.json"
        estimates = _object(estimates_path)
        try:
            measured = _positive_number(
                estimates["median"]["point_estimate"],  # type: ignore[index]
                f"{name}.median.point_estimate",
            )
        except (KeyError, TypeError) as error:
            raise GateError(f"{estimates_path} lacks Criterion median output") from error

        budget = budgets[name]
        assert isinstance(budget, dict)
        pinned = _positive_number(budget["baselineNs"], f"{name}.baselineNs")
        ceiling = _positive_number(
            budget["absoluteCeilingNs"], f"{name}.absoluteCeilingNs"
        )
        regression_limit = pinned * (1 + regression / 100)
        reports.append(
            f"{name}: {measured:.0f} ns "
            f"(baseline {pinned:.0f}, +25% {regression_limit:.0f}, "
            f"ceiling {ceiling:.0f})"
        )
        if measured > ceiling:
            failures.append(f"{name} exceeds its absolute ceiling")
        if measured > regression_limit:
            failures.append(f"{name} regressed by more than 25%")

    if failures:
        raise GateError("; ".join(failures))
    return reports


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", required=True)
    parser.add_argument("--runner", required=True)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--criterion-dir", type=Path, default=Path("target/criterion"))
    args = parser.parse_args(argv)
    baseline = args.baseline or Path("benches/baselines") / f"{args.platform}.json"
    try:
        reports = evaluate(baseline, args.criterion_dir, args.platform, args.runner)
    except GateError as error:
        print(f"G-PERF failed: {error}", file=sys.stderr)
        return 1
    print(f"G-PERF passed on {args.platform} ({host_platform.machine()})")
    print("\n".join(reports))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
