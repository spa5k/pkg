#!/usr/bin/env python3
"""Compare repeated-install behavior with real Nix in disposable profiles."""

import argparse
import importlib.util
import json
import pathlib
import shutil
import subprocess
import tempfile

BUILDER = r"""
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


def commit(source):
    subprocess.run(["git", "-C", str(source), "add", "."], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "-c",
            "user.name=pkg check",
            "-c",
            "user.email=check@example.com",
            "commit",
            "--quiet",
            "-m",
            "Fixture",
        ],
        check=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, required=True)
    parser.add_argument("--pkg", type=pathlib.Path, required=True)
    parser.add_argument("--baseline", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--single", action="store_true")
    args = parser.parse_args()
    args.runs = 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location(
        "speed", args.repo / "tools/pkg-speed-check.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    with tempfile.TemporaryDirectory(prefix="pkg-repeat-check-") as temporary:
        root = pathlib.Path(temporary)
        bench = module.Bench(args, root)
        bench.report["scope"] = "native behavior comparison; timings are observations"
        source = bench.source
        (source / "outputs.c").write_text(BUILDER)
        compiler = [shutil.which("cc") or "cc"]
        if bench.system.endswith("linux"):
            compiler.append("-static")
        subprocess.run(
            compiler + [str(source / "outputs.c"), "-o", str(source / "outputs")],
            check=True,
        )
        base = """{ outputs = {self}: let
  make = name: builtins.derivation {
    name = name + "-1"; system = SYSTEM; builder = ./outputs;
    outputs = [ "out" "dev" ];
  } // { meta.description = "Output fixture"; META };
  packages = { alpha = make "alpha"; beta = make "beta"; nested = { alpha = make "nested-alpha"; beta = make "nested-beta"; }; };
in { packages.SYSTEM = packages; EXTRA }; }
""".replace("SYSTEM", json.dumps(bench.system))
        nix = [
            shutil.which("nix"),
            "--extra-experimental-features",
            "nix-command flakes",
        ]
        checks = []

        def write(extra="", meta=""):
            (source / "flake.nix").write_text(
                base.replace("EXTRA", extra).replace("META", meta)
            )
            commit(source)

        def configure(path):
            for env in bench.envs.values():
                config = pathlib.Path(env["XDG_CONFIG_HOME"]) / "pkg/config.toml"
                config.write_text(
                    "[sources]\nnixpkgs = "
                    + json.dumps("git+file://" + str(path))
                    + "\n"
                )

        def state(label):
            env = bench.envs[label]
            profile = pathlib.Path(env["XDG_STATE_HOME"]) / "nix/profiles/pkg"
            result = subprocess.run(
                nix + ["profile", "list", "--profile", str(profile), "--json"],
                env=env,
                capture_output=True,
                text=True,
                check=True,
            )
            return json.loads(result.stdout), profile.readlink().as_posix()

        def clear():
            for label in bench.binaries:
                rows = bench.query(label, ["list"])
                if rows:
                    bench.observe(
                        label,
                        "cleanup",
                        ["remove", "--entry", *state(label)[0]["elements"]],
                    )

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
                    profile = (
                        pathlib.Path(bench.envs[label]["XDG_STATE_HOME"])
                        / "nix/profiles/pkg"
                    )
                    selected = [
                        "git+file://"
                        + str(source)
                        + "#"
                        + package.removeprefix("nixpkgs:")
                        + native_suffix
                        for package in prepare
                    ]
                    subprocess.run(
                        nix
                        + [
                            "profile",
                            "add",
                            "--profile",
                            str(profile),
                            "--",
                            *selected,
                        ],
                        env=bench.envs[label],
                        capture_output=True,
                        text=True,
                        check=True,
                    )
                before, generation = state(label)
                if collision:
                    assert len(before["elements"]) == 2 * len(prepare), before
                elapsed, stdout, stderr = bench.invoke(
                    label, ["install", *repeat], code=code
                )
                after, after_generation = state(label)
                assert before == after, (name, label, before, after)
                assert generation == after_generation, (
                    name,
                    label,
                    generation,
                    after_generation,
                )
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
                        "entries": len(after["elements"]),
                    }
                )
            assert outputs[0] == outputs[1], (name, outputs)
            clear()

        try:
            write()
            configure(source)
            plain = ["nixpkgs:alpha", "nixpkgs:beta"]
            if args.single:
                plain = plain[:1]
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
            nested = ["nixpkgs:nested.alpha", "nixpkgs:nested.beta"]
            if args.single:
                nested = nested[:1]
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
            check(
                "multiple_default_outputs",
                plain,
                plain,
            )
        finally:
            clear()
            bench.report["checks"] = checks
            bench.save()
        bench.report["complete"] = True
        bench.save()
        print(f"Passed {len(checks)} native behavior checks.")


if __name__ == "__main__":
    main()
