#!/usr/bin/env python3
"""Measure catalog generation with pinned source data and real Nix metadata."""

import argparse
import hashlib
import json
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def invoke(command):
    started = time.perf_counter()
    result = subprocess.run(
        command, capture_output=True, text=True, check=True, timeout=1800
    )
    return round((time.perf_counter() - started) * 1000, 3), result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cask-catalog", type=Path, required=True)
    parser.add_argument(
        "--formula-cache",
        type=Path,
        help="copy an existing Nix metadata cache for a warm first run",
    )
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    repo = Path(__file__).resolve().parent.parent
    binary = args.cask_catalog.resolve(strict=True)
    report = {
        "complete": False,
        "platform": platform.platform(),
        "runs": args.runs,
        "cask_binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "limits": [
            "Catalog generation uses unchanged maintainer code; these are absolute timings.",
            "The download and first Nix metadata run are single observations.",
            "Repeated generation includes JSON parsing, conversion, file writes and process startup.",
        ],
        "cases": {},
    }
    with tempfile.TemporaryDirectory(prefix="pkg-catalog-speed-") as temporary:
        root = Path(temporary)
        pin = json.loads((repo / "nix/casks/catalog/input.json").read_text())
        fetch = [
            str(binary),
            "fetch",
            "--revision",
            pin["revision"],
            "--sha256",
            pin["sha256"],
            "--cache-dir",
            str(root / "input"),
            "--pin",
            str(root / "cask-pin.json"),
        ]
        elapsed, stdout = invoke(fetch)
        report["cask_fetch"] = {"elapsed_ms": elapsed, "stdout": stdout, "pin": pin}
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        formula_cache = root / "formula-cache"
        if args.formula_cache:
            shutil.copytree(args.formula_cache, formula_cache)
        commands = {
            "cask_generate": [
                str(binary),
                "generate",
                "--pin",
                str(root / "cask-pin.json"),
                "--input",
                str(root / "input" / (pin["revision"] + ".json")),
                "--out-dir",
                str(root / "casks"),
            ],
            "formula_generate": [
                sys.executable,
                str(repo / "tools/formula-catalog/generate.py"),
                "--cache",
                str(formula_cache),
                "--output",
                str(root / "formulae/catalog/catalog.json"),
            ],
        }
        for name, command in commands.items():
            print("Preparing " + name, flush=True)
            elapsed, stdout = invoke(command)
            output = root / (
                "casks/catalog.json"
                if name == "cask_generate"
                else "formulae/catalog/catalog.json"
            )
            expected = hashlib.sha256(output.read_bytes()).hexdigest()
            catalog = json.loads(output.read_bytes())
            count = len(catalog["entries"])
            assert count > 1000, count
            samples = []
            for _ in range(args.runs):
                sample, _ = invoke(command)
                assert hashlib.sha256(output.read_bytes()).hexdigest() == expected, (
                    "catalog output changed"
                )
                samples.append(sample)
            report["cases"][name] = {
                "command": command,
                "input_rows": count,
                "output_bytes": output.stat().st_size,
                "output_sha256": expected,
                "first_ms": elapsed,
                "samples_ms": samples,
                "median_ms": round(statistics.median(samples), 3),
                "min_ms": min(samples),
                "max_ms": max(samples),
                "first_stdout": stdout,
                "first_formula_metadata_cache": "copied"
                if args.formula_cache
                else "empty",
            }
            args.output.write_text(json.dumps(report, indent=2) + "\n")
            print(
                name + ": " + str(report["cases"][name]["median_ms"]) + " ms",
                flush=True,
            )
        report["complete"] = True
        args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
