#!/usr/bin/env python3
"""Compare large local catalogs without retaining all fixture or output rows in RAM."""

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import sqlite3
import statistics
import subprocess
import tempfile
import time
import traceback
from pathlib import Path

SOURCE = "github:NixOS/nixpkgs/nixpkgs-unstable"
WORDS = [
    "archive",
    "browser",
    "compiler",
    "database",
    "editor",
    "format",
    "graphics",
    "http",
    "image",
    "json",
    "kernel",
    "library",
    "mail",
    "network",
    "office",
    "parser",
    "query",
    "runtime",
    "server",
    "tool",
]
FAMILIES = ["python314Packages", "nodePackages", "haskellPackages", "rustPackages", ""]


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            value.update(block)
    return value.hexdigest()


def summary(values):
    return {
        "samples_ms": values,
        "median_ms": round(statistics.median(values), 3),
        "mean_ms": round(statistics.mean(values), 3),
        "min_ms": min(values),
        "max_ms": max(values),
    }


def prepare(root, count, unique):
    arch = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
    system = f"{arch}-{'darwin' if platform.system() == 'Darwin' else 'linux'}"
    cache = root / "cache/pkg"
    cache.mkdir(parents=True)
    key = 0xCBF29CE484222325
    for byte in ("native-sqlite-1" + SOURCE + system).encode():
        key = ((key ^ byte) * 0x1000000001B3) & ((1 << 64) - 1)
    database = cache / f"catalog-{key:016x}.sqlite"
    tools = 1
    with sqlite3.connect(database) as connection:
        connection.executescript("""
            PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
            PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;
            CREATE TABLE metadata(value TEXT NOT NULL);
            CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL,
                pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
        """)
        rows = []
        for index in range(count):
            first, second = WORDS[index % 20], WORDS[(index // 20) % 20]
            name = f"{first}-{second}-{index:08d}"
            exposed = f"{FAMILIES[index % 5]}.{name}".lstrip(".")
            rows.append(
                (
                    f"legacyPackages.{system}.{exposed}",
                    exposed,
                    name,
                    "1.2.3",
                    f"{first.capitalize()} utilities for {second} development and data processing",
                )
            )
            tools += first == "tool" or second == "tool"
            if len(rows) == 4096:
                connection.executemany("INSERT INTO packages VALUES (?,?,?,?,?)", rows)
                rows.clear()
        connection.executemany("INSERT INTO packages VALUES (?,?,?,?,?)", rows)
        connection.executemany(
            "INSERT INTO packages VALUES (?,?,?,?,?)",
            [
                (
                    f"legacyPackages.{system}.ripgrep",
                    "ripgrep",
                    "ripgrep",
                    "15.2.0",
                    "A fast line-oriented search tool",
                ),
                (
                    f"legacyPackages.{system}.gnugrep",
                    "gnugrep",
                    "grep",
                    "3.11",
                    "Print lines matching a pattern",
                ),
            ],
        )
        connection.execute(
            "INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed"
        )
        assert connection.execute(
            "SELECT count(*),sum(n>1) FROM claims"
        ).fetchone() == (count + 2, 0)
        header = {
            "schema": 1,
            "source": SOURCE,
            "system": system,
            "reference": "github:NixOS/nixpkgs/" + "a" * 40,
            "revision": "a" * 40,
            "complete": True,
            "saved_unix": int(time.time()),
        }
        if unique:
            header["unique_exposed"] = True
        connection.execute("INSERT INTO metadata VALUES (?)", [json.dumps(header)])
    return database, system, tools


def output_proof(path, total):
    size = path.stat().st_size
    with path.open("rb") as stream:
        prefix = stream.read(min(size, 65536))
        stream.seek(-4, os.SEEK_END)
        assert stream.read() == b"]}}\n"
    matched = re.search(
        rb'"total":(\d+),"approximate":(true|false),"results":\[', prefix
    )
    assert matched and int(matched.group(1)) == total, prefix[:2048]
    parsed = None
    if size <= 4 * 1024 * 1024:
        parsed = json.loads(path.read_bytes())["result"]
        assert parsed["total"] == total
        assert all(source["status"] == "cached" for source in parsed["sources"])
    return {
        "bytes": size,
        "sha256": digest(path),
        "total": total,
        "approximate": matched.group(2) == b"true",
        "parsed_results": None if parsed is None else len(parsed["results"]),
    }


def run(args, root):
    args.output.parent.mkdir(parents=True, exist_ok=True)
    binaries = {
        "baseline": args.baseline.resolve(strict=True),
        "candidate": args.pkg.resolve(strict=True),
    }
    report = {
        "complete": False,
        "driver_sha256": digest(Path(__file__)),
        "platform": platform.platform(),
        "rows": args.rows + 2,
        "runs": args.runs,
        "binaries": {
            label: {"path": str(path), "sha256": digest(path)}
            for label, path in binaries.items()
        },
        "cases": {},
        "limits": [
            "Synthetic local SQLite fixture. No Nix process or network request is allowed during queries.",
            "Fixture generation and output hashing run outside query timing. Rows are generated in bounded batches.",
            "Every measured and resource stdout is written to a private file and hashed before deletion.",
            "Large full outputs are checked against the ordinary baseline hash and envelope total; they are not parsed in RAM.",
            "The elapsed time includes process startup and writing stdout to a regular file. It excludes hashing and validation.",
            "The input can exceed the host page cache. Do not treat these measurements as entirely resident-memory scans.",
            "One excluded pair precedes alternating measured pairs. Resource passes run separately.",
        ],
    }

    def save():
        args.output.write_text(json.dumps(report, indent=2) + "\n")

    assert (
        report["binaries"]["baseline"]["sha256"]
        != report["binaries"]["candidate"]["sha256"]
    )
    required = args.rows * (1500 if args.full_output else 400) + 512 * 1024 * 1024
    assert shutil.disk_usage(root).free >= required, (
        f"need at least {required} free bytes"
    )
    try:
        started = time.monotonic()
        database, system, tools = prepare(root, args.rows, args.unique_exposed)
        report["fixture"] = {
            "path": str(database),
            "bytes": database.stat().st_size,
            "sha256": digest(database),
            "generation_ms": round(1000 * (time.monotonic() - started), 3),
            "python_sqlite_version": sqlite3.sqlite_version,
            "unique_exposed_metadata": args.unique_exposed,
        }
        temporary = root / "tmp"
        temporary.mkdir(mode=0o700)
        env = dict(
            os.environ,
            HOME=str(root),
            XDG_CACHE_HOME=str(root / "cache"),
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_STATE_HOME=str(root / "state"),
            TMPDIR=str(temporary),
            PATH="",
            NO_COLOR="1",
        )
        resource_tool = None
        if args.resources and platform.system() == "Linux":
            spec = importlib.util.spec_from_file_location(
                "search_resources", Path(__file__).with_name("search-speed-check.py")
            )
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            resource_tool = module.linux_resource_probe(root)
            control = module.resources(
                Path("/bin/sh"), ["-c", "true"], env, b"", resource_tool
            )
            assert control["peak_rss_bytes"] < 8 * 1024 * 1024
            report["resource_probe_control"] = control
        cases = {
            "literal": (["ripgrep", "--limit", "20"], 1),
            "literal_no_fuzzy": (["ripgrep", "--no-fuzzy", "--limit", "20"], 1),
            "typo": (["ripgrpe", "--limit", "20"], 1),
            "broad": (["tool", "--broad", "--limit", "20"], tools),
            "regex": (["ripgrep|gnugrep", "--regex", "--limit", "20"], 2),
            "dense_limited": ([".", "--regex", "--limit", "20"], args.rows + 2),
        }
        if args.full_output:
            cases["dense_full"] = ([".", "--regex", "--all"], args.rows + 2)
        save()
        for name, (words, total) in cases.items():
            command = ["search", *words, "--source", "nixpkgs", "--json"]
            case = {
                "command": command,
                "observations": [],
                "resources": {label: [] for label in binaries},
            }
            report["cases"][name] = case
            samples = {label: [] for label in binaries}
            expected = None

            def observe(
                label,
                expected,
                resource=False,
                *,
                command=command,
                name=name,
                total=total,
            ):
                output = root / "stdout.json"
                errors = root / "stderr.txt"
                native = [str(binaries[label]), *command]
                if resource and resource_tool:
                    native.insert(0, str(resource_tool))
                assert shutil.disk_usage(root).free >= (
                    args.rows * 1000 if name == "dense_full" else 128 * 1024 * 1024
                )
                with output.open("wb") as stdout, errors.open("wb") as stderr:
                    start = time.perf_counter()
                    if resource and platform.system() == "Darwin":
                        process = subprocess.Popen(
                            native, env=env, stdout=stdout, stderr=stderr
                        )
                        deadline = time.monotonic() + 1800
                        while True:
                            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                            if pid:
                                process.returncode = os.waitstatus_to_exitcode(status)
                                break
                            if time.monotonic() > deadline:
                                process.kill()
                                process.wait()
                                raise TimeoutError(native)
                            time.sleep(0.01)
                        code = process.returncode
                        resources = {
                            "user_cpu_ms": round(usage.ru_utime * 1000, 3),
                            "system_cpu_ms": round(usage.ru_stime * 1000, 3),
                            "peak_rss_bytes": usage.ru_maxrss,
                        }
                    else:
                        result = subprocess.run(
                            native,
                            env=env,
                            stdout=stdout,
                            stderr=stderr,
                            timeout=1800,
                            check=False,
                        )
                        code = result.returncode
                        resources = None
                    elapsed = round(1000 * (time.perf_counter() - start), 3)
                error = errors.read_text(errors="replace")
                assert code == 0, (name, label, code, error)
                proof = output_proof(output, total)
                output.unlink()
                if resource_tool and resource:
                    resources = json.loads(error.rsplit("\nPKG_RESOURCE:", 1)[1])
                assert expected is None or proof == expected, (
                    name,
                    label,
                    proof,
                    expected,
                )
                return elapsed, proof, error, resources

            for iteration in range(args.runs + 1):
                order = list(binaries)
                if iteration % 2:
                    order.reverse()
                for position, label in enumerate(order):
                    elapsed, proof, error, _ = observe(label, expected)
                    if expected is None:
                        expected = proof
                        case["output"] = proof
                        assert proof["approximate"] == (name == "typo")
                    case["observations"].append(
                        {
                            "binary": label,
                            "iteration": iteration,
                            "excluded": iteration == 0,
                            "position": position,
                            "elapsed_ms": elapsed,
                            "stderr": error,
                        }
                    )
                    if iteration:
                        samples[label].append(elapsed)
                    save()
            case.update({label: summary(values) for label, values in samples.items()})
            case["paired_delta_ms"] = summary(
                [
                    round(c - b, 3)
                    for b, c in zip(samples["baseline"], samples["candidate"])
                ]
            )
            if args.resources:
                for label in binaries:
                    for _ in range(3):
                        _, _, _, resources = observe(label, expected, resource=True)
                        case["resources"][label].append(resources)
                        save()
            print(
                name,
                case["baseline"]["median_ms"],
                case["candidate"]["median_ms"],
                flush=True,
            )
        assert digest(database) == report["fixture"]["sha256"]
        assert not list(temporary.iterdir()), "search left temporary files"
        report["system"] = system
        report["complete"] = True
        save()
    except BaseException:
        report["failure"] = traceback.format_exc()
        save()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--pkg", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--rows", type=int, default=1_000_000)
    parser.add_argument("--runs", type=int, default=8)
    parser.add_argument("--resources", action="store_true")
    parser.add_argument("--full-output", action="store_true")
    parser.add_argument("--unique-exposed", action="store_true")
    args = parser.parse_args()
    if args.rows < 1000 or args.runs <= 0:
        parser.error("rows must be at least 1000 and runs must be positive")
    if platform.system() not in ("Darwin", "Linux"):
        parser.error("this benchmark supports macOS and Linux")
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=False)
        run(args, args.work_dir)
    else:
        with tempfile.TemporaryDirectory(prefix="pkg-search-scale-") as temporary:
            run(args, Path(temporary))


if __name__ == "__main__":
    main()
