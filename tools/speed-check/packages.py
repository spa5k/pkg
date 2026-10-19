"""Package install checks: repeated batches, repeat behavior, real packages, and apps."""

import json
import os
import platform
import shutil
import subprocess
import tempfile
from pathlib import Path

from common import (
    NIXPKGS,
    Bench,
    balanced,
    commit,
    compile_c,
    nix_command,
    nonempty,
    pinned_catalogs,
    write_sources,
)

OUTPUTS_BUILDER = r"""
#include <stdlib.h>
#include <sys/stat.h>
int main(void) {
    const char *names[] = {"out", "dev"};
    for (int i = 0; i < 2; i++) {
        const char *path = getenv(names[i]);
        if (path && mkdir(path, 0755)) return 1;
    }
    return 0;
}
"""

APP_BUILDER = r"""
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

REAL_CASES = [
    "nix_native",
    "formula_equivalent",
    "formula_bottle",
    "cask_dmg",
    "cask_zip",
]


def register(checks, binaries):
    check = checks.add_parser(
        "batch",
        parents=[binaries],
        help="repeated installs of already installed packages",
        description="Measure repeated installs of 1, 10, and 100 packages in real Nix profiles.",
    )
    check.add_argument("--counts", type=int, nargs="+", default=[1, 10, 100])
    check.set_defaults(run=batch, runs=4)

    check = checks.add_parser(
        "repeat-behavior",
        parents=[binaries],
        help="compare repeated-install behavior in edge cases",
        description="Compare repeated-install behavior with real Nix in disposable profiles. Timings are observations.",
    )
    check.add_argument("--single", action="store_true", help="use one package per case")
    check.set_defaults(run=repeat_behavior, runs=1)

    check = checks.add_parser(
        "real-packages",
        parents=[binaries],
        help="real pinned Nix, Formula, and Cask installs",
        description=(
            "Measure real pinned Nix, Formula, and Cask installs in temporary profiles. "
            "Payloads download into the shared Nix store; first fetches are observations. "
            "On macOS, app launchers are created and removed under a temporary HOME."
        ),
    )
    check.add_argument(
        "--fresh-home-per-install",
        action="store_true",
        help="use fresh HOME, XDG paths, profile, and app cache for every install",
    )
    check.add_argument("--case", action="append", choices=REAL_CASES)
    check.add_argument("--zip-package", default="iterm2")
    check.add_argument(
        "--extra-cask",
        action="append",
        nargs=3,
        default=[],
        metavar=("TOKEN", "COMMAND", "CHECK"),
        help="add a Cask; CHECK is version, version-word, help, or app; use '-' for an app command",
    )
    check.add_argument(
        "--extra-only", action="store_true", help="run only --extra-cask cases"
    )
    check.set_defaults(run=real_packages, runs=4)

    check = checks.add_parser(
        "apps",
        parents=[binaries],
        help="macOS launcher sync at several app counts",
        description="Measure native launcher sync at several app counts in temporary homes.",
    )
    check.add_argument("--counts", type=int, nargs="+", default=[1, 10, 30])
    check.set_defaults(run=apps, runs=4)


def batch(args):
    if args.counts != sorted(set(args.counts)) or args.counts[0] < 1:
        args.error("--counts must be positive and strictly increasing")
    with tempfile.TemporaryDirectory(prefix="pkg-batch-speed-") as temporary:
        bench = Bench(args, Path(temporary), package_count=max(128, args.counts[-1]))
        bench.report["scope"] = (
            "store-warm repeated installs; exact profile state checked"
        )
        try:
            for count in args.counts:
                packages = [f"nixpkgs:probe-{number:03d}" for number in range(count)]
                before = {}
                for label in bench.binaries:
                    bench.observe(label, f"prepare_{count}", ["install", *packages])
                    rows = bench.query(label, ["list"])
                    assert len(rows) == count
                    before[label] = rows, os.readlink(bench.profile(label))

                def validate(output, count=count):
                    assert output.count("Already installed:") == count, output
                    assert "Installed:" not in output, output

                bench.measure(f"repeat_batch_{count}", ["install", *packages], validate)
                for label in bench.binaries:
                    after = (
                        bench.query(label, ["list"]),
                        os.readlink(bench.profile(label)),
                    )
                    assert after == before[label]
                bench.report.setdefault("profile_checks", {})[str(count)] = {
                    "same_rows": True,
                    "same_generations": True,
                }
        finally:
            for label in bench.binaries:
                bench.clear(label)
        bench.report["complete"] = True
        bench.save()


def repeat_behavior(args):
    with tempfile.TemporaryDirectory(prefix="pkg-repeat-check-") as temporary:
        root = Path(temporary)
        bench = Bench(args, root)
        bench.report["scope"] = "native behavior comparison; timings are observations"
        source = bench.source
        (source / "outputs.c").write_text(OUTPUTS_BUILDER)
        compile_c(source / "outputs.c", source / "outputs", static=True)
        base = """{ outputs = {self}: let
  make = name: builtins.derivation {
    name = name + "-1"; system = SYSTEM; builder = ./outputs;
    outputs = [ "out" "dev" ];
  } // { meta.description = "Output fixture"; META };
  packages = { alpha = make "alpha"; beta = make "beta"; nested = { alpha = make "nested-alpha"; beta = make "nested-beta"; }; };
in { packages.SYSTEM = packages; EXTRA }; }
""".replace("SYSTEM", json.dumps(bench.system))
        checks = []

        def write(extra="", meta=""):
            (source / "flake.nix").write_text(
                base.replace("EXTRA", extra).replace("META", meta)
            )
            commit(source, "Output fixture")

        def configure(path):
            for env in bench.envs.values():
                write_sources(env, {"nixpkgs": f"git+file://{path}"})

        def state(label):
            result = subprocess.run(
                nix_command(
                    "profile", "list", "--profile", str(bench.profile(label)), "--json"
                ),
                env=bench.envs[label],
                capture_output=True,
                text=True,
                check=True,
            )
            return json.loads(result.stdout), os.readlink(bench.profile(label))

        def clear():
            for label in bench.binaries:
                if bench.query(label, ["list"]):
                    entries = state(label)[0]["elements"]
                    bench.observe(label, "cleanup", ["remove", "--entry", *entries])

        def check(
            name,
            prepare,
            repeat,
            already=True,
            collision=False,
            code=0,
            native_suffix=None,
        ):
            outputs = []
            for label in bench.binaries:
                if native_suffix is None:
                    bench.observe(label, name + "_prepare", ["install", *prepare])
                else:
                    selected = [
                        f"git+file://{source}#{package.removeprefix('nixpkgs:')}{native_suffix}"
                        for package in prepare
                    ]
                    subprocess.run(
                        nix_command(
                            "profile",
                            "add",
                            "--profile",
                            str(bench.profile(label)),
                            "--",
                            *selected,
                        ),
                        env=bench.envs[label],
                        capture_output=True,
                        text=True,
                        check=True,
                    )
                before = state(label)
                if collision:
                    assert len(before[0]["elements"]) == 2 * len(prepare), before
                elapsed, stdout, stderr = bench.invoke(
                    label, ["install", *repeat], code=code
                )
                assert state(label) == before, (name, label, before)
                if already:
                    assert stdout.count("Already installed:") == len(repeat), stdout
                else:
                    assert "Already installed:" not in stdout, stdout
                outputs.append(stdout)
                checks.append(
                    {
                        "case": name,
                        "binary": label,
                        "command": ["install", *repeat],
                        "code": code,
                        "native_prepare_outputs": native_suffix,
                        "elapsed_ms": elapsed,
                        "stdout": stdout,
                        "stderr": stderr,
                        "same_rows": True,
                        "same_generation": True,
                        "entries": len(before[0]["elements"]),
                    }
                )
            assert outputs[0] == outputs[1], (name, outputs)
            clear()

        try:
            write()
            configure(source)
            plain = ["nixpkgs:alpha", "nixpkgs:beta"][: 1 if args.single else 2]
            check("default_outputs", plain, plain)
            for suffix in ["^dev", "^*"]:
                check(
                    "existing_outputs_" + suffix,
                    plain,
                    plain,
                    already=False,
                    native_suffix=suffix,
                )
                check(
                    "refused_suffix_" + suffix,
                    plain,
                    [package + suffix for package in plain],
                    already=False,
                    code=1,
                )
            check(
                "refused_quoted_attribute",
                plain,
                ['nixpkgs:"dotted.name"', "nixpkgs:alpha"],
                already=False,
                code=1,
            )
            nested = ["nixpkgs:nested.alpha", "nixpkgs:nested.beta"][
                : 1 if args.single else 2
            ]
            check("nested_attributes", nested, nested)
            second = root / "second"
            shutil.copytree(source, second)
            for label in bench.binaries:
                bench.observe(label, "collision_first_source", ["install", *plain])
            configure(second)
            check("same_name_different_sources", plain, plain, collision=True)
            configure(source)
            write(extra="legacyPackages." + json.dumps(bench.system) + " = packages;")
            check("ambiguous_roots", plain, plain, already=False)
            write(meta='meta.outputsToInstall = [ "dev" "out" ];')
            check("multiple_default_outputs", plain, plain)
        finally:
            clear()
            bench.report["checks"] = checks
            bench.save()
        bench.report["complete"] = True
        bench.save()
        print(f"Passed {len(checks)} native behavior checks.")


def real_packages(args):
    if args.extra_only and not args.extra_cask:
        args.error("--extra-only needs --extra-cask")
    for token, command, check in args.extra_cask:
        if check not in {"version", "version-word", "help", "app"}:
            args.error("Cask CHECK must be version, version-word, help, or app")
        if not token or "/" in token or not command or "/" in command:
            args.error("extra Cask tokens and commands must be single names")
        if check == "app" and platform.system() != "Darwin":
            args.error("app checks need macOS")
    with tempfile.TemporaryDirectory(prefix="pkg-real-package-speed-") as temporary:
        root = Path(temporary)
        bench = Bench(args, root)
        report = bench.report
        report["scope"] = (
            "real pinned packages; fresh HOME, XDG paths, profile, and app cache per install; shared store warm after initial observations"
            if args.fresh_home_per_install
            else "real pinned packages; first store fetch observations and repeated store-warm profile changes"
        )
        references = pinned_catalogs(root / "real-sources")
        report["references"] = references
        app_roots = []

        def environment(path):
            env = bench.environment(path, references)
            if platform.system() == "Darwin":
                parent = Path("/nix/var/pkg/cask-apps") / str(os.getuid())
                assert parent.is_dir(), "run pkg apps setup once on this Mac"
                app = tempfile.TemporaryDirectory(prefix="pkg-speed-real-", dir=parent)
                app_roots.append(app)
                env["PKG_APP_CACHE_DIR"] = app.name
            return env

        bench.envs = {
            label: environment(root / f"real-{label}") for label in bench.binaries
        }
        packages = [
            ("nix_native", NIXPKGS + "#hello", "hello"),
            ("formula_equivalent", "brew:jq", "jq"),
        ]
        if platform.system() == "Darwin":
            packages += [
                ("formula_bottle", "brew:hello", "hello"),
                ("cask_dmg", "cask:caffeine", None),
                ("cask_zip", "cask:" + args.zip_package, None),
            ]
        if args.case:
            packages = [package for package in packages if package[0] in args.case]
            assert packages, "the requested cases do not apply to this system"
        if args.extra_only:
            packages = []
        check_args = {}
        for token, command, check in args.extra_cask:
            name = "cask_extra_" + token
            assert all(package[0] != name for package in packages), (
                "duplicate extra Cask"
            )
            packages.append(
                (name, "cask:" + token, None if check == "app" else command)
            )
            check_args[name] = {"help": "--help", "version-word": "version"}.get(
                check, "--version"
            )

        def call(label, name, command):
            elapsed, result = bench.run(label, command, timeout=1800)
            report["observations"].append(
                {
                    "binary": label,
                    "name": name,
                    "command": command,
                    "elapsed_ms": elapsed,
                    "code": result.returncode,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                }
            )
            bench.save()
            return result, elapsed

        def remove(label):
            result, _ = call(label, "cleanup_list", ["list", "--json"])
            if result.returncode:
                return
            rows = json.loads(result.stdout)["result"]
            if rows:
                result, _ = call(
                    label,
                    "cleanup_remove",
                    ["remove", *[row["package_id"] for row in rows]],
                )
                assert result.returncode == 0, result.stderr

        def executable_check(label, name, executable):
            binary = bench.profile(label) / "bin" / executable
            try:
                checked = subprocess.run(
                    [str(binary), check_args.get(name, "--version")],
                    env=bench.envs[label],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                )
                result = {
                    "code": checked.returncode,
                    "stdout": checked.stdout,
                    "stderr": checked.stderr,
                }
            except subprocess.TimeoutExpired:
                result = {
                    "code": "timeout",
                    "stdout": "",
                    "stderr": "executable check exceeded 30 seconds",
                }
            except OSError as error:
                result = {"code": "spawn_error", "stdout": "", "stderr": str(error)}
            report["observations"].append(
                {"binary": label, "name": name + "_executable_check", **result}
            )
            return result["code"] == 0

        def check_apps(label, name):
            profile = bench.profile(label)
            found = {
                p.name: {"source": str(p)}
                for p in (profile / "Applications").glob("*.app")
            }
            for manifest in (profile / "libexec/pkg-casks").glob("*/apps.json"):
                data = json.loads(manifest.read_text())
                assert data["schema"] == "pkg-cask-apps/2"
                output = manifest.resolve().parents[3]
                for app, entry in data["apps"].items():
                    restored = (
                        Path(bench.envs[label]["PKG_APP_CACHE_DIR"]) / output.name / app
                    )
                    assert restored.is_dir(), restored
                    subprocess.run(
                        [
                            "/usr/bin/codesign",
                            "--verify",
                            "--deep",
                            "--strict",
                            str(restored),
                        ],
                        capture_output=True,
                        text=True,
                        timeout=120,
                        check=True,
                    )
                    found[app] = {
                        "source": str(output / entry["source"]),
                        "restored": str(restored),
                        "signature_files": len(entry.get("signatures", {})),
                    }
            assert found, "no app bundle found"
            for app, entry in found.items():
                launcher = Path(bench.envs[label]["HOME"]) / "Applications/pkg" / app
                assert launcher.is_dir(), launcher
                entry["launcher"] = str(launcher)
            report["observations"].append(
                {"binary": label, "name": name + "_app_validation", "apps": found}
            )

        try:
            for name, package, executable in packages:
                samples = {label: [] for label in bench.binaries}
                failed = False
                for iteration, label in balanced(bench.binaries, args.runs):
                    if failed:
                        break
                    if args.fresh_home_per_install:
                        bench.envs[label] = environment(
                            root / f"{name}-{label}-{iteration}"
                        )
                    if not iteration:
                        phase = "_first_store_may_be_cold"
                    elif args.fresh_home_per_install:
                        phase = "_fresh_home_install"
                    else:
                        phase = "_warm_install"
                    result, elapsed = call(label, name + phase, ["install", package])
                    if result.returncode:
                        failed = True
                        report["limits"].append(
                            f"{name} {label} install failed; see the retained output"
                        )
                        remove(label)
                        continue
                    listed = bench.query(label, ["list"])
                    assert len(listed) == 1, listed
                    if executable:
                        if not executable_check(label, name, executable):
                            failed = True
                            report["limits"].append(
                                f"{name} {label} executable check failed; see retained output"
                            )
                            remove(label)
                            continue
                    else:
                        check_apps(label, name)
                    if iteration:
                        samples[label].append(elapsed)
                    result, _ = call(
                        label, name + "_remove", ["remove", listed[0]["package_id"]]
                    )
                    assert result.returncode == 0, result.stderr
                    assert bench.query(label, ["list"]) == []
                bench.record(
                    name,
                    samples,
                    status="failed" if failed else "passed",
                    package=package,
                )
        finally:
            for label in bench.binaries:
                remove(label)
            for app in app_roots:
                app.cleanup()
        report["complete"] = True
        bench.save()


def apps(args):
    if platform.system() != "Darwin":
        args.error("native launcher scaling requires macOS")
    if any(count < 1 or count > 100 for count in args.counts):
        args.error("app counts must be 1 through 100")
    root = Path(tempfile.mkdtemp(prefix="pkg-app-scale-"))
    bench = Bench(args, root)
    report = bench.report
    report["scope"] = (
        "native launcher sync with real Nix fixture apps and private homes"
    )
    report["limits"] = [
        "Fixture apps measure launcher count, not vendor payload extraction.",
        "Helper setup and package builds occur outside sync timing.",
        "The shared Nix store and source metadata are warm.",
        "App GUIs are not launched. Fixture commands run after timing.",
    ]
    (bench.source / "app-builder.c").write_text(APP_BUILDER)
    compile_c(bench.source / "app-builder.c", bench.source / "app-builder")
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
    commit(bench.source, "App count fixture")
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
                profile = bench.profile(label)
                assert len(list((profile / "Applications").glob("*.app"))) == count
                result = subprocess.run(
                    [str(profile / f"bin/appset-{count}"), "--version"],
                    env=env,
                    capture_output=True,
                    text=True,
                    check=True,
                )
                assert result.stdout == "pkg app scale fixture 1\n"
                snapshots[label] = bench.query(label, ["list"]), os.readlink(profile)
                destination = Path(env["HOME"]) / "Applications/pkg"
                assert len(list(destination.glob("*.app"))) == count
            bench.measure(f"apps_sync_{count}", ["apps", "sync"], nonempty)
            for label in bench.binaries:
                env = bench.envs[label]
                profile = bench.profile(label)
                assert (
                    bench.query(label, ["list"]),
                    os.readlink(profile),
                ) == snapshots[label]
                health = [
                    row
                    for row in bench.query(label, ["doctor"])
                    if row["component"] == "app launchers"
                ]
                assert len(health) == 1 and health[0]["status"] == "healthy", health
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
