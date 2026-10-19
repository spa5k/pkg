#!/usr/bin/env python3
"""Measure local search with a temporary catalog and compare two pkg binaries."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import sqlite3
import statistics
import subprocess
import tempfile
import time
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


def prepare(root, count):
    """Write the same complete SQLite snapshot shape as native discovery."""
    arch = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
    system = f"{arch}-{'darwin' if platform.system() == 'Darwin' else 'linux'}"
    cache = root / "cache/pkg"
    cache.mkdir(parents=True)
    key = 0xCBF29CE484222325
    for byte in ("native-sqlite-1" + SOURCE + system).encode():
        key = ((key ^ byte) * 0x1000000001B3) & ((1 << 64) - 1)
    with sqlite3.connect(cache / f"catalog-{key:016x}.sqlite") as connection:
        connection.executescript("""
            CREATE TABLE metadata(value TEXT NOT NULL);
            CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL,
                pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
        """)
        header = {
            "schema": 1,
            "source": SOURCE,
            "system": system,
            "reference": "github:NixOS/nixpkgs/" + "a" * 40,
            "revision": "a" * 40,
            "complete": True,
            "saved_unix": int(time.time()),
        }
        connection.execute("INSERT INTO metadata VALUES (?)", [json.dumps(header)])
        rows = []
        tools = 1
        for index in range(count):
            first, second = WORDS[index % 20], WORDS[(index // 20) % 20]
            name = f"{first}-{second}-{index:06d}"
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
        rows.extend(
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
            ]
        )
        connection.executemany("INSERT INTO packages VALUES (?,?,?,?,?)", rows)
        connection.execute(
            "INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed"
        )
    return tools


def invoke(binary, command, env):
    start = time.perf_counter()
    result = subprocess.run(
        [str(binary), *command], env=env, capture_output=True, check=True
    )
    return (time.perf_counter() - start) * 1000, result.stdout


def linux_resource_probe(root):
    """Fork from a small process so Linux RSS does not inherit Python's peak."""
    source = root / "resource-probe.c"
    source.write_text(r"""
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc < 2) return 126;
    pid_t child = fork();
    if (child < 0) return 126;
    if (!child) { execv(argv[1], argv + 1); _exit(127); }
    int status;
    struct rusage usage;
    while (wait4(child, &status, 0, &usage) < 0)
        if (errno != EINTR) return 126;
    fprintf(stderr, "\nPKG_RESOURCE:{\"user_cpu_ms\":%.3f,\"system_cpu_ms\":%.3f,\"peak_rss_bytes\":%lld}\n",
        usage.ru_utime.tv_sec * 1000.0 + usage.ru_utime.tv_usec / 1000.0,
        usage.ru_stime.tv_sec * 1000.0 + usage.ru_stime.tv_usec / 1000.0,
        (long long)usage.ru_maxrss * 1024);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
}
""")
    probe = root / "resource-probe"
    subprocess.run(
        [shutil.which("cc") or "cc", str(source), "-o", str(probe)], check=True
    )
    return probe


def resources(binary, command, env, expected, probe=None):
    """Collect child CPU and peak RSS in a separate, untimed pass."""
    if probe:
        result = subprocess.run(
            [str(probe), str(binary), *command],
            env=env,
            capture_output=True,
            check=True,
            timeout=180,
        )
        if result.stdout != expected:
            raise RuntimeError("resource sample output differs")
        return json.loads(result.stderr.rsplit(b"\nPKG_RESOURCE:", 1)[1])
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        child = subprocess.Popen(
            [str(binary), *command], env=env, stdout=output, stderr=errors
        )
        deadline = time.monotonic() + 180
        while True:
            pid, status, usage = os.wait4(child.pid, os.WNOHANG)
            if pid:
                child.returncode = os.waitstatus_to_exitcode(status)
                break
            if time.monotonic() >= deadline:
                child.kill()
                child.wait()
                raise TimeoutError("resource sample exceeded 180 seconds")
            time.sleep(0.001)
        output.seek(0)
        errors.seek(0)
        if child.returncode or output.read() != expected:
            raise RuntimeError(
                f"resource sample failed or output differs: {errors.read()!r}"
            )
    return {
        "user_cpu_ms": round(usage.ru_utime * 1000, 3),
        "system_cpu_ms": round(usage.ru_stime * 1000, 3),
        "peak_rss_bytes": usage.ru_maxrss
        * (1 if platform.system() == "Darwin" else 1024),
    }


def measure(args, root):
    tool_count = prepare(root, args.packages)
    probe = (
        linux_resource_probe(root)
        if args.resources and platform.system() == "Linux"
        else None
    )
    env = dict(
        os.environ,
        HOME=str(root),
        XDG_CACHE_HOME=str(root / "cache"),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_STATE_HOME=str(root / "state"),
        PATH="",
    )
    common = ["--source", "nixpkgs", "--json"]
    cases = {
        "literal": (["ripgrep", "--limit", "20"], 1),
        "literal_no_fuzzy": (["ripgrep", "--limit", "20", "--no-fuzzy"], 1),
        "typo": (["ripgrpe", "--limit", "20"], 1),
        "broad": (["tool", "--broad", "--limit", "20"], tool_count),
        "regex": (["ripgrep|gnugrep", "--regex", "--limit", "20"], 2),
        "all_results": (["tool", "--broad", "--all"], tool_count),
    }
    if args.dense_output:
        cases["regex_all_results"] = ([".", "--regex", "--all"], args.packages + 2)
    binaries = {"candidate": args.pkg.resolve(strict=True)}
    if args.baseline:
        binaries = {"baseline": args.baseline.resolve(strict=True), **binaries}
    digests = {
        key: hashlib.sha256(path.read_bytes()).hexdigest()
        for key, path in binaries.items()
    }
    if args.baseline and digests["baseline"] == digests["candidate"]:
        raise RuntimeError(
            "baseline and candidate are identical binaries; rebuild in separate target directories"
        )
    report = {
        "platform": platform.platform(),
        "packages": args.packages + 2,
        "runs": args.runs,
        "binaries": {
            key: {"path": str(path), "sha256": digests[key]}
            for key, path in binaries.items()
        },
        "cases": {},
    }
    if probe:
        control = resources(Path("/bin/sh"), ["-c", "true"], env, b"", probe)
        if control["peak_rss_bytes"] > 8 * 1024 * 1024:
            raise RuntimeError("resource probe includes launcher memory")
        report["resource_probe_control"] = control
    for name, (options, expected) in cases.items():
        command = ["search", *options, *common]
        samples = {key: [] for key in binaries}
        output = None
        for run in range(args.runs + 1):
            # Alternate order to reduce drift between the two binaries.
            order = list(binaries.items())
            if run % 2:
                order.reverse()
            for label, binary in order:
                elapsed, stdout = invoke(binary, command, env)
                data = json.loads(stdout)["result"]
                if data["total"] != expected:
                    raise RuntimeError(
                        f"{name}: expected {expected} results, got {data['total']}"
                    )
                if (
                    name in ("literal", "literal_no_fuzzy", "typo")
                    and data["results"][0]["id"] != "nixpkgs:ripgrep"
                ):
                    raise RuntimeError(f"{name}: wrong first result")
                if output is not None and stdout != output:
                    raise RuntimeError(
                        f"{name}: output differs between runs or binaries"
                    )
                output = stdout
                if run:
                    samples[label].append(elapsed)
        case = {
            "command": command,
            "total": expected,
            "sha256": hashlib.sha256(output).hexdigest(),
        }
        for label, values in samples.items():
            case[label] = {
                "samples_ms": [round(value, 3) for value in values],
                "median_ms": round(statistics.median(values), 3),
                "min_ms": round(min(values), 3),
                "max_ms": round(max(values), 3),
            }
        if args.baseline:
            case["latency_reduction_percent"] = round(
                100
                * (1 - case["candidate"]["median_ms"] / case["baseline"]["median_ms"]),
                1,
            )
        if args.resources:
            case["resources"] = {
                label: [
                    resources(binary, command, env, output, probe) for _ in range(3)
                ]
                for label, binary in binaries.items()
            }
        report["cases"][name] = case
        print(f"{name}: {json.dumps(case)}", flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pkg", required=True, type=Path, help="release binary to measure"
    )
    parser.add_argument(
        "--baseline", type=Path, help="release binary to compare; outputs must match"
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=8,
        help="measured pairs after one warmup; even counts balance order",
    )
    parser.add_argument(
        "--packages",
        type=int,
        default=100000,
        help="generated records, plus two named packages",
    )
    parser.add_argument("--output", type=Path, help="save the measurements as JSON")
    parser.add_argument(
        "--resources",
        action="store_true",
        help="also record three separate CPU and peak RSS samples",
    )
    parser.add_argument(
        "--dense-output",
        action="store_true",
        help="also emit every generated record through a regex search",
    )
    args = parser.parse_args()
    if args.runs < 1 or args.packages < 1:
        parser.error("--runs and --packages must be positive")
    if platform.system() not in ("Darwin", "Linux"):
        parser.error("this benchmark supports macOS and Linux")
    with tempfile.TemporaryDirectory(prefix="pkg-search-speed-") as temporary:
        report = measure(args, Path(temporary))
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
