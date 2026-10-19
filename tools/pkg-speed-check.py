#!/usr/bin/env python3
"""Measure real pkg commands with local Nix fixtures and disposable profiles.

No vendor payloads are downloaded. Public tap metadata is optional. Background
service changes are left to disposable Linux hosts. App storage must be private
and on the Nix volume. Raw samples include process startup and captured output.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def load_fixture():
    spec = importlib.util.spec_from_file_location(
        "native_check", Path(__file__).with_name("native-cli-check.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.BUILDER


def summary(values):
    return {
        "samples_ms": values,
        "median_ms": round(statistics.median(values), 3),
        "min_ms": min(values),
        "max_ms": max(values),
    }


def nonempty(output):
    assert output.strip(), "empty output"


def search_result(output):
    result = json.loads(output)["result"]
    assert result["total"] > 0, result
    assert result["results"], result


class Bench:
    def __init__(self, args, root):
        self.args, self.root = args, root
        self.package_count = getattr(args, "package_count", 128)
        self.system = {
            ("Darwin", "arm64"): "aarch64-darwin",
            ("Linux", "x86_64"): "x86_64-linux",
        }[(platform.system(), platform.machine())]
        self.binaries = {
            "baseline": args.baseline.resolve(strict=True),
            "candidate": args.pkg.resolve(strict=True),
        }
        self.report = {
            "complete": False,
            "platform": platform.platform(),
            "system": self.system,
            "runs": args.runs,
            "clock": "perf_counter wall time, child startup and captured output included",
            "binaries": {
                key: {
                    "path": str(binary),
                    "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "size_bytes": binary.stat().st_size,
                }
                for key, binary in self.binaries.items()
            },
            "cases": {},
            "observations": [],
            "limits": [],
        }
        assert (
            self.report["binaries"]["baseline"]["sha256"]
            != self.report["binaries"]["candidate"]["sha256"]
        )
        self.source = root / "fixture"
        self.source.mkdir()
        (self.source / "builder.c").write_text(load_fixture())
        for version in (1, 2):
            command = [shutil.which("cc") or "cc"]
            if platform.system() == "Linux":
                command.append("-static")
            subprocess.run(
                command
                + [
                    f'-DVERSION="{version}"',
                    str(self.source / "builder.c"),
                    "-o",
                    str(self.source / f"builder{version}"),
                ],
                check=True,
            )
        subprocess.run(
            ["git", "init", "--quiet", "-b", "main", str(self.source)], check=True
        )
        self.write_flake(1)
        self.envs = {label: self.environment(root / label) for label in self.binaries}
        nix = shutil.which("nix")
        assert nix, "real Nix is required"
        self.report["nix_version"] = subprocess.check_output(
            [nix, "--version"], text=True
        ).strip()

    def environment(self, root):
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
        config = root / "config/pkg/config.toml"
        config.parent.mkdir(parents=True)
        reference = json.dumps("git+file://" + str(self.source))
        config.write_text(
            f"[sources]\nnixpkgs = {reference}\ncasks = {reference}\nformulae = {reference}\n"
        )
        # A pre-created private location avoids the administrator setup path.
        env["PKG_APP_CACHE_DIR"] = str(root / "app-cache")
        Path(env["PKG_APP_CACHE_DIR"]).mkdir(mode=0o700)
        return env

    def write_flake(self, version):
        def record(index, source, kind):
            token = f"catalog-probe-{index:04d}"
            return {
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

        def index(schema, source, count, kind):
            rows = {
                f"{source}/catalog-probe-{i:04d}": record(i, source, kind)
                for i in range(count)
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
        for i in range(self.package_count):
            name = f"probe-{i:03d}"
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
        subprocess.run(["git", "-C", str(self.source), "add", "."], check=True)
        changed = subprocess.run(
            ["git", "-C", str(self.source), "diff", "--cached", "--quiet"], check=False
        ).returncode
        if changed:
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(self.source),
                    "-c",
                    "user.name=pkg benchmark",
                    "-c",
                    "user.email=benchmark@example.com",
                    "commit",
                    "--quiet",
                    "-m",
                    f"Fixture version {version}",
                ],
                check=True,
            )

    def invoke(self, label, command, env=None, code=0):
        started = time.perf_counter()
        result = subprocess.run(
            [str(self.binaries[label]), *command],
            env=env or self.envs[label],
            capture_output=True,
            text=True,
            timeout=600,
            check=False,
        )
        elapsed = round((time.perf_counter() - started) * 1000, 3)
        if result.returncode != code:
            raise RuntimeError(
                f"{label} {command}: expected {code}, got {result.returncode}: {result.stdout} {result.stderr}"
            )
        return elapsed, result.stdout, result.stderr

    def save(self):
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

    def measure(self, name, command, validate=None, envs=None, code=0):
        values = {label: [] for label in self.binaries}
        for iteration in range(self.args.runs + 1):
            order = list(self.binaries)
            if iteration % 2:
                order.reverse()
            for label in order:
                elapsed, stdout, _stderr = self.invoke(
                    label, command, None if envs is None else envs[label], code
                )
                if validate:
                    validate(stdout)
                if iteration:
                    values[label].append(elapsed)
        self.report["cases"][name] = dict(
            command=command,
            expected_code=code,
            **{label: summary(samples) for label, samples in values.items()},
        )
        case = self.report["cases"][name]
        case["latency_reduction_percent"] = round(
            100 * (1 - case["candidate"]["median_ms"] / case["baseline"]["median_ms"]),
            1,
        )
        self.save()
        print(
            f"{name}: baseline={case['baseline']['median_ms']} candidate={case['candidate']['median_ms']} ms",
            flush=True,
        )

    def query(self, label, command, env=None):
        return json.loads(self.invoke(label, [*command, "--json"], env)[1])["result"]

    def probe_version(self, label):
        executable = (
            Path(self.envs[label]["XDG_STATE_HOME"]) / "nix/profiles/pkg/bin/probe-000"
        )
        return subprocess.check_output(
            [str(executable)], env=self.envs[label], text=True
        ).strip()

    def run(self):
        for name, command in [
            ("version", ["--version"]),
            ("help", ["--help"]),
            ("completion_bash", ["completion", "bash"]),
            ("completion_zsh", ["completion", "zsh"]),
            ("completion_fish", ["completion", "fish"]),
            ("shellenv", ["shellenv"]),
            ("list_empty", ["list", "--json"]),
            ("tap_list_empty", ["tap", "list", "--json"]),
            ("background_status_disabled", ["background", "status"]),
            ("background_disable_absent", ["background", "disable"]),
            ("doctor_empty", ["doctor", "--json"]),
        ]:
            self.measure(
                name,
                command,
                nonempty,
            )
        # First refresh includes source resolution, native enumeration, serialization and fsync.
        for label in self.binaries:
            self.observe(label, "setup_empty_cache", ["setup", "--no-background"])
            assert (
                self.query(label, ["search", "probe-000", "--source", "nixpkgs"])[
                    "total"
                ]
                >= 1
            )
        for name, command in [
            ("setup_warm", ["setup", "--no-background"]),
            ("update_warm", ["update"]),
            ("update_catalogs_only", ["update", "--catalogs-only"]),
            (
                "search_nixpkgs_128",
                ["search", "probe-000", "--source", "nixpkgs", "--json"],
            ),
            (
                "search_casks_1000",
                ["search", "catalog-probe-0000", "--source", "cask", "--json"],
            ),
            (
                "search_formulae_2000",
                ["search", "catalog-probe-0000", "--source", "brew", "--json"],
            ),
            (
                "search_all_sources",
                ["search", "catalog", "--broad", "--limit", "20", "--json"],
            ),
            ("search_text", ["search", "probe-000", "--source", "nixpkgs"]),
            ("info_nixpkgs", ["info", "nixpkgs:probe-000", "--json"]),
            ("info_cask", ["info", "cask:catalog-probe-0000", "--json"]),
            ("info_formula", ["info", "brew:catalog-probe-0000", "--json"]),
            ("apps_sync_empty", ["apps", "sync"]),
        ]:
            self.measure(
                name,
                command,
                search_result
                if name.startswith("search_") and "--json" in command
                else nonempty,
            )
        for label in self.binaries:
            env = self.envs[label]
            if (
                platform.system() == "Darwin"
                and os.stat(env["PKG_APP_CACHE_DIR"]).st_dev
                == os.stat("/nix/store").st_dev
            ):
                self.observe(label, "apps_setup_private_storage", ["apps", "setup"])
            elif platform.system() == "Linux":
                self.observe(label, "apps_setup_unsupported", ["apps", "setup"], code=1)
            else:
                self.report["limits"].append(
                    "apps setup administrator path not run: temporary storage is on a different volume"
                )
        # Real native profiles: each binary has its own state; both reuse the same derivations.
        for count in (1, 10, 100):
            for label in self.binaries:
                env = self.envs[label]
                current = self.query(label, ["list"], env)
                if current:
                    self.observe(
                        label,
                        f"remove_before_scale_{count}",
                        ["remove", *[row["package_id"] for row in current]],
                    )
                self.observe(
                    label,
                    f"install_{count}_store_may_be_cold",
                    ["install", *[f"nixpkgs:probe-{i:03d}" for i in range(count)]],
                )
                assert len(self.query(label, ["list"])) == count

            def check_list(output, expected=count):
                assert len(json.loads(output)["result"]) == expected

            self.measure(f"list_{count}", ["list", "--json"], check_list)
            self.measure(f"list_text_{count}", ["list"])
            self.measure(f"doctor_{count}", ["doctor", "--json"])
            self.measure(f"history_{count}", ["history"])
            self.measure(f"install_duplicate_{count}", ["install", "nixpkgs:probe-000"])
            self.measure(f"prune_recent_{count}", ["prune", "--older-than", "30d"])
        for label in self.binaries:
            rows = self.query(label, ["list"])
            self.observe(
                label,
                "remove_scale_profile",
                ["remove", *[row["package_id"] for row in rows]],
            )
        # Warm mutation measurements use built store outputs. Discard the first full cycle.
        commands = {
            name: {label: [] for label in self.binaries}
            for name in (
                "install_warm",
                "upgrade_changed",
                "rollback_previous",
                "rollback_explicit",
                "remove_warm",
            )
        }
        for iteration in range(self.args.runs + 1):
            order = list(self.binaries)
            if iteration % 2:
                order.reverse()
            for label in order:
                self.write_flake(1)
                elapsed = self.invoke(label, ["install", "nixpkgs:probe-000"])[0]
                if iteration:
                    commands["install_warm"][label].append(elapsed)
                self.write_flake(2)
                elapsed = self.invoke(label, ["upgrade", "--all"])[0]
                assert self.probe_version(label) == "2"
                if iteration:
                    commands["upgrade_changed"][label].append(elapsed)
                profile = Path(self.envs[label]["XDG_STATE_HOME"]) / "nix/profiles/pkg"
                generation = (
                    os.readlink(profile).removeprefix("pkg-").removesuffix("-link")
                )
                for name, command, version in [
                    ("rollback_previous", ["rollback"], "1"),
                    ("rollback_explicit", ["rollback", generation], "2"),
                ]:
                    elapsed = self.invoke(label, command)[0]
                    assert self.probe_version(label) == version
                    if iteration:
                        commands[name][label].append(elapsed)
                elapsed = self.invoke(label, ["remove", "nixpkgs:probe-000"])[0]
                assert self.query(label, ["list"]) == []
                if iteration:
                    commands["remove_warm"][label].append(elapsed)
        for name, values in commands.items():
            self.report["cases"][name] = {
                label: summary(samples) for label, samples in values.items()
            }
            case = self.report["cases"][name]
            case["latency_reduction_percent"] = round(
                100
                * (1 - case["candidate"]["median_ms"] / case["baseline"]["median_ms"]),
                1,
            )
        self.trace_nix()
        if self.args.public_tap:
            try:
                self.public_tap()
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                self.report["limits"].append(
                    "Public tap benchmark incomplete: " + str(error)
                )
                self.save()
        self.report["limits"].extend(
            [
                "Fixture source sizes: 128 native packages, 1000 casks, 2000 formulae; generated metadata, real Nix evaluator and profiles.",
                "First install and setup observations are ordered baseline then candidate and share the Nix store; do not use them as fair speedup estimates.",
                "Background enable changes a host service. It is measured only with --background on a disposable Linux host.",
                "App setup administrator provisioning is excluded; macOS app payload/helper timings are in native-cli-check results.",
                "No vendor network download or large vendor package build is included.",
            ]
        )
        if self.args.background:
            assert platform.system() == "Linux", (
                "service changes require a disposable Linux host"
            )
            for label in self.binaries:
                # Record real manager availability rather than replacing it with a stub.
                started = time.perf_counter()
                result = subprocess.run(
                    [str(self.binaries[label]), "background", "enable"],
                    env=self.envs[label],
                    text=True,
                    capture_output=True,
                    timeout=60,
                    check=False,
                )
                self.report["observations"].append(
                    {
                        "binary": label,
                        "name": "background_enable",
                        "command": ["background", "enable"],
                        "code": result.returncode,
                        "elapsed_ms": round((time.perf_counter() - started) * 1000, 3),
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                    }
                )
                if result.returncode == 0:
                    self.observe(
                        label, "background_disable_enabled", ["background", "disable"]
                    )
                else:
                    self.report["limits"].append(
                        "Linux user service manager unavailable: "
                        + result.stderr.strip()
                    )
        self.report["complete"] = True
        self.save()

    def trace_nix(self):
        """Count real Nix calls separately from the timed commands."""
        trace = self.root / "nix-calls"
        wrapper = self.root / "nix-trace"
        nix = shutil.which("nix")
        wrapper.write_text(
            f"#!{sys.executable}\nimport json, os, sys\n"
            f"with open({str(trace)!r}, 'a') as log: log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            f"os.execv({nix!r}, [{nix!r}, *sys.argv[1:]])\n"
        )
        wrapper.chmod(0o700)
        config = Path(self.envs["candidate"]["XDG_CONFIG_HOME"]) / "pkg/config.toml"
        original = config.read_text()
        config.write_text(
            original + "\n[runtime]\nnix = " + json.dumps(str(wrapper)) + "\n"
        )
        calls = {}
        try:
            for name, command in [
                ("search", ["search", "probe-000", "--source", "nixpkgs", "--json"]),
                ("info", ["info", "nixpkgs:probe-000", "--json"]),
                ("list_empty", ["list", "--json"]),
                ("doctor_empty", ["doctor", "--json"]),
                ("update", ["update", "--catalogs-only"]),
            ]:
                trace.write_text("")
                self.invoke("candidate", command)
                calls[name] = [
                    json.loads(line) for line in trace.read_text().splitlines()
                ]
        finally:
            config.write_text(original)
        self.report["nix_calls"] = calls

    def public_tap(self):
        source, revision = "dotsecenv/tap", "c307152f3876fbd15afe1287fc54a5a8763df6b7"
        for label in self.binaries:
            self.observe(
                label,
                "tap_add_pinned",
                ["tap", "add", source, "--revision", revision, "--trust"],
            )
        self.measure("tap_list_registered", ["tap", "list", "--json"])
        self.measure(
            "search_registered_tap",
            ["search", "dotsecenv", "--source", source, "--json"],
            search_result,
        )
        try:
            self.measure(
                "tap_update_pinned", ["tap", "update", source, "--revision", revision]
            )
        finally:
            for label in self.binaries:
                self.observe(label, "tap_remove", ["tap", "remove", source])
                assert self.query(label, ["tap", "list"]) == []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pkg", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument(
        "--runs", type=int, default=8, help="measured pairs; even counts balance order"
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--public-tap",
        action="store_true",
        help="import one pinned public tap; requires a ready sandbox",
    )
    parser.add_argument(
        "--background",
        action="store_true",
        help="try the real user service manager in disposable Linux",
    )
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    with tempfile.TemporaryDirectory(prefix="pkg-speed-") as temporary:
        Bench(args, Path(temporary)).run()


if __name__ == "__main__":
    main()
