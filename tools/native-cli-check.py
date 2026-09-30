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
    args = parser.parse_args()
    pkg = str(args.pkg.resolve(strict=True))
    nix = shutil.which("nix")
    assert nix, "Nix is required; this check must not skip"
    system = {("Linux", "x86_64"): "x86_64-linux",
              ("Darwin", "arm64"): "aarch64-darwin"}[
                  (platform.system(), platform.machine())]
    root = Path(tempfile.mkdtemp(prefix="pkg-native-"))
    print(f"Evidence: {root}", flush=True)
    env = dict(os.environ)
    # Keep the real HOME. Only pkg's three XDG bases are isolated.
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
        result = subprocess.run(command, env=env, text=True, capture_output=True,
                                timeout=timeout)
        results.append(dict(command=command, code=result.returncode,
                            stdout=result.stdout, stderr=result.stderr))
        (root / "results.json").write_text(json.dumps(results, indent=2))
        assert result.returncode == code, results[-1]
        return result.stdout

    def flake(version, collision=False):
        def drv(name, v):
            return (f'builtins.derivation {{ name = "{name}-{v}"; system = "{system}"; '
                    f'builder = ./builder{v}; args = [ ./builder{v} "{name}" ]; '
                    f'nonce = "{root.name}"; }} '
                    f'// {{ version = "{v}"; pname = "{name}"; '
                    'meta.description = "native pkg regression fixture"; }')
        (source / "flake.nix").write_text(
            '{ outputs = {self}: { packages.' + system + ' = { probe = '
            + drv("probe", version) + '; slow = ' + drv("slow", version)
            + '; app-probe = ' + drv("app-probe", version) + '; };'
            + (' legacyPackages.' + system + '.probe = ' + drv("probe", "1") + ';'
               if collision else '') + ' }; }')

    def entries():
        return json.loads(run(pkg, "--json", "list"))["result"]

    flake("1")
    run(pkg, "install", "nixpkgs:probe")
    assert run(str(profile / "bin/probe")).strip() == "1"
    flake("2")
    run(pkg, "upgrade", "--all")
    assert run(str(profile / "bin/probe")).strip() == "2"
    run(pkg, "rollback")
    assert run(str(profile / "bin/probe")).strip() == "1"
    run(pkg, "remove", entries()[0]["entry_id"])
    assert entries() == []

    flake("2", collision=True)
    message = run(pkg, "info", "nixpkgs:probe", code=1)
    diagnostic = results[-1]["stderr"] + message
    for namespace, version in (("packages", "2"), ("legacyPackages", "1")):
        attribute = f"{namespace}.{system}.probe"
        assert attribute in diagnostic, diagnostic
        info = json.loads(run(pkg, "--json", "info", "nixpkgs:" + attribute))["result"]
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
    doctor = json.loads(run(pkg, "--json", "doctor", code=1))["result"]
    assert "fixture/broken" in json.dumps(doctor), doctor
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
                    if "building " in text:
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
        launcher = Path.home() / "Applications/pkg/AppProbe.app"
        assert launcher.is_dir(), launcher
        run(pkg, "remove", entries()[0]["entry_id"])
        assert not launcher.exists()
        assert entries() == []
        print("PASS: macOS helper build, startup, launcher sync and removal")
    print("PASS: native lifecycle, mutable source, namespace choices, tap isolation, cancellation")
    shutil.rmtree(root)


if __name__ == "__main__":
    main()
