"""Measure real pkg commands with local Nix fixtures and disposable profiles.

No vendor payloads are downloaded. Public tap metadata is optional. Background
service changes are left to disposable Linux hosts. App storage must be private
and on the Nix volume.
"""

import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from common import Bench, balanced, nonempty, search_result

PUBLIC_TAP = ("dotsecenv/tap", "c307152f3876fbd15afe1287fc54a5a8763df6b7")


def register(checks, binaries):
    check = checks.add_parser(
        "commands", parents=[binaries], help="pkg command matrix", description=__doc__
    )
    check.add_argument(
        "--public-tap",
        action="store_true",
        help="import one pinned public tap; requires a ready sandbox",
    )
    check.add_argument(
        "--background",
        action="store_true",
        help="try the real user service manager in disposable Linux",
    )
    check.set_defaults(run=run)


def probe_version(bench, label):
    executable = bench.profile(label) / "bin/probe-000"
    return subprocess.check_output(
        [str(executable)], env=bench.envs[label], text=True
    ).strip()


def startup(bench):
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
        bench.measure(name, command, nonempty)


def catalogs(bench):
    # First refresh includes source resolution, native enumeration, serialization and fsync.
    for label in bench.binaries:
        bench.observe(label, "prepare_empty_cache", ["update", "--scheduled"])
        found = bench.query(label, ["search", "probe-000", "--source", "nixpkgs"])
        assert found["total"] >= 1
    for name, command in [
        ("update_warm", ["update"]),
        ("update_scheduled", ["update", "--scheduled"]),
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
        json_search = name.startswith("search_") and "--json" in command
        bench.measure(name, command, search_result if json_search else nonempty)


def app_setup(bench):
    for label in bench.binaries:
        env = bench.envs[label]
        if platform.system() == "Linux":
            bench.observe(label, "apps_setup_unsupported", ["apps", "setup"], code=1)
        elif os.stat(env["PKG_APP_CACHE_DIR"]).st_dev == os.stat("/nix/store").st_dev:
            bench.observe(label, "apps_setup_private_storage", ["apps", "setup"])
        else:
            bench.report["limits"].append(
                "apps setup administrator path not run: temporary storage is on a different volume"
            )


def profile_scale(bench):
    # Each binary has its own state; both reuse the same derivations.
    for count in (1, 10, 100):
        packages = [f"nixpkgs:probe-{number:03d}" for number in range(count)]
        for label in bench.binaries:
            bench.clear(label)
            bench.observe(
                label, f"install_{count}_store_may_be_cold", ["install", *packages]
            )
            assert len(bench.query(label, ["list"])) == count

        def check_list(output, expected=count):
            assert len(json.loads(output)["result"]) == expected

        bench.measure(f"list_{count}", ["list", "--json"], check_list)
        bench.measure(f"list_text_{count}", ["list"])
        bench.measure(f"doctor_{count}", ["doctor", "--json"])
        bench.measure(f"history_{count}", ["history"])
        bench.measure(f"install_duplicate_{count}", ["install", "nixpkgs:probe-000"])
        bench.measure(f"prune_recent_{count}", ["prune", "--older-than", "30d"])
    for label in bench.binaries:
        bench.clear(label)


def mutations(bench):
    """Time one warm install, upgrade, two rollbacks, and remove per pair."""
    names = (
        "install_warm",
        "upgrade_changed",
        "rollback_previous",
        "rollback_explicit",
        "remove_warm",
    )
    samples = {name: {label: [] for label in bench.binaries} for name in names}
    for iteration, label in balanced(bench.binaries, bench.args.runs):

        def timed(name, command, label=label, iteration=iteration):
            elapsed = bench.invoke(label, command)[0]
            if iteration:
                samples[name][label].append(elapsed)

        bench.write_flake(1)
        timed("install_warm", ["install", "nixpkgs:probe-000"])
        bench.write_flake(2)
        timed("upgrade_changed", ["upgrade", "--all"])
        assert probe_version(bench, label) == "2"
        link = os.readlink(bench.profile(label))
        generation = link.removeprefix("pkg-").removesuffix("-link")
        timed("rollback_previous", ["rollback"])
        assert probe_version(bench, label) == "1"
        timed("rollback_explicit", ["rollback", generation])
        assert probe_version(bench, label) == "2"
        timed("remove_warm", ["remove", "nixpkgs:probe-000"])
        assert bench.query(label, ["list"]) == []
    for name, values in samples.items():
        bench.record(name, values)


def trace_nix(bench):
    """Count real Nix calls separately from the timed commands."""
    trace = bench.root / "nix-calls"
    wrapper = bench.root / "nix-trace"
    nix = shutil.which("nix")
    wrapper.write_text(
        f"#!{sys.executable}\nimport json, os, sys\n"
        f"with open({str(trace)!r}, 'a') as log: log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
        f"os.execv({nix!r}, [{nix!r}, *sys.argv[1:]])\n"
    )
    wrapper.chmod(0o700)
    config = Path(bench.envs["candidate"]["XDG_CONFIG_HOME"]) / "pkg/config.toml"
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
            ("update", ["update", "--scheduled"]),
        ]:
            trace.write_text("")
            bench.invoke("candidate", command)
            calls[name] = [json.loads(line) for line in trace.read_text().splitlines()]
    finally:
        config.write_text(original)
    bench.report["nix_calls"] = calls


def public_tap(bench):
    source, revision = PUBLIC_TAP
    for label in bench.binaries:
        bench.observe(
            label,
            "tap_add_pinned",
            ["tap", "add", source, "--revision", revision, "--trust"],
        )
    try:
        bench.measure("tap_list_registered", ["tap", "list", "--json"])
        bench.measure(
            "search_registered_tap",
            ["search", "dotsecenv", "--source", source, "--json"],
            search_result,
        )
        bench.measure(
            "tap_update_pinned", ["tap", "update", source, "--revision", revision]
        )
    finally:
        for label in bench.binaries:
            bench.observe(label, "tap_remove", ["tap", "remove", source])
            assert bench.query(label, ["tap", "list"]) == []


def background(bench):
    assert platform.system() == "Linux", (
        "service changes require a disposable Linux host"
    )
    for label in bench.binaries:
        # Record real manager availability rather than replacing it with a stub.
        elapsed, result = bench.run(label, ["background", "enable"], timeout=60)
        bench.report["observations"].append(
            {
                "binary": label,
                "name": "background_enable",
                "command": ["background", "enable"],
                "code": result.returncode,
                "elapsed_ms": elapsed,
                "stdout": result.stdout,
                "stderr": result.stderr,
            }
        )
        if result.returncode == 0:
            bench.observe(
                label, "background_disable_enabled", ["background", "disable"]
            )
        else:
            bench.report["limits"].append(
                "Linux user service manager unavailable: " + result.stderr.strip()
            )


def run(args):
    with tempfile.TemporaryDirectory(prefix="pkg-speed-") as temporary:
        bench = Bench(args, Path(temporary))
        startup(bench)
        catalogs(bench)
        app_setup(bench)
        profile_scale(bench)
        mutations(bench)
        trace_nix(bench)
        if args.public_tap:
            try:
                public_tap(bench)
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                bench.report["limits"].append(
                    "Public tap benchmark incomplete: " + str(error)
                )
        bench.report["limits"].extend(
            [
                "Fixture source sizes: 128 native packages, 1000 casks, 2000 formulae; generated metadata, real Nix evaluator and profiles.",
                "First install and setup observations are ordered baseline then candidate and share the Nix store; do not use them as fair speedup estimates.",
                "Background enable changes a host service. It is measured only with --background on a disposable Linux host.",
                "App setup administrator provisioning is excluded.",
                "No vendor network download or large vendor package build is included.",
            ]
        )
        if args.background:
            background(bench)
        bench.report["complete"] = True
        bench.save()
