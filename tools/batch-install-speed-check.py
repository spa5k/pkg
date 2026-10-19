#!/usr/bin/env python3
"""Measure repeated installs of 1, 10, and 100 packages in real Nix profiles."""

import argparse
import importlib.util
import pathlib
import shutil
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, required=True)
    parser.add_argument("--pkg", type=pathlib.Path, required=True)
    parser.add_argument("--baseline", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument(
        "--runs", type=int, default=4, help="measured pairs; even counts balance order"
    )
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 10, 100])
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if args.counts != sorted(set(args.counts)) or args.counts[0] < 1:
        parser.error("--counts must be positive and strictly increasing")
    args.package_count = max(128, max(args.counts))
    spec = importlib.util.spec_from_file_location(
        "speed", args.repo / "tools/pkg-speed-check.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    root = pathlib.Path(tempfile.mkdtemp(prefix="pkg-batch-speed-"))
    bench = module.Bench(args, root)
    bench.report["scope"] = "store-warm repeated installs; exact profile state checked"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        for count in args.counts:
            packages = [f"nixpkgs:probe-{index:03d}" for index in range(count)]
            before = {}
            generations = {}
            for label in bench.binaries:
                bench.observe(label, f"prepare_{count}", ["install", *packages])
                before[label] = bench.query(label, ["list"])
                assert len(before[label]) == count
                profile = (
                    pathlib.Path(bench.envs[label]["XDG_STATE_HOME"])
                    / "nix/profiles/pkg"
                )
                generations[label] = profile.readlink().as_posix()

            def validate(output, count=count):
                assert output.count("Already installed:") == count, output
                assert "Installed:" not in output, output

            bench.measure(f"repeat_batch_{count}", ["install", *packages], validate)
            for label in bench.binaries:
                assert bench.query(label, ["list"]) == before[label]
                profile = (
                    pathlib.Path(bench.envs[label]["XDG_STATE_HOME"])
                    / "nix/profiles/pkg"
                )
                assert profile.readlink().as_posix() == generations[label]
            bench.report.setdefault("profile_checks", {})[str(count)] = {
                "same_rows": True,
                "same_generations": True,
            }
    finally:
        for label in bench.binaries:
            rows = bench.query(label, ["list"])
            if rows:
                bench.observe(
                    label,
                    "cleanup",
                    ["remove", *[row["package_id"] for row in rows]],
                )
                assert bench.query(label, ["list"]) == []
        shutil.rmtree(root)
    bench.report["complete"] = True
    bench.save()


if __name__ == "__main__":
    main()
