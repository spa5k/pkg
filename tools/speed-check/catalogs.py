"""Catalog checks: full search preparation, maintainer catalog generation, Formula metadata."""

import gzip
import hashlib
import importlib.util
import json
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from common import (
    REPO,
    Bench,
    balanced,
    digest,
    elapsed_ms,
    native_rows,
    nix_command,
    pinned_catalogs,
    summary,
)

FORMULA_GENERATOR = REPO / "tools/formula-catalog/generate.py"


def register(checks, binaries, report):
    check = checks.add_parser(
        "setup",
        parents=[binaries],
        help="full pinned catalog preparation",
        description="Measure full pinned catalog preparation with a fresh pkg cache per sample.",
    )
    check.add_argument(
        "--update-runs", type=int, default=0, help="measured full updates per setup"
    )
    check.add_argument(
        "--measure-memory",
        action="store_true",
        help="record the largest process RSS from native time; excludes the Nix daemon",
    )
    check.set_defaults(run=setup, runs=4)

    check = checks.add_parser(
        "catalog",
        parents=[report],
        help="maintainer Cask and Formula catalog generation",
        description="Measure catalog generation with pinned source data and real Nix metadata.",
    )
    check.add_argument(
        "--cask-catalog", type=Path, required=True, help="cask-catalog binary"
    )
    check.add_argument(
        "--formula-cache",
        type=Path,
        help="copy an existing Nix metadata cache for a warm first run",
    )
    check.set_defaults(run=catalog, runs=7)

    check = checks.add_parser(
        "formula-metadata",
        parents=[report],
        help="cold Formula metadata generation",
        description=(
            "Measure cold Formula metadata generation and verify the exact catalog bytes. "
            "Use a new cache path for each run. Pass a baseline generator from another checkout."
        ),
    )
    check.add_argument("--generator", type=Path, default=FORMULA_GENERATOR)
    check.add_argument("--cache", type=Path, required=True)
    check.set_defaults(run=formula_metadata, runs=1)


def peak_rss(resources):
    darwin = platform.system() == "Darwin"
    match = re.search(
        r"(?m)^\s*(\d+)\s+maximum resident set size\s*$"
        if darwin
        else r"Maximum resident set size \(kbytes\):\s*(\d+)",
        resources,
    )
    assert match, resources
    return int(match[1]) * (1 if darwin else 1024)


def setup(args):
    if args.update_runs < 0:
        args.error("--update-runs must not be negative")
    with tempfile.TemporaryDirectory(prefix="pkg-real-setup-speed-") as temporary:
        root = Path(temporary)
        bench = Bench(args, root)
        references = pinned_catalogs(root / "real-sources")
        report = bench.report
        report.update(
            scope="full real source preparation; fresh pkg cache and prewarmed source metadata per sample",
            references=references,
            samples=[],
        )
        report["limits"] = [
            "Source metadata prewarming is outside setup timing and is recorded separately.",
            "The source store and operating system file cache are warm.",
            "No profile or background service is changed.",
            "Native rows are hashed after timing and before temporary files are removed.",
        ]
        expected = None
        setups = {label: [] for label in bench.binaries}
        updates = {label: [] for label in bench.binaries}
        try:
            # Every sample uses fresh paths, so the first pair is measured too.
            for iteration, label in balanced(bench.binaries, args.runs - 1):
                folder = root / f"{label}-{iteration}"
                env = bench.environment(folder, references)
                warmups = []
                for reference in references.values():
                    started = time.perf_counter()
                    result = subprocess.run(
                        nix_command("flake", "metadata", reference, "--json"),
                        env=env,
                        capture_output=True,
                        text=True,
                        check=True,
                        timeout=300,
                    )
                    warmups.append(
                        {
                            "reference": reference,
                            "elapsed_ms": elapsed_ms(started),
                            "stderr": result.stderr,
                        }
                    )
                command = [str(bench.binaries[label]), "update", "--scheduled"]
                resources = folder / "time-resources.txt"
                measured = command
                if args.measure_memory:
                    flag = "-l" if platform.system() == "Darwin" else "-v"
                    measured = ["/usr/bin/time", flag, "-o", str(resources), *command]
                started = time.perf_counter()
                result = subprocess.run(
                    measured,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=1800,
                )
                elapsed = elapsed_ms(started)
                sample = {
                    "binary": label,
                    "iteration": iteration,
                    "command": command,
                    "elapsed_ms": elapsed,
                    "code": result.returncode,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "prewarm": warmups,
                }
                report["samples"].append(sample)
                if args.measure_memory:
                    sample["resources"] = resources.read_text()
                    sample["process_max_rss_bytes"] = peak_rss(sample["resources"])
                bench.save()
                assert result.returncode == 0, result.stderr
                sample["native"] = native_rows(env["XDG_CACHE_HOME"])
                expected = expected or sample["native"]
                assert sample["native"] == expected, (
                    label,
                    iteration,
                    sample["native"],
                    expected,
                )
                _, output, _ = bench.invoke(
                    label,
                    ["search", "--source", "nixpkgs", "--json", "--no-fuzzy", "hello"],
                    env,
                )
                assert json.loads(output)["result"]["total"] > 0
                assert bench.query(label, ["list"], env) == []
                setups[label].append(elapsed)
                for update in range(args.update_runs + 1 if args.update_runs else 0):
                    elapsed_update, output, stderr = bench.invoke(
                        label, ["update"], env
                    )
                    report["observations"].append(
                        {
                            "binary": label,
                            "name": "full_catalog_update",
                            "setup_iteration": iteration,
                            "iteration": update,
                            "warmup": update == 0,
                            "elapsed_ms": elapsed_update,
                            "stdout": output,
                            "stderr": stderr,
                        }
                    )
                    assert native_rows(env["XDG_CACHE_HOME"]) == expected
                    assert bench.query(label, ["list"], env) == []
                    if update:
                        updates[label].append(elapsed_update)
                bench.save()
                print(label, iteration, elapsed, sample["native"], flush=True)
                shutil.rmtree(folder)
            bench.record("full_setup", setups)
            if args.update_runs:
                bench.record("full_catalog_update", updates)
            report["complete"] = True
        finally:
            bench.save()


def catalog(args):
    binary = args.cask_catalog.resolve(strict=True)
    report = {
        "check": args.check,
        "complete": False,
        "platform": platform.platform(),
        "runs": args.runs,
        "cask_binary_sha256": digest(binary),
        "limits": [
            "Catalog generation uses unchanged maintainer code; these are absolute timings.",
            "The download and first Nix metadata run are single observations.",
            "Repeated generation includes JSON parsing, conversion, file writes and process startup.",
        ],
        "cases": {},
    }

    def save():
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")

    def invoke(command):
        started = time.perf_counter()
        result = subprocess.run(
            command, capture_output=True, text=True, check=True, timeout=1800
        )
        return elapsed_ms(started), result.stdout

    with tempfile.TemporaryDirectory(prefix="pkg-catalog-speed-") as temporary:
        root = Path(temporary)
        pin = json.loads((REPO / "nix/casks/catalog/input.json").read_text())
        elapsed, stdout = invoke(
            [
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
        )
        report["cask_fetch"] = {"elapsed_ms": elapsed, "stdout": stdout, "pin": pin}
        save()
        formula_cache = root / "formula-cache"
        if args.formula_cache:
            shutil.copytree(args.formula_cache, formula_cache)
        cases = {
            "cask_generate": (
                [
                    str(binary),
                    "generate",
                    "--pin",
                    str(root / "cask-pin.json"),
                    "--input",
                    str(root / "input" / (pin["revision"] + ".json")),
                    "--out-dir",
                    str(root / "casks"),
                ],
                root / "casks/catalog.json",
            ),
            "formula_generate": (
                [
                    sys.executable,
                    str(FORMULA_GENERATOR),
                    "--cache",
                    str(formula_cache),
                    "--output",
                    str(root / "formulae/catalog/catalog.json"),
                ],
                root / "formulae/catalog/catalog.json",
            ),
        }
        for name, (command, output) in cases.items():
            print("Preparing " + name, flush=True)
            first, stdout = invoke(command)
            expected = digest(output)
            count = len(json.loads(output.read_bytes())["entries"])
            assert count > 1000, count
            samples = []
            for _ in range(args.runs):
                samples.append(invoke(command)[0])
                assert digest(output) == expected, "catalog output changed"
            report["cases"][name] = {
                "command": command,
                "input_rows": count,
                "output_bytes": output.stat().st_size,
                "output_sha256": expected,
                "first_ms": first,
                **summary(samples),
                "first_stdout": stdout,
                "first_formula_metadata_cache": "copied"
                if args.formula_cache
                else "empty",
            }
            save()
            print(f"{name}: {report['cases'][name]['median_ms']} ms", flush=True)
        report["complete"] = True
        save()


def formula_metadata(args):
    if args.cache.exists():
        args.error("--cache must be a new path")
    generator = args.generator.resolve(strict=True)
    spec = importlib.util.spec_from_file_location("formula", generator)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    raw = gzip.decompress(
        (REPO / "tools/formula-catalog/inputs/formula.json.gz").read_bytes()
    )
    input_sha256 = hashlib.sha256(raw).hexdigest()
    pinned = json.loads((REPO / "tools/formula-catalog/inputs/input.json").read_text())
    assert input_sha256 == pinned["sha256"]
    formulae = json.loads(raw)
    nix = nix_command()[0]
    started = time.perf_counter()
    metadata = {
        system: module.native_metadata(formulae, system, nix, args.cache)
        for system in module.SYSTEMS
    }
    catalog_bytes = (
        json.dumps(
            module.generate(formulae, metadata, input_sha256),
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode()
    elapsed = elapsed_ms(started)
    assert catalog_bytes == (REPO / "nix/formulae/catalog/catalog.json").read_bytes(), (
        "generated catalog bytes changed"
    )
    report = {
        "check": args.check,
        "elapsed_ms": elapsed,
        "generator_sha256": digest(generator),
        "metadata_sha256": hashlib.sha256(
            json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "catalog_sha256": hashlib.sha256(catalog_bytes).hexdigest(),
        "metadata_rows": {system: len(rows) for system, rows in metadata.items()},
        "input_sha256": input_sha256,
        "matches_committed_bytes": True,
        "cache_before": "absent",
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)
