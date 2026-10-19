#!/usr/bin/env python3
"""Run a compiled pkg client against real Nix in disposable profiles.

Requires Nix and a C compiler. Does not download vendor packages or change the
user's profile. Failures retain the temporary directory for diagnosis.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import pty
import select
import shutil
import signal
import subprocess
import tempfile
import time


BUILDER = r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
int main(int argc, char **argv) {
    const char *out = getenv("out");
    if (!out) { puts(VERSION); return 0; }
    if (argc != 3) return 2;
    if (getenv("fail")) { fputs("native fixture build failed\n", stderr); return 42; }
    if (!strcmp(argv[2], "slow")) sleep(10);
    char bin[4096], dst[4096];
    if (mkdir(out, 0755)) return 3;
    snprintf(bin, sizeof(bin), "%s/bin", out);
    if (mkdir(bin, 0755)) return 4;
    snprintf(dst, sizeof(dst), "%s/%s", bin, argv[2]);
    FILE *in = fopen(argv[1], "rb"), *to = fopen(dst, "wb");
    if (!in || !to) return 5;
    char buf[8192]; size_t n;
    while ((n = fread(buf, 1, sizeof(buf), in)))
        if (fwrite(buf, 1, n, to) != n) return 6;
    if (ferror(in) || fclose(to)) return 7;
    fclose(in);
    if (chmod(dst, 0755)) return 8;
    if (!strcmp(argv[2], "app-probe")) {
        char app[4096];
        snprintf(app, sizeof(app), "%s/Applications", out); mkdir(app, 0755);
        snprintf(app, sizeof(app), "%s/Applications/AppProbe.app", out); mkdir(app, 0755);
        snprintf(app, sizeof(app), "%s/Applications/AppProbe.app/Contents", out); mkdir(app, 0755);
        snprintf(app, sizeof(app), "%s/Applications/AppProbe.app/Contents/MacOS", out); mkdir(app, 0755);
        snprintf(app, sizeof(app), "%s/Applications/AppProbe.app/Contents/MacOS/app-probe", out);
        if (link(dst, app)) return 9;
        snprintf(app, sizeof(app), "%s/Applications/AppProbe.app/Contents/Info.plist", out);
        FILE *plist = fopen(app, "w");
        if (!plist) return 10;
        fputs("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>"
              "<key>CFBundleIdentifier</key><string>dev.pkg.native-check</string>"
              "<key>CFBundleExecutable</key><string>app-probe</string>"
              "<key>CFBundleName</key><string>AppProbe</string>"
              "<key>CFBundlePackageType</key><string>APPL</string>"
              "<key>CFBundleVersion</key><string>1</string>"
              "</dict></plist>", plist);
        if (fclose(plist)) return 11;
    }
    return 0;
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pkg", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path,
                        help="create this directory and retain command results and fixture files")
    args = parser.parse_args()
    pkg = str(args.pkg.resolve(strict=True))
    nix = shutil.which("nix")
    assert nix, "Nix is required; this check must not skip"
    system = {("Linux", "x86_64"): "x86_64-linux",
              ("Darwin", "arm64"): "aarch64-darwin"}[
                  (platform.system(), platform.machine())]
    root = args.evidence_dir.resolve() if args.evidence_dir else Path(tempfile.mkdtemp(prefix="pkg-native-"))
    if args.evidence_dir:
        root.mkdir(parents=True)
    print(f"Evidence: {root}", flush=True)
    env = dict(os.environ)
    env["HOME"] = str(root / "home")
    Path(env["HOME"]).mkdir()
    for key, leaf in (("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config"),
                      ("XDG_CACHE_HOME", "cache")):
        env[key] = str(root / leaf)
    profile = root / "state/nix/profiles/pkg"
    source = root / ("a" * 40)
    source.mkdir()
    (source / "builder.c").write_text(BUILDER)
    compiler = [shutil.which("cc") or "cc"]
    if platform.system() == "Linux":
        compiler.append("-static")
    for version in ("1", "2"):
        subprocess.run(compiler + [f'-DVERSION="{version}"', str(source / "builder.c"),
                                   "-o", str(source / f"builder{version}")], check=True)
    config = root / "config/pkg/config.toml"
    config.parent.mkdir(parents=True)
    reference = "path:" + str(source)
    config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference)}\n"
                      f"casks = {json.dumps(reference)}\n")
    results = []

    def run(*command, code=0, timeout=180):
        started = time.perf_counter()
        result = subprocess.run(command, env=env, text=True, capture_output=True,
                                timeout=timeout)
        results.append(dict(command=command, code=result.returncode,
                            elapsed_ms=round((time.perf_counter() - started) * 1000, 3),
                            stdout=result.stdout, stderr=result.stderr))
        (root / "results.json").write_text(json.dumps(results, indent=2))
        assert result.returncode == code, results[-1]
        return result.stdout

    print(run(nix, "--version").strip(), flush=True)

    def flake(version, collision=False, failure=False, nonce=""):
        def drv(name, v):
            return (f'builtins.derivation {{ name = "{name}-{v}"; system = "{system}"; '
                    f'builder = ./builder{v}; args = [ ./builder{v} "{name}" ]; '
                    + ('fail = "1"; ' if name == "failing" or (failure and name == "probe") else '')
                    + f'nonce = "{root.name}{nonce}"; }} '
                    f'// {{ version = "{v}"; pname = "{name}"; '
                    'meta.description = "native pkg regression fixture"; }')
        (source / "flake.nix").write_text(
            '{ outputs = {self}: { packages.' + system + ' = { probe = '
            + drv("probe", version) + '; slow = ' + drv("slow", version)
            + '; spare = ' + drv("spare", version) + '; failing = ' + drv("failing", version)
            + '; app-probe = ' + drv("app-probe", version) + '; };'
            + (' legacyPackages.' + system + '.probe = ' + drv("probe", "1") + ';'
               if collision else '') + ' }; }')

    def query(command, *arguments, code=0):
        envelope = json.loads(run(pkg, command, *arguments, "--json", code=code))
        assert set(envelope) == {"schema", "command", "result"}, envelope
        assert envelope["schema"] == 1, envelope
        assert envelope["command"] == command, envelope
        return envelope["result"]

    def entries():
        return query("list")

    configured = config.read_text()
    config.unlink()
    for empty_bases in (False, True):
        bases = {key: env[key] for key in
                 ("XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME")}
        if empty_bases:
            env.update({key: "" for key in bases})
        doctor = {row["component"]: row for row in query("doctor")}
        config_base = Path(env["HOME"]) / ".config" if empty_bases else root / "config"
        cache_base = Path(env["HOME"]) / ".cache" if empty_bases else root / "cache"
        state_base = Path(env["HOME"]) / ".local/state" if empty_bases else root / "state"
        assert doctor["config"] == dict(component="config", status="defaults",
                                         detail=str(config_base / "pkg/config.toml")), doctor
        assert doctor["discovery cache"]["detail"] == str(cache_base / "pkg"), doctor
        assert doctor["pkg profile"]["status"] == "fresh", doctor
        assert doctor["taps"]["status"] == "none", doctor
        assert run(pkg, "doctor") == "Healthy: no problems found.\n"
        assert "config: defaults" in run(pkg, "doctor", "--verbose")
        assert not (config_base / "pkg/config.toml").exists()
        assert doctor["discovery cache"]["status"] == "empty", doctor
        assert not (cache_base / "pkg").exists()
        assert not any(path.is_file() or path.is_symlink()
                       for path in state_base.rglob("*"))
        assert entries() == []
        assert run(pkg, "list") == "No packages installed.\nNext: pkg search QUERY\n"
        assert not (state_base / "nix/profiles/pkg").exists()
        env.update(bases)
    config.write_text(configured)

    flake("1")
    flake_file = source / "flake.nix"
    valid_flake = flake_file.read_text()
    flake_file.write_text(valid_flake.replace(
        "probe = ", 'broken = throw "missing attribute dependency fixture"; probe = ', 1))
    run(pkg, "info", "broken", code=1)
    assert "missing attribute dependency fixture" in results[-1]["stderr"], results[-1]
    subflake = source / "sub flake"
    subflake.mkdir()
    (subflake / "flake.nix").write_text(
        '{ outputs = {self}: { packages.' + system
        + '.broken = throw "subflake evaluation fixture"; }; }')
    config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference + '?dir=sub%20flake')}\n"
                      f"casks = {json.dumps(reference)}\n")
    run(pkg, "info", "broken", code=1)
    assert "subflake evaluation fixture" in results[-1]["stderr"], results[-1]
    (subflake / "flake.nix").write_text(
        '{ outputs = {self}: { packages.' + system
        + '.broken = throw "identity:${self.shortRev}:${toString self.lastModified}:${toString self.revCount}"; }; }')
    identity_reference = (reference + '?dir=sub%20flake&lastModified=1234567890'
                          '&rev=0123456789abcdef0123456789abcdef01234567&revCount=7')
    config.write_text(f"[sources]\nnixpkgs = {json.dumps(identity_reference)}\n"
                      f"casks = {json.dumps(reference)}\n")
    run(pkg, "info", "broken", code=1)
    assert "identity:0123456:1234567890:7" in results[-1]["stderr"], results[-1]
    config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference)}\n"
                      f"casks = {json.dumps(reference)}\n")
    flake_file.write_text(valid_flake)
    installed = run(pkg, "add", "nixpkgs:probe")
    assert "Installed: nixpkgs:probe" in installed, installed
    assert "Commands: probe" in installed, installed
    assert "Installing: nixpkgs:probe" in results[-1]["stderr"], results[-1]
    assert run(str(profile / "bin/probe")).strip() == "1"
    installed_entry = entries()[0]
    assert installed_entry["package_id"] == "nixpkgs:probe", installed_entry
    assert installed_entry["name"] == "probe", installed_entry
    assert installed_entry["entry_id"], installed_entry
    manifest = json.loads((profile / "manifest.json").read_text())
    native_entry = manifest["elements"][installed_entry["entry_id"]]
    assert installed_entry["source"] == native_entry["originalUrl"], installed_entry
    assert installed_entry["locked_source"] == native_entry["url"], installed_entry
    assert installed_entry["revision"] is None, installed_entry

    def snapshot():
        return ((profile / "manifest.json").read_bytes(), os.readlink(profile),
                {path.name: os.readlink(path) for path in profile.parent.glob("pkg-*-link")})

    first_generation = os.readlink(profile)
    first_manifest = (profile / "manifest.json").read_bytes()
    before = snapshot()
    run(pkg, "--verbose", "install", "nixpkgs:spare", "nixpkgs:failing", code=1)
    assert "native fixture build failed" in results[-1]["stderr"], results[-1]
    assert "Installed:" not in results[-1]["stdout"], results[-1]
    assert snapshot() == before
    assert not (profile / "bin/spare").exists()
    assert run(str(profile / "bin/probe")).strip() == "1"
    before_manifest = (profile / "manifest.json").read_bytes()
    before_link = os.readlink(profile)
    for arguments in (("upgrade", "--all", installed_entry["entry_id"]),
                      ("--json", "remove", "--entry", installed_entry["entry_id"]),
                      ("--json", "prune", "--older-than=1d")):
        run(pkg, *arguments, code=2)
        assert (profile / "manifest.json").read_bytes() == before_manifest
        assert os.readlink(profile) == before_link
    for name in ("remove", "uninstall"):
        run(pkg, "--verbose", name, "--entry", "--", "-missing", code=1)
        assert "-missing" in results[-1]["stderr"], results[-1]
        assert (profile / "manifest.json").read_bytes() == before_manifest
        assert os.readlink(profile) == before_link
    assert run(str(profile / "bin/probe")).strip() == "1"
    flake("2", failure=True)
    before = snapshot()
    run(pkg, "--verbose", "upgrade", installed_entry["entry_id"], "--entry", code=1)
    assert "native fixture build failed" in results[-1]["stderr"], results[-1]
    assert "Upgraded:" not in results[-1]["stdout"], results[-1]
    assert snapshot() == before
    assert run(str(profile / "bin/probe")).strip() == "1"
    flake("2")
    run(pkg, "upgrade", installed_entry["entry_id"], "--entry")
    assert run(str(profile / "bin/probe")).strip() == "2"
    second_generation = os.readlink(profile)
    first_number = first_generation.removeprefix("pkg-").removesuffix("-link")
    second_number = second_generation.removeprefix("pkg-").removesuffix("-link")
    restored = run(pkg, "rollback", first_number)
    assert f"Restored: generation {first_number}." in restored, restored
    assert os.readlink(profile) == first_generation
    assert (profile / "manifest.json").read_bytes() == first_manifest
    assert run(str(profile / "bin/probe")).strip() == "1"
    before = snapshot()
    assert run(pkg, "rollback", first_number) == f"Unchanged: generation {first_number} is still active.\n"
    run(pkg, "rollback", "999999", code=1)
    assert snapshot() == before
    run(pkg, "rollback", second_number)
    assert run(str(profile / "bin/probe")).strip() == "2"
    history = run(pkg, "history")
    rows = [line.split() for line in history.splitlines()[1:] if line and line[0].isdigit()]
    assert {int(row[0]) for row in rows} == {
        int(name.removeprefix("pkg-").removesuffix("-link")) for name in snapshot()[2]
    }, history
    assert [row[0] for row in rows if row[-1] == "current"] == [second_number], history
    run(pkg, "rollback")
    assert run(str(profile / "bin/probe")).strip() == "1"
    print("PASS: failed batch and upgrade preserve generations; selected and previous rollback restore commands")
    removed = run(pkg, "uninstall", "--entry", "--", entries()[0]["entry_id"])
    assert "Removed: nixpkgs:probe" in removed, removed
    assert "Removing: nixpkgs:probe" in results[-1]["stderr"], results[-1]
    assert "error (ignored)" not in results[-1]["stderr"], results[-1]
    assert entries() == []

    flake("2", collision=True)
    message = run(pkg, "info", "nixpkgs:probe", code=1)
    diagnostic = results[-1]["stderr"] + message
    for namespace, version in (("packages", "2"), ("legacyPackages", "1")):
        attribute = f"{namespace}.{system}.probe"
        assert attribute in diagnostic, diagnostic
        info = query("info", "nixpkgs:" + attribute)
        assert info["version"] == version, info
    flake("2")

    registry = root / "state/pkg/taps/registry.json"
    registry.parent.mkdir(parents=True)
    registry.write_text(json.dumps({"schema": "pkg-tap-registry/1", "sources": {
        "fixture/broken": {"origin": "https://github.com/fixture/homebrew-broken",
                            "added_unix": 1}}}))
    run(pkg, "install", "nixpkgs:probe")
    assert run(str(profile / "bin/probe")).strip() == "2"
    run(pkg, "info", "probe", code=1)
    profile_bytes = (profile / "manifest.json").read_bytes()
    profile_link = os.readlink(profile)
    registry_bytes = registry.read_bytes()
    doctor = {row["component"]: row for row in query("doctor", code=1)}
    assert doctor["tap fixture/broken"]["status"] == "unhealthy", doctor
    assert "pkg tap update fixture/broken" in doctor["tap fixture/broken"]["detail"], doctor
    human = run(pkg, "doctor", code=1)
    assert "Needs attention:" in human and "pkg tap update fixture/broken" in human, human
    assert registry.read_bytes() == registry_bytes
    assert not (registry.parent / "fixture").exists()
    registry.write_text("{ not json")
    doctor = {row["component"]: row for row in query("doctor", code=1)}
    assert doctor["tap registry"]["status"] == "unreadable", doctor
    assert registry.read_text() == "{ not json"
    assert (profile / "manifest.json").read_bytes() == profile_bytes
    assert os.readlink(profile) == profile_link
    registry.unlink()
    run(pkg, "remove", entries()[0]["entry_id"])

    for verbose, version in ((False, "1"), (True, "2")):
        flake(version)
        before = entries()
        command = [pkg] + (["--verbose"] if verbose else []) + ["install", "nixpkgs:slow"]
        with (root / f"cancel-{verbose}.log").open("w+") as log:
            # Compact build status is visible only on a terminal. Observe a
            # real PTY to interrupt after the build starts, before it exits.
            master, slave = pty.openpty()
            child = subprocess.Popen(command, env=env, stdout=slave, stderr=slave)
            os.close(slave)
            try:
                deadline = time.monotonic() + 60
                text = ""
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.1)[0]:
                        chunk = os.read(master, 8192).decode(errors="replace")
                        log.write(chunk)
                        text += chunk
                    log.flush()
                    if "building " in text and (verbose or "(2s)" in text):
                        break
                    assert child.poll() is None, text
                else:
                    raise AssertionError("slow build did not start")
                child.send_signal(signal.SIGINT)
                assert child.wait(timeout=30) == 130, Path(log.name).read_text()
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
                os.close(master)
        assert entries() == before
        run(pkg, "install", "nixpkgs:slow")
        assert run(str(profile / "bin/slow")).strip() == version
        run(pkg, "remove", entries()[0]["entry_id"])
        assert entries() == []

    flake("1", nonce="-concurrent")
    before = snapshot()
    with (root / "lock-holder.log").open("w+") as holder_log, \
            (root / "lock-waiter.log").open("w+") as waiter_log:
        holder = subprocess.Popen([pkg, "--verbose", "install", "nixpkgs:slow"],
                                  env=env, stdout=holder_log, stderr=holder_log)
        waiter = None
        try:
            deadline = time.monotonic() + 60
            while "building " not in Path(holder_log.name).read_text():
                assert holder.poll() is None, Path(holder_log.name).read_text()
                assert time.monotonic() < deadline, "concurrent build did not start"
                time.sleep(0.05)
            waiter = subprocess.Popen([pkg, "install", "nixpkgs:slow"],
                                      env=env, stdout=waiter_log, stderr=waiter_log)
            while "Waiting for another pkg command:" not in Path(waiter_log.name).read_text():
                assert waiter.poll() is None, Path(waiter_log.name).read_text()
                assert holder.poll() is None, Path(holder_log.name).read_text()
                assert time.monotonic() < deadline, "second command did not wait for the profile lock"
                time.sleep(0.05)
            assert snapshot() == before
            waiter.send_signal(signal.SIGINT)
            assert waiter.wait(timeout=10) in (-signal.SIGINT, 130), Path(waiter_log.name).read_text()
            assert holder.wait(timeout=60) == 0, Path(holder_log.name).read_text()
        finally:
            for child in (waiter, holder):
                if child is not None and child.poll() is None:
                    child.kill()
                    child.wait()
    assert run(str(profile / "bin/slow")).strip() == "1"
    assert len(entries()) == 1
    before = snapshot()
    assert "Already installed:" in run(pkg, "install", "nixpkgs:slow")
    assert snapshot() == before
    run(pkg, "remove", "nixpkgs:slow")
    assert entries() == []
    flake("2")
    print("PASS: concurrent CLI waits, waiter cancellation, holder completion and duplicate install")

    formulae = root / "formulae"
    formulae.mkdir()
    for version in ("1", "2"):
        shutil.copy2(source / f"builder{version}", formulae / f"builder{version}")
    formula_reference = "path:" + str(formulae)
    config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference)}\n"
                      f"casks = {json.dumps(reference)}\nformulae = {json.dumps(formula_reference)}\n")
    formula_records = {name: dict(source="homebrew/core", token=name, name=name,
                                  description="Native Formula fixture", homepage=None, version="1",
                                  status="eligible", kind=kind, reason=None, detail=None)
                       for name, kind in (("formula-probe", "nixpkgs-equivalent"),
                                          ("formula-bottle", "formula-bottle"))}
    formula_records["formula-blocked"] = dict(formula_records["formula-probe"],
                                             token="formula-blocked", name="formula-blocked",
                                             status="excluded", kind=None, reason="install-hook")

    def formula_version(version):
        for name in ("formula-probe", "formula-bottle"):
            formula_records[name]["version"] = version
        index = dict(schema="pkg-formula-catalog/1", generator=dict(name="formula-catalog", version="0.1.0"),
                     inputs={"homebrew/core": dict(url="https://formulae.brew.sh/api/formula.json",
                                                   revision="a" * 40, sha256="b" * 64, license="BSD-2-Clause")},
                     targets=[system], macosBaseline="14", aliases={"formula-alias": "formula-probe"},
                     executables={}, systems={system: {"entries": {"homebrew/core/" + name: record
                                                                   for name, record in formula_records.items()}}})
        (formulae / "index.json").write_text(json.dumps(index))
        packages = " ".join(
            f'"homebrew/core/{name}" = builtins.derivation {{ name = "{name}-{version}"; '
            f'system = "{system}"; builder = ./builder{version}; '
            f'args = [ ./builder{version} "{name}" ]; nonce = "{root.name}"; }} '
            f'// {{ version = "{version}"; meta.description = "Formula fixture"; }};'
            for name in ("formula-probe", "formula-bottle"))
        (formulae / "flake.nix").write_text(
            '{ outputs = {self}: { formulaIndex = builtins.fromJSON (builtins.readFile ./index.json); '
            f'packages.{system} = {{ {packages} }}; }}; }}')

    formula_version("1")
    before_manifest = (profile / "manifest.json").read_bytes()
    before_link = os.readlink(profile)
    run(pkg, "install", "brew:formula-alias", "brew:formula-blocked", code=1)
    assert "install-hook" in results[-1]["stderr"], results[-1]
    assert (profile / "manifest.json").read_bytes() == before_manifest
    assert os.readlink(profile) == before_link
    registry.write_text("broken registry")
    run(pkg, "add", "brew:formula-alias", "brew:formula-probe", "brew:formula-bottle", "nixpkgs:probe")
    installed_formulae = entries()
    assert len(installed_formulae) == 3, installed_formulae
    assert {row["package_id"] for row in installed_formulae} == {
        "brew:homebrew/core/formula-probe", "brew:homebrew/core/formula-bottle", "nixpkgs:probe"
    }, installed_formulae
    info = query("info", "brew:formula-alias")
    assert info["installed_state"] == "installed" and info["formula_kind"] == "nixpkgs-equivalent", info
    run(pkg, "install", "brew:formula-alias")
    assert "Already installed" in results[-1]["stdout"], results[-1]
    registry.unlink()
    before_manifest = (profile / "manifest.json").read_bytes()
    before_link = os.readlink(profile)
    formula_records["formula-probe"].update(status="excluded", kind=None, reason="install-hook")
    formula_version("2")
    run(pkg, "upgrade", "--all", code=1)
    assert "install-hook" in results[-1]["stderr"], results[-1]
    assert (profile / "manifest.json").read_bytes() == before_manifest
    assert os.readlink(profile) == before_link
    formula_records["formula-probe"].update(status="eligible", kind="nixpkgs-equivalent", reason=None)
    formula_version("2")
    run(pkg, "upgrade", "--all")
    assert run(str(profile / "bin/formula-probe")).strip() == "2"
    assert run(str(profile / "bin/formula-bottle")).strip() == "2"
    run(pkg, "rollback")
    assert run(str(profile / "bin/formula-probe")).strip() == "1"
    run(pkg, "remove", "brew:formula-probe", "brew:homebrew/core/formula-bottle", "nixpkgs:probe")
    assert entries() == []
    print("PASS: Formula alias, native state, duplicate install, refusals, upgrade, rollback and removal")

    active_manifest = (profile / "manifest.json").read_bytes()
    active_link = os.readlink(profile)
    generation_links = {path.name: os.readlink(path)
                        for path in profile.parent.glob("pkg-*-link")}
    assert len(generation_links) > 1, generation_links
    for age in ("30d", "30D"):
        assert run(pkg, "prune", "--older-than", age) == "No old generations to prune.\n"
        assert (profile / "manifest.json").read_bytes() == active_manifest
        assert os.readlink(profile) == active_link
        assert {path.name: os.readlink(path)
                for path in profile.parent.glob("pkg-*-link")} == generation_links
    print("PASS: CLI usage refusal, JSON schema, doctor read-only state and prune retention")

    flake("1")
    run(pkg, "install", "nixpkgs:probe")
    recent_generation = os.readlink(profile)
    recent_number = recent_generation.removeprefix("pkg-").removesuffix("-link")
    flake("2")
    run(pkg, "upgrade", "nixpkgs:probe")
    before = snapshot()
    current_generation = os.readlink(profile)
    aged = set(before[2]) - {recent_generation, current_generation}
    # Nix keeps the generation that was active at the retention cutoff.
    boundary = max(aged, key=lambda name: int(name.split("-")[1]))
    expired = aged - {boundary}
    assert expired, before[2]
    old_time = time.time() - 90 * 86400
    for name in aged:
        os.utime(profile.parent / name, (old_time, old_time), follow_symlinks=False)
    pruned = run(pkg, "prune", "--older-than", "30d")
    assert pruned == f"Pruned: {len(expired)} old generation(s). Current generation kept.\n", pruned
    after = snapshot()
    assert after[:2] == before[:2]
    assert after[2] == {name: before[2][name]
                        for name in (boundary, recent_generation, current_generation)}
    assert run(str(profile / "bin/probe")).strip() == "2"
    before = snapshot()
    missing = sorted(expired)[0].removeprefix("pkg-").removesuffix("-link")
    run(pkg, "rollback", missing, code=1)
    assert snapshot() == before
    run(pkg, "rollback", recent_number)
    assert run(str(profile / "bin/probe")).strip() == "1"
    run(pkg, "rollback", current_generation.removeprefix("pkg-").removesuffix("-link"))
    assert run(str(profile / "bin/probe")).strip() == "2"
    before = snapshot()
    assert run(pkg, "prune", "--older-than", "30d") == "No old generations to prune.\n"
    assert snapshot() == before
    for name in before[2]:
        os.utime(profile.parent / name, (old_time, old_time), follow_symlinks=False)
    assert run(pkg, "prune", "--older-than", "30d") == "Pruned: 2 old generation(s). Current generation kept.\n"
    after = snapshot()
    assert after[:2] == before[:2]
    assert after[2] == {current_generation: before[2][current_generation]}
    assert run(str(profile / "bin/probe")).strip() == "2"
    assert run(pkg, "prune", "--older-than", "30d") == "No old generations to prune.\n"
    assert snapshot() == after
    run(pkg, "remove", "nixpkgs:probe")
    assert entries() == []
    print("PASS: expired history removed, recent rollback works and an old current generation stays usable")

    if platform.system() == "Darwin":
        host = subprocess.check_output(["/usr/bin/sw_vers", "-productVersion"], text=True).strip()
        major = int(host.split(".")[0])
        casks = root / "casks"
        casks.mkdir()
        for version in ("1", "2"):
            shutil.copy2(source / f"builder{version}", casks / f"builder{version}")
        fixture = Path(__file__).resolve().parent.parent / "crates/pkg-cli/tests/fixtures/catalog-index.json"
        index = json.loads(fixture.read_text())
        record = index["systems"][system]["entries"]["homebrew/cask/1password-cli"].copy()
        record.update(token="os-probe", name="OS probe", version="1", minMacos=None, maxMacos=None)
        index["targets"] = [system]
        index["systems"] = {system: {"entries": {"homebrew/cask/os-probe": record}}}

        def cask_version(version):
            (casks / "flake.nix").write_text(
                '{ outputs = {self}: { catalogIndex = builtins.fromJSON (builtins.readFile ./index.json); '
                f'packages.{system}."homebrew/cask/os-probe" = builtins.derivation {{ '
                f'name = "os-probe-{version}"; system = "{system}"; builder = ./builder{version}; '
                f'args = [ ./builder{version} "os-probe" ]; nonce = "{root.name}"; }}; }}; }}')

        def bounds(lo, hi):
            record.update(minMacos=lo, maxMacos=hi)
            (casks / "index.json").write_text(json.dumps(index))

        config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference)}\n"
                          f"casks = {json.dumps('path:' + str(casks))}\n")
        cask_version("1")
        before_manifest = (profile / "manifest.json").read_bytes()
        before_link = os.readlink(profile)
        for lo, hi in ((None, str(major - 1)), (str(major + 1), None),
                       ("99999999999999999999", None)):
            bounds(lo, hi)
            run(pkg, "install", "cask:homebrew/cask/os-probe", code=1)
            assert (profile / "manifest.json").read_bytes() == before_manifest
            assert os.readlink(profile) == before_link
        bounds("0", None)
        run(pkg, "install", "cask:homebrew/cask/os-probe")
        assert run(str(profile / "bin/os-probe")).strip() == "1"
        before_manifest = (profile / "manifest.json").read_bytes()
        before_link = os.readlink(profile)
        # A changed default source must not replace an installed source.
        other = root / "other-casks"
        other.mkdir()
        (other / "flake.nix").write_text("{ outputs = {self}: {}; }")
        config.write_text(f"[sources]\nnixpkgs = {json.dumps(reference)}\n"
                          f"casks = {json.dumps('path:' + str(other))}\n")
        bounds(None, str(major - 1))
        run(pkg, "upgrade", "--all", code=1)
        assert f"macOS {host}" in results[-1]["stderr"], results[-1]
        assert (profile / "manifest.json").read_bytes() == before_manifest
        assert os.readlink(profile) == before_link
        bounds("0", None)
        cask_version("2")
        run(pkg, "upgrade", "--all")
        assert run(str(profile / "bin/os-probe")).strip() == "2"
        run(pkg, "remove", entries()[0]["entry_id"])
        assert entries() == []
        print("PASS: macOS bounds and upgrades through the original cask source")
        run(pkg, "install", "nixpkgs:app-probe", timeout=600)
        helper = root / "state/nix/profiles/pkg-app-tools/bin/mac-app-util"
        run(str(helper), "--help")
        run(pkg, "apps", "sync")
        launcher = Path(env["HOME"]) / "Applications/pkg/AppProbe.app"
        assert launcher.is_dir(), launcher
        run(pkg, "remove", entries()[0]["entry_id"])
        assert not launcher.exists()
        assert entries() == []
        print("PASS: macOS helper build, startup, launcher sync and removal")
    print("PASS: native lifecycle, mutable source, namespace choices, tap isolation, cancellation")
    if not args.evidence_dir:
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
