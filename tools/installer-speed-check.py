#!/usr/bin/env python3
"""Measure the real downloader with local release transport and real Nix setup.

The HTTP fixture serves archives with the measured client binaries.
A normal release template can supply the complete license and notice payload.
A curl adapter changes only the release URL to the local server. Checksums,
archive validation, file installation, shell setup, and pkg setup run unchanged.
No scheduled job is enabled. All home and XDG paths are temporary.
"""

import argparse
import functools
import hashlib
import http.server
import importlib.util
import io
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import tarfile
import tempfile
import threading
import time
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass


def release(directory, binary, version, system, template=None):
    directory.mkdir(parents=True)
    root = f"pkg-{version}-{system}"
    archive = directory / f"{root}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(binary, arcname=f"{root}/bin/pkg")
        for shell in ("bash", "zsh", "fish"):
            data = subprocess.check_output([str(binary), "completion", shell])
            member = tarfile.TarInfo(f"{root}/completions/{shell}.txt")
            member.size = len(data)
            member.mode = 0o644
            output.addfile(member, io.BytesIO(data))
        if template:
            with tarfile.open(template) as source:
                for member in source.getmembers():
                    if member.name in {
                        f"{root}/bin/pkg",
                        *(
                            f"{root}/completions/{shell}.txt"
                            for shell in ("bash", "zsh", "fish")
                        ),
                    }:
                        continue
                    assert member.isfile() or member.isdir(), member.name
                    if member.isfile():
                        with source.extractfile(member) as payload:
                            output.addfile(member, payload)
                    else:
                        output.addfile(member)
    (directory / "SHA256SUMS").write_text(f"{digest(archive)}  {archive.name}\n")
    result = {"sha256": digest(archive), "size_bytes": archive.stat().st_size}
    if template:
        result["notice_template"] = {"path": str(template), "sha256": digest(template)}
        with tarfile.open(archive) as source:
            assert source.extractfile(f"{root}/LICENSE").read()
            assert source.extractfile(f"{root}/NOTICES.md").read()
            assert source.extractfile(f"{root}/licenses/THIRDPARTY.md").read()
            result["notice_file_count"] = sum(
                member.isfile()
                and "/licenses/" in member.name
                and not Path(member.name).name.startswith("._")
                for member in source.getmembers()
            )
            assert result["notice_file_count"] > 10
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pkg", required=True, type=Path)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--installer", type=Path, default=Path("install.sh"))
    parser.add_argument("--baseline-installer", required=True, type=Path)
    parser.add_argument(
        "--runs", type=int, default=8, help="measured pairs; even counts balance order"
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--release-template", type=Path)
    parser.add_argument("--full-setup", action="store_true")
    parser.add_argument("--prepare-runs", type=int)
    args = parser.parse_args()
    assert args.runs > 0
    assert args.prepare_runs is None or args.prepare_runs > 0
    scripts = {
        "baseline": args.baseline_installer.resolve(strict=True),
        "candidate": args.installer.resolve(strict=True),
    }
    versions = {
        re.search(r'^version="([^"]+)"', p.read_text(), re.MULTILINE)[1]
        for p in scripts.values()
    }
    assert len(versions) == 1, "compare installers for the same release"
    version = versions.pop()
    spec = importlib.util.spec_from_file_location(
        "pkg_speed", Path(__file__).with_name("pkg-speed-check.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    report = {
        "complete": False,
        "platform": platform.platform(),
        "runs": args.runs,
        "transport": "real curl over local HTTP; release archives; no added latency",
        "limits": [
            "Public network latency is excluded.",
            "Archives contain real clients and completions. Notice payloads are included when a release template is supplied.",
            "Nix is preinstalled. The downloader does not install Nix.",
            "Background scheduling is disabled in these cases.",
        ],
        "scripts": {
            label: {"path": str(path), "sha256": digest(path)}
            for label, path in scripts.items()
        },
        "cases": {},
        "observations": [],
        "attempts": [],
        "prepare_runs": args.prepare_runs or args.runs,
    }

    def save():
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")

    with tempfile.TemporaryDirectory(prefix="pkg-installer-speed-") as temporary:
        root = Path(temporary)
        fixture = root / "fixture"
        fixture.mkdir()
        bench = module.Bench(args, fixture)
        report["system"] = bench.system
        report["binaries"] = bench.report["binaries"]
        setup_module = None
        references = None
        expected_native = None
        if args.full_setup:
            setup_spec = importlib.util.spec_from_file_location(
                "real_setup", args.repo / "tools/real-setup-speed-check.py"
            )
            setup_module = importlib.util.module_from_spec(setup_spec)
            setup_spec.loader.exec_module(setup_module)
            references = setup_module.pinned_catalogs(args.repo, root / "real-sources")
            report["setup_references"] = references
            report["catalog_sha256"] = {
                part: digest(root / "real-sources" / part / "catalog/catalog.json")
                for part in ["casks", "formulae"]
            }
            report["limits"].extend(
                [
                    "Full setup uses fresh HOME, XDG paths, profiles, and pkg caches per sample. Source metadata is not prewarmed.",
                    "The shared Nix store and filesystem cache are warm after the excluded first pair.",
                    "Cask and Formula data use a committed local source. Native Nixpkgs uses the exact pinned public source.",
                ]
            )
        web = root / "web"
        report["archives"] = {
            label: release(
                web / label, binary, version, bench.system, args.release_template
            )
            for label, binary in bench.binaries.items()
        }
        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), functools.partial(Handler, directory=str(web))
        )
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            adapters = root / "adapters"
            adapters.mkdir()
            curl = adapters / "curl"
            curl.write_text(
                "#!/usr/bin/env python3\n"
                "import os, sys, urllib.parse\n"
                "args = sys.argv[1:]\n"
                "url = urllib.parse.urlsplit(args[-1])\n"
                f"allowed = {{'pkg-{version}-{bench.system}.tar.gz', 'SHA256SUMS'}}\n"
                "name = url.path.rsplit('/', 1)[-1]\n"
                f"assert url.scheme == 'https' and url.netloc == 'github.com' and url.path.startswith('/spa5k/pkg/releases/download/v{version}/') and name in allowed, url\n"
                f"args[-1] = 'http://127.0.0.1:{server.server_port}/' + os.environ['PKG_BENCH_LABEL'] + '/' + name\n"
                f"os.execv({shutil.which('curl')!r}, ['curl', *args])\n"
            )
            curl.chmod(0o755)
            modes = [
                ("client_only", None, False, False),
                ("bash_paths", "/bin/bash", False, False),
                ("zsh_paths", "/bin/zsh", False, False),
                ("custom_paths", "/bin/bash", False, True),
                ("repeat_bash_paths", "/bin/bash", False, False),
                ("prepare_no_background", "/bin/bash", True, False),
            ]
            for name, shell, prepare, custom in modes:
                environments = {}
                for label in bench.binaries:
                    env = bench.environment(root / f"{name}-{label}")
                    env.update(
                        PATH=str(adapters) + os.pathsep + os.environ["PATH"],
                        PKG_BENCH_LABEL=label,
                        PKG_INSTALL_SHELL_SETUP="1" if shell else "0",
                        PKG_INSTALL_PREPARE="1" if prepare else "0",
                        PKG_INSTALL_BACKGROUND="0",
                    )
                    env.pop("ZDOTDIR", None)
                    env.pop("PKG_INSTALL_BIN", None)
                    env.pop("PKG_INSTALL_COMPLETIONS", None)
                    if shell:
                        env["SHELL"] = shell
                    else:
                        env.pop("SHELL", None)
                    if custom:
                        env["PKG_INSTALL_BIN"] = str(root / f"custom-{label}/bin")
                        env["PKG_INSTALL_COMPLETIONS"] = str(
                            root / f"custom-{label}/completions"
                        )
                    environments[label] = env
                samples = {label: [] for label in bench.binaries}
                case_runs = (
                    args.prepare_runs
                    if prepare and args.prepare_runs is not None
                    else args.runs
                )
                for iteration in range(case_runs + 1):
                    order = list(bench.binaries)
                    if iteration % 2:
                        order.reverse()
                    for label in order:
                        env = environments[label]
                        if prepare and args.full_setup:
                            fresh = bench.environment(
                                root / f"{name}-{label}-{iteration}"
                            )
                            fresh.update(
                                {
                                    key: value
                                    for key, value in env.items()
                                    if key
                                    not in {
                                        "HOME",
                                        "XDG_CONFIG_HOME",
                                        "XDG_STATE_HOME",
                                        "XDG_CACHE_HOME",
                                        "PKG_APP_CACHE_DIR",
                                    }
                                }
                            )
                            config = Path(fresh["XDG_CONFIG_HOME"]) / "pkg/config.toml"
                            config.write_text(
                                "[sources]\n"
                                + "".join(
                                    name + " = " + json.dumps(reference) + "\n"
                                    for name, reference in references.items()
                                )
                            )
                            env = fresh
                        home = Path(env["HOME"])
                        bindir = Path(env.get("PKG_INSTALL_BIN", home / ".local/bin"))
                        completions = Path(
                            env.get(
                                "PKG_INSTALL_COMPLETIONS",
                                home / ".local/share/pkg/completions",
                            )
                        )
                        if name != "repeat_bash_paths":
                            shutil.rmtree(bindir, ignore_errors=True)
                            shutil.rmtree(completions, ignore_errors=True)
                            for startup in (".bashrc", ".profile", ".zshrc"):
                                (home / startup).unlink(missing_ok=True)
                        started = time.perf_counter()
                        result = subprocess.run(
                            ["sh", str(scripts[label])],
                            check=False,
                            env=env,
                            capture_output=True,
                            text=True,
                            timeout=600,
                        )
                        elapsed = round(1000 * (time.perf_counter() - started), 3)
                        attempt = {
                            "case": name,
                            "binary": label,
                            "iteration": iteration,
                            "warmup": iteration == 0,
                            "elapsed_ms": elapsed,
                            "code": result.returncode,
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                        }
                        report["attempts"].append(attempt)
                        save()
                        assert result.returncode == 0, result.stdout + result.stderr
                        assert digest(bindir / "pkg") == digest(bench.binaries[label])
                        assert all(
                            (completions / f"{s}.txt").stat().st_size > 0
                            for s in ("bash", "zsh", "fish")
                        )
                        if shell:
                            startup = home / (
                                ".zshrc" if shell.endswith("zsh") else ".bashrc"
                            )
                            assert startup.read_text().count("# >>> pkg paths >>>") == 1
                        else:
                            assert not (home / ".bashrc").exists()
                        if prepare:
                            assert (
                                bench.query(
                                    label,
                                    [
                                        "search",
                                        "hello" if args.full_setup else "probe-000",
                                        "--source",
                                        "nixpkgs",
                                    ],
                                    env,
                                )["total"]
                                > 0
                            )
                        if prepare and args.full_setup:
                            attempt["native"] = setup_module.native_rows(
                                Path(env["XDG_CACHE_HOME"])
                            )
                            save()
                            if expected_native is None:
                                expected_native = attempt["native"]
                            assert attempt["native"] == expected_native
                            assert bench.query(label, ["list"], env) == []
                            attempt["empty_profile"] = True
                        if iteration:
                            samples[label].append(elapsed)
                        else:
                            report["observations"].append(
                                {
                                    "name": name,
                                    "binary": label,
                                    "elapsed_ms": elapsed,
                                    "stdout": result.stdout,
                                    "stderr": result.stderr,
                                }
                            )
                report["cases"][name] = {
                    label: module.summary(values) for label, values in samples.items()
                }
                case = report["cases"][name]
                case["latency_reduction_percent"] = round(
                    100
                    * (
                        1
                        - statistics.median(samples["candidate"])
                        / statistics.median(samples["baseline"])
                    ),
                    1,
                )
                save()
                print(
                    name,
                    {label: case[label]["median_ms"] for label in samples},
                    flush=True,
                )
        finally:
            server.shutdown()
            server.server_close()
            worker.join()
    report["complete"] = True
    save()


if __name__ == "__main__":
    main()
