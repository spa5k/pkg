"""Compare local search on a generated native catalog of any size.

Fixture rows are written in bounded batches, and each stdout goes to a private
file that is hashed and then deleted, so large catalogs never sit in RAM. No
Nix process or network request runs during queries. Elapsed time includes
process startup and the stdout write; hashing and validation are excluded.
"""

import json
import os
import platform
import re
import shutil
import sqlite3
import subprocess
import tempfile
import time
import traceback
from pathlib import Path

from common import (
    balanced,
    compare,
    compile_c,
    digest,
    elapsed_ms,
    host_system,
    summary,
)

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
NAMED = [
    ("ripgrep", "ripgrep", "15.2.0", "A fast line-oriented search tool"),
    ("gnugrep", "grep", "3.11", "Print lines matching a pattern"),
]
RESOURCE_PROBE = r"""
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
"""


def register(checks, binaries):
    check = checks.add_parser(
        "search", parents=[binaries], help="local catalog search", description=__doc__
    )
    check.add_argument(
        "--rows",
        type=int,
        default=100_000,
        help="generated rows, plus two named packages",
    )
    check.add_argument(
        "--work-dir", type=Path, help="new directory to keep the fixture in"
    )
    check.add_argument(
        "--resources",
        action="store_true",
        help="also record three CPU and peak RSS samples",
    )
    check.add_argument(
        "--full-output",
        action="store_true",
        help="also print every row through a regex search",
    )
    check.add_argument(
        "--unique-exposed",
        action="store_true",
        help="mark the fixture's exposed names as unique",
    )
    check.set_defaults(run=run)


def prepare(root, count, unique):
    """Write the same complete SQLite snapshot shape as native discovery."""
    system = host_system()
    cache = root / "cache/pkg"
    cache.mkdir(parents=True)
    key = 0xCBF29CE484222325
    for byte in ("native-sqlite-1" + SOURCE + system).encode():
        key = ((key ^ byte) * 0x1000000001B3) & ((1 << 64) - 1)
    database = cache / f"catalog-{key:016x}.sqlite"
    tools = 1
    insert = "INSERT INTO packages VALUES (?,?,?,?,?)"
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
            description = f"{first.capitalize()} utilities for {second} development and data processing"
            rows.append(
                (
                    f"legacyPackages.{system}.{exposed}",
                    exposed,
                    name,
                    "1.2.3",
                    description,
                )
            )
            tools += first == "tool" or second == "tool"
            if len(rows) == 4096:
                connection.executemany(insert, rows)
                rows.clear()
        rows += [
            (f"legacyPackages.{system}.{exposed}", exposed, *rest)
            for exposed, *rest in NAMED
        ]
        connection.executemany(insert, rows)
        connection.execute(
            "INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed"
        )
        claims = connection.execute(
            "SELECT count(*), sum(n > 1) FROM claims"
        ).fetchone()
        assert claims == (count + len(NAMED), 0), claims
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


def output_proof(path, total, first):
    """Check the JSON envelope without parsing large outputs in RAM."""
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
        assert first is None or parsed["results"][0]["id"] == first, parsed["results"][
            :1
        ]
    return {
        "bytes": size,
        "sha256": digest(path),
        "total": total,
        "approximate": matched.group(2) == b"true",
        "parsed_results": None if parsed is None else len(parsed["results"]),
    }


def wait_with_usage(process, timeout):
    deadline = time.monotonic() + timeout
    while True:
        pid, status, usage = os.wait4(process.pid, os.WNOHANG)
        if pid:
            process.returncode = os.waitstatus_to_exitcode(status)
            return {
                "user_cpu_ms": round(usage.ru_utime * 1000, 3),
                "system_cpu_ms": round(usage.ru_stime * 1000, 3),
                "peak_rss_bytes": usage.ru_maxrss,
            }
        if time.monotonic() > deadline:
            process.kill()
            process.wait()
            raise TimeoutError(process.args)
        time.sleep(0.01)


def run(args):
    if args.rows < 1000:
        args.error("--rows must be at least 1000")
    if platform.system() not in ("Darwin", "Linux"):
        args.error("this benchmark supports macOS and Linux")
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=False)
        measure(args, args.work_dir)
    else:
        with tempfile.TemporaryDirectory(prefix="pkg-search-speed-") as temporary:
            measure(args, Path(temporary))


def measure(args, root):
    binaries = {
        "baseline": args.baseline.resolve(strict=True),
        "candidate": args.pkg.resolve(strict=True),
    }
    total_rows = args.rows + len(NAMED)
    report = {
        "check": args.check,
        "complete": False,
        "platform": platform.platform(),
        "rows": total_rows,
        "runs": args.runs,
        "binaries": {
            label: {"path": str(path), "sha256": digest(path)}
            for label, path in binaries.items()
        },
        "cases": {},
        "limits": [
            "Synthetic local SQLite fixture. No Nix process or network request is allowed during queries.",
            "Large full outputs are checked by hash and envelope total; they are not parsed in RAM.",
            "The input can exceed the host page cache. Do not treat these measurements as entirely resident-memory scans.",
            "Resource passes run separately from timed passes.",
        ],
    }
    assert (
        report["binaries"]["baseline"]["sha256"]
        != report["binaries"]["candidate"]["sha256"]
    ), "build the baseline and candidate in separate target directories"

    def save():
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")

    required = args.rows * (1500 if args.full_output else 400) + 512 * 1024 * 1024
    assert shutil.disk_usage(root).free >= required, (
        f"need at least {required} free bytes"
    )
    try:
        started = time.perf_counter()
        database, system, tools = prepare(root, args.rows, args.unique_exposed)
        report["fixture"] = {
            "bytes": database.stat().st_size,
            "sha256": digest(database),
            "generation_ms": elapsed_ms(started),
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
        probe = None
        if args.resources and platform.system() == "Linux":
            # Fork from a small process so Linux RSS does not inherit Python's peak.
            (root / "resource-probe.c").write_text(RESOURCE_PROBE)
            probe = root / "resource-probe"
            compile_c(root / "resource-probe.c", probe)
            control = subprocess.run(
                [str(probe), "/bin/sh", "-c", "true"],
                env=env,
                capture_output=True,
                check=True,
            )
            report["resource_probe_control"] = json.loads(
                control.stderr.rsplit(b"\nPKG_RESOURCE:", 1)[1]
            )
            assert report["resource_probe_control"]["peak_rss_bytes"] < 8 * 1024 * 1024
        cases = {
            "literal": (["ripgrep", "--limit", "20"], 1, "nixpkgs:ripgrep"),
            "literal_no_fuzzy": (
                ["ripgrep", "--no-fuzzy", "--limit", "20"],
                1,
                "nixpkgs:ripgrep",
            ),
            "typo": (["ripgrpe", "--limit", "20"], 1, "nixpkgs:ripgrep"),
            "broad": (["tool", "--broad", "--limit", "20"], tools, None),
            "broad_all": (["tool", "--broad", "--all"], tools, None),
            "regex": (["ripgrep|gnugrep", "--regex", "--limit", "20"], 2, None),
            "dense_limited": ([".", "--regex", "--limit", "20"], total_rows, None),
        }
        if args.full_output:
            cases["dense_full"] = ([".", "--regex", "--all"], total_rows, None)
        save()
        for name, (words, total, first) in cases.items():
            command = ["search", *words, "--source", "nixpkgs", "--json"]
            case = {
                "command": command,
                "observations": [],
                "resources": {label: [] for label in binaries},
            }
            report["cases"][name] = case
            expected = None

            def observe(
                label,
                expected,
                resource=False,
                *,
                command=command,
                name=name,
                total=total,
                first=first,
            ):
                output = root / "stdout.json"
                errors = root / "stderr.txt"
                native = [str(binaries[label]), *command]
                if resource and probe:
                    native.insert(0, str(probe))
                free = args.rows * 1000 if name == "dense_full" else 128 * 1024 * 1024
                assert shutil.disk_usage(root).free >= free
                resources = None
                with output.open("wb") as stdout, errors.open("wb") as stderr:
                    started = time.perf_counter()
                    if resource and not probe:
                        process = subprocess.Popen(
                            native, env=env, stdout=stdout, stderr=stderr
                        )
                        resources = wait_with_usage(process, 1800)
                        code = process.returncode
                    else:
                        code = subprocess.run(
                            native,
                            env=env,
                            stdout=stdout,
                            stderr=stderr,
                            timeout=1800,
                            check=False,
                        ).returncode
                    elapsed = elapsed_ms(started)
                error = errors.read_text(errors="replace")
                assert code == 0, (name, label, code, error)
                proof = output_proof(output, total, first)
                output.unlink()
                if resource and probe:
                    resources = json.loads(error.rsplit("\nPKG_RESOURCE:", 1)[1])
                assert expected is None or proof == expected, (
                    name,
                    label,
                    proof,
                    expected,
                )
                return elapsed, proof, error, resources

            samples = {label: [] for label in binaries}
            for iteration, label in balanced(binaries, args.runs):
                elapsed, proof, error, _ = observe(label, expected)
                if expected is None:
                    expected = case["output"] = proof
                    assert proof["approximate"] == (name == "typo")
                case["observations"].append(
                    {
                        "binary": label,
                        "iteration": iteration,
                        "excluded": iteration == 0,
                        "elapsed_ms": elapsed,
                        "stderr": error,
                    }
                )
                if iteration:
                    samples[label].append(elapsed)
                save()
            case.update(compare(samples))
            case["paired_delta_ms"] = summary(
                [
                    round(c - b, 3)
                    for b, c in zip(
                        samples["baseline"], samples["candidate"], strict=True
                    )
                ]
            )
            if args.resources:
                for label in binaries:
                    for _ in range(3):
                        case["resources"][label].append(
                            observe(label, expected, resource=True)[3]
                        )
                        save()
            print(
                name,
                case["baseline"]["median_ms"],
                case["candidate"]["median_ms"],
                flush=True,
            )
        assert digest(database) == report["fixture"]["sha256"], (
            "search changed the fixture"
        )
        assert not list(temporary.iterdir()), "search left temporary files"
        report["system"] = system
        report["complete"] = True
        save()
    except BaseException:
        report["failure"] = traceback.format_exc()
        save()
        raise
