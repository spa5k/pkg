"""Measure the real downloader with local release transport and real Nix.

The HTTP fixture serves archives with the measured client binaries. A normal
release template can supply the complete license and notice payload. A curl
adapter sends only the release URLs to the local server, and drops the
HTTPS-only flags that plain local HTTP cannot meet. Checksums, archive
validation, file installation, and shell setup run unchanged.

The completions directory and the client path are fixed. The baseline can
be an older installer, so the checks accept old and new completion names.
PKG_INSTALL_MODE=test keeps the new installer from enabling the daily user
job. PKG_INSTALL_PREPARE=0 and PKG_INSTALL_BACKGROUND=0 do the same for an
older baseline installer. The scheduled_update case runs the installed
`pkg update --scheduled` after the installer, inside the measured time, and
then checks search. All home and XDG paths are temporary.
"""

import functools
import http.server
import io
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
from pathlib import Path

from common import (
    REPO,
    Bench,
    balanced,
    digest,
    elapsed_ms,
    native_rows,
    pinned_catalogs,
)

SHELLS = ("bash", "zsh", "fish")
# Older installers keep <shell>.txt. Newer installers use the names that
# each shell loads.
COMPLETION_NAMES = {"bash": "pkg.bash", "zsh": "_pkg", "fish": "pkg.fish"}
STARTUP_FILES = (".bashrc", ".profile", ".bash_profile", ".bash_login", ".zshrc")
# Options of older installers that must not leak in from the caller.
LEGACY_OPTIONS = ("PKG_INSTALL_BIN", "PKG_INSTALL_COMPLETIONS", "PKG_INSTALL_SHELL_SETUP")
PRIVATE_PATHS = {
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "PKG_APP_CACHE_DIR",
}
# Name, login shell, run the scheduled update after the installer.
MODES = [
    ("client_only", None, False),
    ("bash_paths", "/bin/bash", False),
    ("zsh_paths", "/bin/zsh", False),
    ("repeat_bash_paths", "/bin/bash", False),
    ("scheduled_update", "/bin/bash", True),
]


def register(checks, binaries):
    check = checks.add_parser(
        "installer", parents=[binaries], help="install.sh modes", description=__doc__
    )
    check.add_argument("--installer", type=Path, default=REPO / "install.sh")
    check.add_argument("--baseline-installer", required=True, type=Path)
    check.add_argument("--release-template", type=Path)
    check.add_argument(
        "--full-setup",
        action="store_true",
        help="update the real pinned catalogs in fresh paths per sample",
    )
    check.add_argument(
        "--update-runs", type=int, help="measured pairs for the scheduled_update mode"
    )
    check.set_defaults(run=run)


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass


def release(directory, binary, version, system, template=None):
    directory.mkdir(parents=True)
    root = f"pkg-{version}-{system}"
    archive = directory / f"{root}.tar.gz"
    generated = {
        f"{root}/bin/pkg",
        *(f"{root}/completions/{shell}.txt" for shell in SHELLS),
    }
    with tarfile.open(archive, "w:gz") as output:
        output.add(binary, arcname=f"{root}/bin/pkg")
        for shell in SHELLS:
            data = subprocess.check_output([str(binary), "completion", shell])
            member = tarfile.TarInfo(f"{root}/completions/{shell}.txt")
            member.size = len(data)
            member.mode = 0o644
            output.addfile(member, io.BytesIO(data))
        if template:
            with tarfile.open(template) as source:
                for member in source.getmembers():
                    if member.name in generated:
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
            for name in ("LICENSE", "NOTICES.md", "licenses/THIRDPARTY.md"):
                assert source.extractfile(f"{root}/{name}").read()
            result["notice_file_count"] = sum(
                member.isfile()
                and "/licenses/" in member.name
                and not Path(member.name).name.startswith("._")
                for member in source.getmembers()
            )
            assert result["notice_file_count"] > 10
    return result


def curl_adapter(directory, port, version, system):
    """Redirect only the release downloads to the local server.

    The installer reads the pinned release. The scheduled update first
    reads the checksum list of the latest release, which is the same
    local release here. Its HTTPS-only flags (--proto, --tlsv1.2) are
    dropped, because the local server speaks plain HTTP.
    """
    directory.mkdir()
    curl = directory / "curl"
    curl.write_text(
        "#!/usr/bin/env python3\n"
        "import os, sys, urllib.parse\n"
        "args = sys.argv[1:]\n"
        "url = urllib.parse.urlsplit(args[-1])\n"
        f"allowed = {{'pkg-{version}-{system}.tar.gz', 'SHA256SUMS'}}\n"
        "name = url.path.rsplit('/', 1)[-1]\n"
        "pinned = url.path.startswith('/spa5k/pkg/releases/download/v"
        f"{version}/') and name in allowed\n"
        "latest = url.path == '/spa5k/pkg/releases/latest/download/SHA256SUMS'\n"
        "assert url.scheme == 'https' and url.netloc == 'github.com' and (pinned or latest), url\n"
        "kept = []\n"
        "options = iter(args[:-1])\n"
        "for arg in options:\n"
        "    if arg == '--proto':\n"
        "        next(options, None)\n"
        "    elif arg != '--tlsv1.2' and not arg.startswith('--proto='):\n"
        "        kept.append(arg)\n"
        f"args = [*kept, 'http://127.0.0.1:{port}/' + os.environ['PKG_BENCH_LABEL'] + '/' + name]\n"
        f"os.execv({shutil.which('curl')!r}, ['curl', *args])\n"
    )
    curl.chmod(0o755)
    return directory


def completions_installed(directory):
    """Every shell has a non-empty completion under an old or a new name."""
    return all(
        any(
            (directory / name).is_file() and (directory / name).stat().st_size > 0
            for name in (f"{shell}.txt", COMPLETION_NAMES[shell])
        )
        for shell in SHELLS
    )


def run_step(command, env):
    """Run one measured step in a new session, so nothing can prompt."""
    return subprocess.run(
        command,
        check=False,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=600,
        start_new_session=True,
    )


def run(args):
    if args.update_runs is not None and args.update_runs < 1:
        args.error("--update-runs must be positive")
    scripts = {
        "baseline": args.baseline_installer.resolve(strict=True),
        "candidate": args.installer.resolve(strict=True),
    }
    versions = {
        re.search(r'^version="([^"]+)"', path.read_text(), re.MULTILINE)[1]
        for path in scripts.values()
    }
    assert len(versions) == 1, "compare installers for the same release"
    version = versions.pop()
    with tempfile.TemporaryDirectory(prefix="pkg-installer-speed-") as temporary:
        root = Path(temporary)
        (root / "bench").mkdir()
        bench = Bench(args, root / "bench")
        report = bench.report
        report.update(
            transport="real curl over local HTTP; release archives; no added latency",
            scripts={
                label: {"path": str(path), "sha256": digest(path)}
                for label, path in scripts.items()
            },
            attempts=[],
            update_runs=args.update_runs or args.runs,
        )
        report["limits"] = [
            "Public network latency is excluded.",
            "Archives contain real clients and completions. Notice payloads are included when a release template is supplied.",
            "Nix is preinstalled, so the downloader never offers to install Nix. That path is not measured.",
            "The daily user job stays off: PKG_INSTALL_MODE=test for the new installer, PKG_INSTALL_PREPARE=0 and PKG_INSTALL_BACKGROUND=0 for an older baseline installer.",
            "The scheduled_update case includes `pkg update --scheduled` after the installer. Its client check reads the local release.",
        ]
        references = None
        expected_native = None
        if args.full_setup:
            references = pinned_catalogs(root / "real-sources")
            report["setup_references"] = references
            report["limits"] += [
                "With --full-setup, scheduled_update uses fresh HOME, XDG paths, profiles, and pkg caches per sample. Source metadata is not prewarmed.",
                "The shared Nix store and filesystem cache are warm after the excluded first pair.",
                "Cask and Formula data use a committed local source. Native Nixpkgs uses the exact pinned public source.",
            ]
        web = root / "web"
        report["archives"] = {
            label: release(
                web / label, binary, version, bench.system, args.release_template
            )
            for label, binary in bench.binaries.items()
        }
        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), functools.partial(Quiet, directory=str(web))
        )
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            adapters = curl_adapter(
                root / "adapters", server.server_port, version, bench.system
            )
            for name, shell, update in MODES:
                environments = {}
                for label in bench.binaries:
                    env = bench.environment(root / f"{name}-{label}")
                    for key in ("ZDOTDIR", *LEGACY_OPTIONS):
                        env.pop(key, None)
                    env.update(
                        PATH=str(adapters) + os.pathsep + os.environ["PATH"],
                        PKG_BENCH_LABEL=label,
                        PKG_INSTALL_MODE="test",
                        PKG_INSTALL_PREPARE="0",
                        PKG_INSTALL_BACKGROUND="0",
                        # An empty SHELL means no login shell. An unset one
                        # is filled in by Bash, the macOS /bin/sh.
                        SHELL=shell or "",
                    )
                    environments[label] = env
                samples = {label: [] for label in bench.binaries}
                runs = args.update_runs if update and args.update_runs else args.runs
                for iteration, label in balanced(bench.binaries, runs):
                    env = environments[label]
                    if update and args.full_setup:
                        fresh = bench.environment(
                            root / f"{name}-{label}-{iteration}", references
                        )
                        fresh.update(
                            {k: v for k, v in env.items() if k not in PRIVATE_PATHS}
                        )
                        env = fresh
                    home = Path(env["HOME"])
                    bindir = home / ".local/bin"
                    completions = home / ".local/share/pkg/completions"
                    if name != "repeat_bash_paths":
                        shutil.rmtree(bindir, ignore_errors=True)
                        shutil.rmtree(completions, ignore_errors=True)
                        for startup in STARTUP_FILES:
                            (home / startup).unlink(missing_ok=True)
                    started = time.perf_counter()
                    result = run_step(["sh", str(scripts[label])], env)
                    scheduled = None
                    if update and result.returncode == 0:
                        scheduled = run_step(
                            [str(bindir / "pkg"), "update", "--scheduled"], env
                        )
                    elapsed = elapsed_ms(started)
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
                    if scheduled is not None:
                        attempt["update"] = {
                            "command": ["pkg", "update", "--scheduled"],
                            "code": scheduled.returncode,
                            "stdout": scheduled.stdout,
                            "stderr": scheduled.stderr,
                        }
                    report["attempts"].append(attempt)
                    bench.save()
                    assert result.returncode == 0, result.stdout + result.stderr
                    if update:
                        assert scheduled.returncode == 0, (
                            scheduled.stdout + scheduled.stderr
                        )
                    assert digest(bindir / "pkg") == digest(bench.binaries[label])
                    assert completions_installed(completions), (
                        sorted(path.name for path in completions.iterdir())
                        if completions.is_dir()
                        else "no completions directory"
                    )
                    if shell:
                        startup = home / (
                            ".zshrc" if shell.endswith("zsh") else ".bashrc"
                        )
                        assert startup.read_text().count("# >>> pkg paths >>>") == 1
                    else:
                        for startup in STARTUP_FILES:
                            assert not (home / startup).exists(), startup
                    if update:
                        query = "hello" if args.full_setup else "probe-000"
                        found = bench.query(
                            label, ["search", query, "--source", "nixpkgs"], env
                        )
                        assert found["total"] > 0
                    if update and args.full_setup:
                        attempt["native"] = native_rows(env["XDG_CACHE_HOME"])
                        expected_native = expected_native or attempt["native"]
                        assert attempt["native"] == expected_native
                        assert bench.query(label, ["list"], env) == []
                        attempt["empty_profile"] = True
                        bench.save()
                    if iteration:
                        samples[label].append(elapsed)
                bench.record(name, samples)
        finally:
            server.shutdown()
            server.server_close()
            worker.join()
        report["complete"] = True
        bench.save()
