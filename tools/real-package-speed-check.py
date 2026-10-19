#!/usr/bin/env python3
"""Measure real pinned Nix, Formula, and Cask installs in temporary profiles.

Downloads real package payloads into the shared Nix store. Runs native command
version checks. On macOS it creates and removes app launchers under a temporary
HOME. First fetches are observations. Repeated installs use the warm store.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import platform
import shutil
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument("--repo", type=pathlib.Path, required=True)
p.add_argument("--pkg", type=pathlib.Path, required=True)
p.add_argument("--baseline", type=pathlib.Path, required=True)
p.add_argument("--output", type=pathlib.Path, required=True)
p.add_argument(
    "--runs", type=int, default=4, help="measured pairs; even counts balance order"
)
p.add_argument(
    "--fresh-home-per-install",
    action="store_true",
    help="use fresh HOME, XDG paths, profile, and app cache for every install",
)
p.add_argument(
    "--case",
    action="append",
    choices=[
        "nix_native",
        "formula_equivalent",
        "formula_bottle",
        "cask_dmg",
        "cask_zip",
    ],
)
p.add_argument("--zip-package", default="iterm2")
p.add_argument(
    "--extra-cask",
    action="append",
    nargs=3,
    default=[],
    metavar=("TOKEN", "COMMAND", "CHECK"),
    help="add a Cask; CHECK is version, version-word, help, or app; use '-' for an app command",
)
p.add_argument("--extra-only", action="store_true", help="run only --extra-cask cases")
a = p.parse_args()
if a.runs < 1:
    p.error("--runs must be positive")
if a.extra_only and not a.extra_cask:
    p.error("--extra-only needs --extra-cask")
for token, command, check in a.extra_cask:
    if check not in {"version", "version-word", "help", "app"}:
        p.error("Cask CHECK must be version, version-word, help, or app")
    if not token or "/" in token or not command or "/" in command:
        p.error("extra Cask tokens and commands must be single names")
    if check == "app" and platform.system() != "Darwin":
        p.error("app checks need macOS")
spec = importlib.util.spec_from_file_location(
    "speed", a.repo / "tools/pkg-speed-check.py"
)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
root = pathlib.Path(tempfile.mkdtemp(prefix="pkg-real-package-speed-"))
b = m.Bench(a, root)
report = b.report
report["scope"] = (
    "real pinned packages; fresh HOME, XDG paths, profile, and app cache per install; shared store warm after initial observations"
    if a.fresh_home_per_install
    else "real pinned packages; first store fetch observations and repeated store-warm profile changes"
)
report["cases"] = {}
report["limits"] = []
source = root / "real-sources"
source.mkdir()
for part in ["casks", "formulae"]:
    shutil.copytree(a.repo / "nix" / part, source / part)
subprocess.run(["git", "init", "--quiet", "-b", "main", str(source)], check=True)
subprocess.run(["git", "-C", str(source), "add", "."], check=True)
subprocess.run(
    [
        "git",
        "-C",
        str(source),
        "-c",
        "user.name=pkg benchmark",
        "-c",
        "user.email=benchmark@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "pinned catalog fixture",
    ],
    check=True,
)
revision = subprocess.check_output(
    ["git", "-C", str(source), "rev-parse", "HEAD"], text=True
).strip()
report["source_revision"] = revision
report["source_catalog_sha256"] = {
    part: hashlib.sha256(
        (source / part / "catalog/catalog.json").read_bytes()
    ).hexdigest()
    for part in ["casks", "formulae"]
}
app_roots = []


def configure(env):
    config = pathlib.Path(env["XDG_CONFIG_HOME"]) / "pkg/config.toml"
    config.write_text(
        '[sources]\nnixpkgs = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30"\ncasks = '
        + json.dumps("git+file://" + str(source) + "?dir=casks")
        + "\nformulae = "
        + json.dumps("git+file://" + str(source) + "?dir=formulae")
        + "\n"
    )
    if platform.system() == "Darwin":
        parent = pathlib.Path("/nix/var/pkg/cask-apps") / str(os.getuid())
        assert parent.is_dir()
        app = tempfile.TemporaryDirectory(prefix="pkg-speed-real-", dir=parent)
        app_roots.append(app)
        env["PKG_APP_CACHE_DIR"] = app.name


for env in b.envs.values():
    configure(env)
packages = [
    (
        "nix_native",
        "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30#hello",
        "hello",
    ),
    ("formula_equivalent", "brew:jq", "jq"),
]
if platform.system() == "Darwin":
    packages += [
        ("formula_bottle", "brew:hello", "hello"),
        ("cask_dmg", "cask:caffeine", None),
        ("cask_zip", "cask:" + a.zip_package, None),
    ]
if a.case:
    packages = [package for package in packages if package[0] in a.case]
    assert packages, "the requested cases do not apply to this system"
if a.extra_only:
    packages = []
check_args = {}
for token, command, check in a.extra_cask:
    name = "cask_extra_" + token
    assert all(package[0] != name for package in packages), "duplicate extra Cask"
    packages.append((name, "cask:" + token, None if check == "app" else command))
    check_args[name] = {
        "help": "--help",
        "version-word": "version",
    }.get(check, "--version")


def save():
    a.output.parent.mkdir(parents=True, exist_ok=True)
    a.output.write_text(json.dumps(report, indent=2) + "\n")


def call(label, name, command, observe=True):
    started = time.perf_counter()
    r = subprocess.run(
        [str(b.binaries[label]), *command],
        env=b.envs[label],
        capture_output=True,
        text=True,
        timeout=1800,
        check=False,
    )
    elapsed = round(1000 * (time.perf_counter() - started), 3)
    if observe:
        report["observations"].append(
            {
                "binary": label,
                "name": name,
                "command": command,
                "elapsed_ms": elapsed,
                "code": r.returncode,
                "stdout": r.stdout,
                "stderr": r.stderr,
            }
        )
        save()
    return r, elapsed


def remove(label):
    r, _ = call(label, "cleanup_list", ["list", "--json"])
    if r.returncode:
        return
    rows = json.loads(r.stdout)["result"]
    if rows:
        r, _ = call(
            label, "cleanup_remove", ["remove", *[row["package_id"] for row in rows]]
        )
        assert r.returncode == 0, r.stderr


def check_apps(label, name, profile):
    apps = {
        p.name: {"source": str(p)} for p in (profile / "Applications").glob("*.app")
    }
    for manifest in (profile / "libexec/pkg-casks").glob("*/apps.json"):
        data = json.loads(manifest.read_text())
        assert data["schema"] == "pkg-cask-apps/2"
        output = manifest.resolve().parents[3]
        for app, entry in data["apps"].items():
            restored = (
                pathlib.Path(b.envs[label]["PKG_APP_CACHE_DIR"]) / output.name / app
            )
            assert restored.is_dir(), restored
            subprocess.run(
                ["/usr/bin/codesign", "--verify", "--deep", "--strict", str(restored)],
                capture_output=True,
                text=True,
                timeout=120,
                check=True,
            )
            apps[app] = {
                "source": str(output / entry["source"]),
                "restored": str(restored),
                "signature_files": len(entry.get("signatures", {})),
            }
    assert apps, "no app bundle found"
    for app, entry in apps.items():
        launcher = pathlib.Path(b.envs[label]["HOME"]) / "Applications/pkg" / app
        assert launcher.is_dir(), launcher
        entry["launcher"] = str(launcher)
    report["observations"].append(
        {"binary": label, "name": name + "_app_validation", "apps": apps}
    )


try:
    for name, package, executable in packages:
        samples = {label: [] for label in b.binaries}
        failed = False
        for iteration in range(a.runs + 1):
            order = list(b.binaries)
            if iteration % 2:
                order.reverse()
            for label in order:
                if a.fresh_home_per_install:
                    env = b.environment(root / f"{name}-{label}-{iteration}")
                    configure(env)
                    b.envs[label] = env
                if not iteration:
                    phase = "_first_store_may_be_cold"
                elif a.fresh_home_per_install:
                    phase = "_fresh_home_install"
                else:
                    phase = "_warm_install"
                r, elapsed = call(
                    label,
                    name + phase,
                    ["install", package],
                )
                if r.returncode:
                    failed = True
                    report["limits"].append(
                        f"{name} {label} install failed; see the retained output"
                    )
                    remove(label)
                    continue
                listed = b.query(label, ["list"])
                assert len(listed) == 1, listed
                if executable:
                    binary = (
                        pathlib.Path(b.envs[label]["XDG_STATE_HOME"])
                        / "nix/profiles/pkg/bin"
                        / executable
                    )
                    try:
                        checked = subprocess.run(
                            [str(binary), check_args.get(name, "--version")],
                            env=b.envs[label],
                            capture_output=True,
                            text=True,
                            timeout=30,
                            check=False,
                        )
                        check_result = {
                            "code": checked.returncode,
                            "stdout": checked.stdout,
                            "stderr": checked.stderr,
                        }
                    except subprocess.TimeoutExpired:
                        check_result = {
                            "code": "timeout",
                            "stdout": "",
                            "stderr": "executable check exceeded 30 seconds",
                        }
                    except OSError as error:
                        check_result = {
                            "code": "spawn_error",
                            "stdout": "",
                            "stderr": str(error),
                        }
                    report["observations"].append(
                        {
                            "binary": label,
                            "name": name + "_executable_check",
                            **check_result,
                        }
                    )
                    if check_result["code"] != 0:
                        failed = True
                        report["limits"].append(
                            f"{name} {label} executable check failed; see retained output"
                        )
                        remove(label)
                        continue
                else:
                    profile = (
                        pathlib.Path(b.envs[label]["XDG_STATE_HOME"])
                        / "nix/profiles/pkg"
                    )
                    check_apps(label, name, profile)
                if iteration:
                    samples[label].append(elapsed)
                r, _ = call(
                    label, name + "_remove", ["remove", listed[0]["package_id"]]
                )
                assert r.returncode == 0, r.stderr
                assert b.query(label, ["list"]) == []
            if failed:
                break
        report["cases"][name] = {
            "status": "failed" if failed else "passed",
            "package": package,
            **{label: m.summary(values) for label, values in samples.items() if values},
        }
        save()
        print(name, report["cases"][name], flush=True)
finally:
    for label in b.binaries:
        remove(label)
    for app in app_roots:
        app.cleanup()
report["complete"] = True
save()
shutil.rmtree(root)
