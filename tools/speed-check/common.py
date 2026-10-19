"""Shared fixtures, timing, and report helpers for the pkg speed checks.

Each check compares a baseline and a candidate pkg binary. One excluded pair
precedes the measured pairs, and the binary order alternates for each pair.
Timings are wall time and include child startup and captured output.
"""

import hashlib
import importlib.util
import json
import os
import platform
import shutil
import sqlite3
import statistics
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
NIXPKGS = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30"
CATALOG_PARTS = ("casks", "formulae")


def host_system():
    return {
        ("Darwin", "arm64"): "aarch64-darwin",
        ("Linux", "x86_64"): "x86_64-linux",
    }[(platform.system(), platform.machine())]


def nix_command(*args):
    nix = shutil.which("nix")
    assert nix, "real Nix is required"
    return [
        nix,
        "--extra-experimental-features",
        "nix-command flakes",
        "--option",
        "accept-flake-config",
        "false",
        "--option",
        "allow-import-from-derivation",
        "false",
        *args,
    ]


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while block := stream.read(1024 * 1024):
            value.update(block)
    return value.hexdigest()


def elapsed_ms(started):
    return round((time.perf_counter() - started) * 1000, 3)


def summary(values):
    return {
        "samples_ms": values,
        "median_ms": round(statistics.median(values), 3),
        "min_ms": min(values),
        "max_ms": max(values),
    }


def compare(samples, **fields):
    """Summarize each binary and the candidate's median latency reduction."""
    case = dict(fields)
    case.update({label: summary(values) for label, values in samples.items() if values})
    if "baseline" in case and "candidate" in case:
        case["latency_reduction_percent"] = round(
            100 * (1 - case["candidate"]["median_ms"] / case["baseline"]["median_ms"]),
            1,
        )
    return case


def balanced(labels, runs):
    """Yield one excluded warmup pair, then measured pairs in alternating order."""
    for iteration in range(runs + 1):
        order = list(labels)
        if iteration % 2:
            order.reverse()
        for label in order:
            yield iteration, label


def commit(directory, message, init=False):
    if init:
        subprocess.run(
            ["git", "init", "--quiet", "-b", "main", str(directory)], check=True
        )
    subprocess.run(["git", "-C", str(directory), "add", "."], check=True)
    staged = subprocess.run(
        ["git", "-C", str(directory), "diff", "--cached", "--quiet"], check=False
    )
    if staged.returncode:
        subprocess.run(
            [
                "git",
                "-C",
                str(directory),
                "-c",
                "user.name=pkg speed check",
                "-c",
                "user.email=speed-check@example.invalid",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
            check=True,
        )


def compile_c(source, output, *flags, static=False):
    command = [shutil.which("cc") or "cc", *flags]
    if static and platform.system() == "Linux":
        command.append("-static")
    subprocess.run([*command, str(source), "-o", str(output)], check=True)


def pinned_catalogs(destination):
    """Commit the pinned Cask and Formula catalogs as one local source."""
    destination.mkdir()
    for part in CATALOG_PARTS:
        shutil.copytree(REPO / "nix" / part, destination / part)
    commit(destination, "Pinned sources", init=True)
    return {
        "nixpkgs": NIXPKGS,
        **{part: f"git+file://{destination}?dir={part}" for part in CATALOG_PARTS},
    }


def write_sources(env, references):
    config = Path(env["XDG_CONFIG_HOME"]) / "pkg/config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(
        "[sources]\n"
        + "".join(
            f"{name} = {json.dumps(value)}\n" for name, value in references.items()
        )
    )


def native_rows(cache):
    """Hash the published native catalog rows in canonical order."""
    databases = list(Path(cache).rglob("catalog-*.sqlite"))
    assert len(databases) == 1, databases
    value = hashlib.sha256()
    count = 0
    with sqlite3.connect(f"file:{databases[0]}?mode=ro", uri=True) as connection:
        for row in connection.execute(
            "SELECT attribute, exposed, pname, version, description FROM packages ORDER BY attribute"
        ):
            value.update(
                json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode()
            )
            value.update(b"\n")
            count += 1
    assert count > 10000, count
    return {"rows": count, "canonical_rows_sha256": value.hexdigest()}


def native_builder():
    """Reuse the native lifecycle check's fixture builder source."""
    spec = importlib.util.spec_from_file_location(
        "native_check", REPO / "tools/native-cli-check.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.BUILDER


def nonempty(output):
    assert output.strip(), "empty output"


def search_result(output):
    result = json.loads(output)["result"]
    assert result["total"] > 0, result
    assert result["results"], result


class Bench:
    """Two pkg binaries with private homes and one local Nix fixture source."""

    def __init__(self, args, root, package_count=128):
        self.args, self.root = args, root
        self.package_count = package_count
        self.system = host_system()
        self.binaries = {
            "baseline": args.baseline.resolve(strict=True),
            "candidate": args.pkg.resolve(strict=True),
        }
        self.report = {
            "check": args.check,
            "complete": False,
            "platform": platform.platform(),
            "system": self.system,
            "runs": args.runs,
            "clock": "perf_counter wall time, child startup and captured output included",
            "binaries": {
                label: {
                    "path": str(binary),
                    "sha256": digest(binary),
                    "size_bytes": binary.stat().st_size,
                }
                for label, binary in self.binaries.items()
            },
            "cases": {},
            "observations": [],
            "limits": [],
        }
        assert (
            self.report["binaries"]["baseline"]["sha256"]
            != self.report["binaries"]["candidate"]["sha256"]
        ), "build the baseline and candidate in separate target directories"
        self.source = root / "fixture"
        self.source.mkdir()
        (self.source / "builder.c").write_text(native_builder())
        for version in (1, 2):
            compile_c(
                self.source / "builder.c",
                self.source / f"builder{version}",
                f'-DVERSION="{version}"',
                static=True,
            )
        subprocess.run(
            ["git", "init", "--quiet", "-b", "main", str(self.source)], check=True
        )
        self.write_flake(1)
        self.envs = {label: self.environment(root / label) for label in self.binaries}
        self.report["nix_version"] = subprocess.check_output(
            nix_command("--version"), text=True
        ).strip()

    def environment(self, root, references=None):
        """Create private HOME and XDG paths that use the given sources."""
        root.mkdir()
        env = dict(
            os.environ,
            HOME=str(root / "home"),
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_STATE_HOME=str(root / "state"),
            XDG_CACHE_HOME=str(root / "cache"),
            NO_COLOR="1",
        )
        Path(env["HOME"]).mkdir()
        if references is None:
            fixture = "git+file://" + str(self.source)
            references = {part: fixture for part in ("nixpkgs", *CATALOG_PARTS)}
        write_sources(env, references)
        # A pre-created private location avoids the administrator setup path.
        env["PKG_APP_CACHE_DIR"] = str(root / "app-cache")
        Path(env["PKG_APP_CACHE_DIR"]).mkdir(mode=0o700)
        return env

    def profile(self, label, env=None):
        return Path((env or self.envs[label])["XDG_STATE_HOME"]) / "nix/profiles/pkg"

    def write_flake(self, version):
        """Write native packages and generated catalogs at one fixture version."""

        def index(schema, source, count, kind):
            rows = {}
            for number in range(count):
                token = f"catalog-probe-{number:04d}"
                rows[f"{source}/{token}"] = {
                    "id": f"{source}/{token}",
                    "source": source,
                    "token": token,
                    "name": token,
                    "description": "Catalog benchmark tool",
                    "version": str(version),
                    "homepage": None,
                    "status": "eligible",
                    "kind": kind,
                    "reason": None,
                    "detail": None,
                }
            return {
                "schema": schema,
                "generator": {"name": "pkg-benchmark", "version": "1"},
                "inputs": {
                    source: {
                        "url": "https://example.com/catalog.json",
                        "revision": "a" * 40,
                        "sha256": "b" * 64,
                        "license": "BSD-2-Clause",
                    }
                },
                "targets": [self.system],
                "macosBaseline": "14",
                "systems": {self.system: {"entries": rows}},
            }

        casks = index("pkg-cask-catalog/4", "homebrew/cask", 1000, "binary")
        for row in casks["systems"][self.system]["entries"].values():
            row.update(minMacos=None, maxMacos=None)
        formulae = index(
            "pkg-formula-catalog/1", "homebrew/core", 2000, "nixpkgs-equivalent"
        )
        formulae.update(aliases={}, executables={})
        (self.source / "casks.json").write_text(json.dumps(casks))
        (self.source / "formulae.json").write_text(json.dumps(formulae))
        packages = []
        for number in range(self.package_count):
            name = f"probe-{number:03d}"
            packages.append(
                f'"{name}" = builtins.derivation {{ name = "{name}-{version}"; system = "{self.system}"; builder = ./builder{version}; args = [ ./builder{version} "{name}" ]; }} // {{ pname = "{name}"; version = "{version}"; meta.description = "Benchmark tool"; }};'
            )
        (self.source / "flake.nix").write_text(
            "{ outputs = {self}: { catalogIndex = builtins.fromJSON (builtins.readFile ./casks.json); formulaIndex = builtins.fromJSON (builtins.readFile ./formulae.json); packages."
            + self.system
            + " = { "
            + " ".join(packages)
            + " }; }; }"
        )
        commit(self.source, f"Fixture version {version}")

    def run(self, label, command, env=None, timeout=600):
        started = time.perf_counter()
        result = subprocess.run(
            [str(self.binaries[label]), *command],
            env=env or self.envs[label],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return elapsed_ms(started), result

    def invoke(self, label, command, env=None, code=0):
        elapsed, result = self.run(label, command, env)
        if result.returncode != code:
            raise RuntimeError(
                f"{label} {command}: expected {code}, got {result.returncode}: {result.stdout} {result.stderr}"
            )
        return elapsed, result.stdout, result.stderr

    def save(self):
        self.args.output.parent.mkdir(parents=True, exist_ok=True)
        self.args.output.write_text(json.dumps(self.report, indent=2) + "\n")

    def observe(self, label, name, command, env=None, code=0):
        elapsed, stdout, stderr = self.invoke(label, command, env, code)
        self.report["observations"].append(
            {
                "binary": label,
                "name": name,
                "command": command,
                "elapsed_ms": elapsed,
                "code": code,
                "stdout": stdout,
                "stderr": stderr,
            }
        )
        self.save()
        print(f"{label} {name}: {elapsed} ms", flush=True)
        return stdout

    def record(self, name, samples, **fields):
        case = self.report["cases"][name] = compare(samples, **fields)
        self.save()
        medians = {
            label: case[label]["median_ms"] for label in samples if label in case
        }
        print(f"{name}: {medians} ms", flush=True)
        return case

    def measure(self, name, command, validate=None, envs=None, code=0):
        samples = {label: [] for label in self.binaries}
        for iteration, label in balanced(self.binaries, self.args.runs):
            elapsed, stdout, _ = self.invoke(
                label, command, None if envs is None else envs[label], code
            )
            if validate:
                validate(stdout)
            if iteration:
                samples[label].append(elapsed)
        return self.record(name, samples, command=command, expected_code=code)

    def query(self, label, command, env=None):
        return json.loads(self.invoke(label, [*command, "--json"], env)[1])["result"]

    def clear(self, label, env=None):
        rows = self.query(label, ["list"], env)
        if rows:
            self.observe(
                label, "cleanup", ["remove", *[row["package_id"] for row in rows]], env
            )
        assert self.query(label, ["list"], env) == []
