#!/usr/bin/env python3
"""Measure native launcher sync at several app counts in temporary homes."""

import argparse
import importlib.util
import os
import platform
import shutil
import subprocess
import tempfile
from pathlib import Path

BUILDER = r"""
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
int main(int argc, char **argv) {
    const char *out = getenv("out");
    if (!out) { puts("pkg app scale fixture 1"); return 0; }
    const char *count = getenv("appCount");
    if (argc != 3 || !count) return 2;
    char path[4096], binary[4096];
    if (mkdir(out, 0755)) return 3;
    snprintf(path, sizeof(path), "%s/bin", out);
    if (mkdir(path, 0755)) return 4;
    snprintf(binary, sizeof(binary), "%s/bin/%s", out, argv[2]);
    FILE *from = fopen(argv[1], "rb"), *to = fopen(binary, "wb");
    if (!from || !to) return 5;
    char buf[8192]; size_t n;
    while ((n = fread(buf, 1, sizeof(buf), from)))
        if (fwrite(buf, 1, n, to) != n) return 6;
    if (ferror(from) || fclose(to) || fclose(from)) return 7;
    if (chmod(binary, 0755)) return 8;
    snprintf(path, sizeof(path), "%s/Applications", out);
    if (mkdir(path, 0755)) return 9;
    for (int i = 0; i < atoi(count); i++) {
        snprintf(path, sizeof(path), "%s/Applications/PkgSpeedProbe%03d.app", out, i);
        if (mkdir(path, 0755)) return 10;
        snprintf(path, sizeof(path), "%s/Applications/PkgSpeedProbe%03d.app/Contents", out, i);
        if (mkdir(path, 0755)) return 11;
        snprintf(path, sizeof(path), "%s/Applications/PkgSpeedProbe%03d.app/Contents/MacOS", out, i);
        if (mkdir(path, 0755)) return 12;
        snprintf(path, sizeof(path), "%s/Applications/PkgSpeedProbe%03d.app/Contents/MacOS/probe", out, i);
        if (link(binary, path)) return 13;
        snprintf(path, sizeof(path), "%s/Applications/PkgSpeedProbe%03d.app/Contents/Info.plist", out, i);
        FILE *plist = fopen(path, "w");
        if (!plist) return 14;
        fprintf(plist, "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>"
            "<key>CFBundleIdentifier</key><string>dev.pkg.speed.probe%03d</string>"
            "<key>CFBundleExecutable</key><string>probe</string>"
            "<key>CFBundleName</key><string>PkgSpeedProbe%03d</string>"
            "<key>CFBundlePackageType</key><string>APPL</string>"
            "<key>CFBundleVersion</key><string>1</string></dict></plist>", i, i);
        if (fclose(plist)) return 15;
    }
    return 0;
}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--pkg", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 10, 30])
    parser.add_argument(
        "--runs", type=int, default=4, help="measured pairs; even counts balance order"
    )
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("native launcher scaling requires macOS")
    if args.runs < 1 or any(count < 1 or count > 100 for count in args.counts):
        parser.error("runs must be positive and app counts must be 1 through 100")
    spec = importlib.util.spec_from_file_location(
        "speed", args.repo / "tools/pkg-speed-check.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    root = Path(tempfile.mkdtemp(prefix="pkg-app-scale-"))
    bench = module.Bench(args, root)
    report = bench.report
    report["scope"] = (
        "native launcher sync with real Nix fixture apps and private homes"
    )
    report["limits"] = [
        "Fixture apps measure launcher count, not vendor payload extraction.",
        "One excluded warmup and alternating measured samples per case.",
        "Helper setup and package builds occur outside sync timing.",
        "The shared Nix store and source metadata are warm.",
        "App GUIs are not launched. Fixture commands run after timing.",
    ]
    report["fixture_builder"] = BUILDER
    report["script"] = Path(__file__).read_text()
    builder = bench.source / "app-builder.c"
    builder.write_text(BUILDER)
    subprocess.run(
        [
            shutil.which("cc") or "cc",
            str(builder),
            "-o",
            str(bench.source / "app-builder"),
        ],
        check=True,
    )
    packages = []
    for count in sorted(set(args.counts)):
        name = f"appset-{count}"
        packages.append(
            f'{name} = builtins.derivation {{ name = "{name}-1"; '
            f'system = "{bench.system}"; builder = ./app-builder; '
            f'args = [ ./app-builder "{name}" ]; appCount = "{count}"; }} '
            f'// {{ pname = "{name}"; version = "1"; meta.description = "App scale fixture"; }};'
        )
    (bench.source / "flake.nix").write_text(
        "{ outputs = { self }: { packages."
        + bench.system
        + " = { "
        + " ".join(packages)
        + " }; }; }"
    )
    subprocess.run(["git", "-C", str(bench.source), "add", "."], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(bench.source),
            "-c",
            "user.name=pkg benchmark",
            "-c",
            "user.email=benchmark@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "app count fixture",
        ],
        check=True,
    )
    try:
        for count in args.counts:
            snapshots = {}
            for label in bench.binaries:
                bench.observe(
                    label,
                    f"install_{count}_apps",
                    ["install", f"nixpkgs:appset-{count}"],
                )
                env = bench.envs[label]
                profile = Path(env["XDG_STATE_HOME"]) / "nix/profiles/pkg"
                apps = list((profile / "Applications").glob("*.app"))
                assert len(apps) == count
                command = [str(profile / f"bin/appset-{count}"), "--version"]
                result = subprocess.run(
                    command, env=env, capture_output=True, text=True, check=True
                )
                assert result.stdout == "pkg app scale fixture 1\n"
                snapshots[label] = (bench.query(label, ["list"]), os.readlink(profile))
                destination = Path(env["HOME"]) / "Applications/pkg"
                assert len(list(destination.glob("*.app"))) == count
            bench.measure(f"apps_sync_{count}", ["apps", "sync"], module.nonempty)
            for label in bench.binaries:
                env = bench.envs[label]
                profile = Path(env["XDG_STATE_HOME"]) / "nix/profiles/pkg"
                rows, generation = snapshots[label]
                assert bench.query(label, ["list"]) == rows
                assert os.readlink(profile) == generation
                health = bench.query(label, ["doctor"])
                app_health = [
                    row for row in health if row["component"] == "app launchers"
                ]
                assert len(app_health) == 1 and app_health[0]["status"] == "healthy", (
                    health
                )
                destination = Path(env["HOME"]) / "Applications/pkg"
                for index in range(count):
                    info = (
                        destination
                        / f"PkgSpeedProbe{index:03d}.app/Contents/Info.plist"
                    )
                    subprocess.run(
                        ["/usr/bin/plutil", "-lint", str(info)],
                        capture_output=True,
                        check=True,
                    )
                    value = subprocess.check_output(
                        [
                            "/usr/bin/plutil",
                            "-extract",
                            "CFBundleIdentifier",
                            "raw",
                            str(info),
                        ],
                        text=True,
                    )
                    assert value.strip() == f"dev.pkg.speed.probe{index:03d}"
                bench.observe(
                    label, f"remove_{count}_apps", ["remove", f"nixpkgs:appset-{count}"]
                )
                assert bench.query(label, ["list"]) == []
                assert not list(destination.glob("*.app"))
                report["observations"].append(
                    {
                        "binary": label,
                        "name": f"validated_{count}_apps",
                        "launcher_count": count,
                        "healthy": True,
                        "profile_unchanged_by_sync": True,
                        "empty_after_remove": True,
                    }
                )
                bench.save()
        report["complete"] = True
        bench.save()
    finally:
        if report["complete"]:
            shutil.rmtree(root)
        else:
            report["retained_root"] = str(root)
            bench.save()


if __name__ == "__main__":
    main()
