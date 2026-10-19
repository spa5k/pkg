#!/usr/bin/env python3
"""Measure full pinned catalog preparation with a fresh pkg cache per sample."""

import argparse
import hashlib
import importlib.util
import json
import pathlib
import platform
import re
import shutil
import sqlite3
import subprocess
import tempfile
import time

REVISION = "8edc0c72e3a38faf5434e40d1f19431125fe4b30"


def native_rows(cache):
    databases = list(cache.rglob("catalog-*.sqlite"))
    assert len(databases) == 1, databases
    digest = hashlib.sha256()
    count = 0
    with sqlite3.connect(
        "file:" + str(databases[0]) + "?mode=ro", uri=True
    ) as connection:
        rows = connection.execute(
            "SELECT attribute, exposed, pname, version, description FROM packages ORDER BY attribute"
        )
        for row in rows:
            digest.update(
                json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode()
            )
            digest.update(b"\n")
            count += 1
    assert count > 10000, count
    return {"rows": count, "canonical_rows_sha256": digest.hexdigest()}


def pinned_catalogs(repo, destination):
    destination.mkdir()
    for part in ["casks", "formulae"]:
        shutil.copytree(repo / "nix" / part, destination / part)
    commands = [
        ["git", "init", "--quiet", "-b", "main", str(destination)],
        ["git", "-C", str(destination), "add", "."],
        [
            "git",
            "-C",
            str(destination),
            "-c",
            "user.name=pkg benchmark",
            "-c",
            "user.email=benchmark@example.com",
            "commit",
            "--quiet",
            "-m",
            "Pinned sources",
        ],
    ]
    for command in commands:
        subprocess.run(command, check=True)
    return {
        "nixpkgs": "github:NixOS/nixpkgs/" + REVISION,
        **{
            part: "git+file://" + str(destination) + "?dir=" + part
            for part in ["casks", "formulae"]
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, required=True)
    parser.add_argument("--pkg", type=pathlib.Path, required=True)
    parser.add_argument("--baseline", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument(
        "--runs", type=int, default=4, help="measured pairs; even counts balance order"
    )
    parser.add_argument("--update-runs", type=int, default=0)
    parser.add_argument(
        "--measure-memory",
        action="store_true",
        help="record the largest process RSS from native time; excludes the Nix daemon",
    )
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if args.update_runs < 0:
        parser.error("--update-runs must not be negative")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location(
        "speed", args.repo / "tools/pkg-speed-check.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    with tempfile.TemporaryDirectory(prefix="pkg-real-setup-speed-") as temporary:
        root = pathlib.Path(temporary)
        bench = module.Bench(args, root)
        references = pinned_catalogs(args.repo, root / "real-sources")
        report = bench.report
        report["scope"] = (
            "full real source preparation; fresh pkg cache and prewarmed source metadata per sample"
        )
        report["references"] = references
        report["catalog_sha256"] = {
            part: hashlib.sha256(
                (root / "real-sources" / part / "catalog/catalog.json").read_bytes()
            ).hexdigest()
            for part in ["casks", "formulae"]
        }
        report["samples"] = []
        report["limits"] = [
            "Source metadata prewarming is outside setup timing and is recorded separately.",
            "The source store and operating system file cache are warm.",
            "No profile or background service is changed.",
            "Native rows are hashed after timing and before temporary files are removed.",
        ]
        expected = None
        values = {label: [] for label in bench.binaries}
        updates = {label: [] for label in bench.binaries}
        nix = [
            shutil.which("nix"),
            "--extra-experimental-features",
            "nix-command flakes",
            "--option",
            "accept-flake-config",
            "false",
            "--option",
            "allow-import-from-derivation",
            "false",
        ]
        try:
            for iteration in range(args.runs):
                order = list(bench.binaries)
                if iteration % 2:
                    order.reverse()
                for label in order:
                    folder = root / f"{label}-{iteration}"
                    env = bench.environment(folder)
                    config = pathlib.Path(env["XDG_CONFIG_HOME"]) / "pkg/config.toml"
                    config.write_text(
                        "[sources]\n"
                        + "".join(
                            name + " = " + json.dumps(reference) + "\n"
                            for name, reference in references.items()
                        )
                    )
                    warmups = []
                    for reference in references.values():
                        started = time.perf_counter()
                        result = subprocess.run(
                            nix + ["flake", "metadata", reference, "--json"],
                            env=env,
                            capture_output=True,
                            text=True,
                            check=True,
                            timeout=300,
                        )
                        warmups.append(
                            {
                                "reference": reference,
                                "elapsed_ms": round(
                                    1000 * (time.perf_counter() - started), 3
                                ),
                                "stderr": result.stderr,
                            }
                        )
                    command = [str(bench.binaries[label]), "setup", "--no-background"]
                    memory_file = folder / "time-resources.txt"
                    measured = command
                    if args.measure_memory:
                        measured = [
                            "/usr/bin/time",
                            "-l" if platform.system() == "Darwin" else "-v",
                            "-o",
                            str(memory_file),
                            *command,
                        ]
                    started = time.perf_counter()
                    result = subprocess.run(
                        measured,
                        env=env,
                        capture_output=True,
                        text=True,
                        check=False,
                        timeout=1800,
                    )
                    elapsed = round(1000 * (time.perf_counter() - started), 3)
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
                        resources = memory_file.read_text()
                        match = re.search(
                            r"(?m)^\s*(\d+)\s+maximum resident set size\s*$"
                            if platform.system() == "Darwin"
                            else r"Maximum resident set size \(kbytes\):\s*(\d+)",
                            resources,
                        )
                        assert match, resources
                        sample["resources"] = resources
                        sample["process_max_rss_bytes"] = int(match[1]) * (
                            1 if platform.system() == "Darwin" else 1024
                        )
                    bench.save()
                    assert result.returncode == 0, result.stderr
                    sample["native"] = native_rows(pathlib.Path(env["XDG_CACHE_HOME"]))
                    if expected is None:
                        expected = sample["native"]
                    assert sample["native"] == expected, (
                        label,
                        iteration,
                        sample["native"],
                        expected,
                    )
                    _, output, _ = bench.invoke(
                        label,
                        [
                            "search",
                            "--source",
                            "nixpkgs",
                            "--json",
                            "--no-fuzzy",
                            "hello",
                        ],
                        env=env,
                    )
                    assert json.loads(output)["result"]["total"] > 0
                    assert bench.query(label, ["list"], env=env) == []
                    values[label].append(elapsed)
                    if args.update_runs:
                        for update in range(args.update_runs + 1):
                            elapsed_update, output, stderr = bench.invoke(
                                label, ["update"], env=env
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
                            assert (
                                native_rows(pathlib.Path(env["XDG_CACHE_HOME"]))
                                == expected
                            )
                            assert bench.query(label, ["list"], env=env) == []
                            if update:
                                updates[label].append(elapsed_update)
                    bench.save()
                    print(label, iteration, elapsed, sample["native"], flush=True)
                    shutil.rmtree(folder)
            report["cases"]["full_setup"] = {
                label: module.summary(samples) for label, samples in values.items()
            }
            if args.update_runs:
                report["cases"]["full_catalog_update"] = {
                    label: module.summary(samples) for label, samples in updates.items()
                }
            report["complete"] = True
        finally:
            bench.save()


if __name__ == "__main__":
    main()
