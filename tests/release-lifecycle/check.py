#!/usr/bin/env python3
"""Check exact signed alpha releases only on explicitly selected disposable hosts."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = "spa5k/pkg"
CLI = "/usr/local/bin/pkg"
SPEC = importlib.util.spec_from_file_location("installer", ROOT / "tools/install/render.py")
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)


def require_disposable(confirmation):
    if confirmation != "TEST-DISPOSABLE-HOST" or os.geteuid() == 0:
        raise ValueError("run as the test user with explicit disposable-host confirmation")
    hosted = os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
    virtual_mac = platform.system() == "Darwin" and subprocess.check_output(
        ["/usr/sbin/sysctl", "-n", "hw.model"], text=True).startswith("VirtualMac")
    if not hosted and not virtual_mac:
        raise ValueError("requires a GitHub-hosted disposable runner or a VirtualMac guest")


def valid_tag(tag):
    if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+-alpha\.[1-9][0-9]*", tag) is None:
        raise ValueError("invalid alpha release tag")
    return tag


def boot_id():
    if platform.system() == "Darwin":
        return subprocess.check_output(["/usr/sbin/sysctl", "-n", "kern.bootsessionuuid"], text=True).strip()
    return Path("/proc/sys/kernel/random/boot_id").read_text().strip()


class Checks:
    def __init__(self, work):
        self.work = work
        self.rows = []
        state = "Library/Application Support/pkg" if platform.system() == "Darwin" else ".local/share/pkg"
        self.bin = Path.home() / state / "current/bin"
        self.env = os.environ | {"CI": "true", "NO_COLOR": "1",
            "PATH": f"{Path.home() / state / 'current/bin'}:/usr/local/bin:{os.environ['PATH']}"}

    def run(self, label, command, timeout=1200):
        print(f"Running: {label}", flush=True)
        started = time.monotonic()
        log = self.work / f"{len(self.rows) + 1:02d}-{re.sub(r'[^a-z0-9]+', '-', label.lower())}.log"
        status = "failed"
        try:
            with log.open("wb") as output:
                subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, env=self.env,
                               check=True, timeout=timeout)
            status = "passed"
            return log.read_text(errors="replace")
        finally:
            self.rows.append({"check": label, "status": status, "seconds": round(time.monotonic() - started, 2), "log": log.name})
            self.save()
            print(f"{'PASS' if status == 'passed' else 'FAIL'}: {label} ({self.rows[-1]['seconds']}s)", flush=True)

    def package(self, name, label):
        return self.run(label, [str(self.bin / name), "--version"])

    def removed(self, name, names):
        if name in names.splitlines() or os.path.lexists(self.bin / name):
            raise ValueError("removed package remains active")

    def save(self):
        (self.work / "checks.json").write_text(json.dumps(self.rows, indent=2) + "\n")
        lines = ["## pkg · Exact release checks", "", "| Check | Result | Time |", "| :--- | :--- | ---: |"]
        for row in self.rows:
            mark = "✅ Passed" if row['status'] == 'passed' else "❌ Failed"
            lines.append(f"| {row['check']} | {mark} | {row['seconds']}s |")
        lines += ["", "Reboot is proved only by a successful resume report. Linux public flakes are not supported.", ""]
        (self.work / "summary.md").write_text("\n".join(lines))

    def acquire(self, tag):
        assets = self.work / tag
        assets.mkdir(exist_ok=True)
        system = "aarch64-darwin" if platform.system() == "Darwin" else "x86_64-linux"
        names = ["pkg-install-x86_64-linux", f"pkg-{tag[1:]}-preview.pkg", "install-preview.sh"]
        names += [f"{binary}-{system}" for binary in ("pkg", "pkg-nix-broker", "pkg-root-helper")]
        for name in names:
            if not (assets / name).exists():
                self.run(f"Download {tag} {name}", ["gh", "release", "download", tag, "--repo", REPOSITORY,
                    "--dir", str(assets), "--pattern", name, "--pattern", name + ".sigstore.json"])
            self.run(f"Verify {tag} {name}", ["cosign", "verify-blob", "--bundle", str(assets / (name + ".sigstore.json")),
                "--certificate-identity", f"https://github.com/{REPOSITORY}/.github/workflows/alpha-release.yml@refs/tags/{tag}",
                "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com", str(assets / name)])
        script = assets / "install.sh"
        script.write_text(INSTALLER.render(tag, assets))
        return assets

    def installed_bytes(self, assets):
        system = "aarch64-darwin" if platform.system() == "Darwin" else "x86_64-linux"
        lines = []
        for binary, path in [("pkg", CLI), ("pkg-nix-broker", "/opt/pkg/bin/pkg-nix-broker"), ("pkg-root-helper", "/opt/pkg/bin/pkg-root-helper")]:
            with (assets / f"{binary}-{system}").open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            lines.append(f"{digest}  {path}\n")
        inventory = assets / "installed.sha256"
        inventory.write_text("".join(lines))
        self.run("Installed release bytes", ["sudo", "shasum", "-a", "256", "--check", str(inventory)])


def prepare(checks, args):
    if Path(CLI).exists() or Path("/nix/receipt.json").exists():
        raise ValueError("fresh proof requires an empty disposable host; existing installations are retained")
    before = checks.acquire(args.from_release)
    checks.run("Fresh installation", ["/bin/sh", str(before / "install.sh")])
    checks.installed_bytes(before)
    checks.run("Install cached package", [CLI, "install", "fzf", "-y"])
    checks.package("fzf", "Run installed package")
    nix_before = checks.run("Record Nix identity", ["sudo", "shasum", "-a", "256", "/nix/receipt.json", "/nix/nix-installer"])
    checkpoint = {"host": platform.node(), "release": args.from_release,
                  "status": "awaiting-upgrade", "nix": nix_before, "checks": checks.rows}
    (checks.work / "checkpoint.json").write_text(json.dumps(checkpoint, indent=2) + "\n")


def upgrade(checks, args):
    checkpoint = json.loads((checks.work / "checkpoint.json").read_text())
    if checkpoint["status"] != "awaiting-upgrade" or checkpoint["release"] != args.from_release or checkpoint["host"] != platform.node():
        raise ValueError("prepare the previous release on this VM before upgrading")
    checks.rows = checkpoint["checks"]
    checks.installed_bytes(checks.work / args.from_release)
    nix_before = checkpoint["nix"]
    after = checks.acquire(args.to_release)
    checks.run("Product upgrade", ["/bin/sh", str(after / "install.sh")])
    checks.installed_bytes(after)
    nix_after = checks.run("Check Nix identity", ["sudo", "shasum", "-a", "256", "/nix/receipt.json", "/nix/nix-installer"])
    if nix_before != nix_after:
        raise ValueError("product upgrade changed the Base Nix identity")
    checks.package("fzf", "Retained package")
    checks.run("Repeat installer", ["/bin/sh", str(after / "install.sh")])
    if platform.system() == "Darwin":
        checks.run("Record build disk space", ["/bin/df", "-k", "/nix"])
        checks.run("Record build system load", ["/usr/sbin/sysctl", "-n", "vm.loadavg", "hw.logicalcpu"])
        if os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted":
            checks.run("Wait for hosted build capacity", [sys.executable,
                str(ROOT / "tests/release-lifecycle/wait_for_build_capacity.py")], timeout=660)
        checks.run("Build public flake", [CLI, "install", "github:casey/just#default", "-y"], timeout=1800)
        checks.package("just", "Run built package")
    checks.run("Remove package", [CLI, "remove", "fzf", "-y"])
    names = checks.run("Check removal", [CLI, "list", "--name-only"])
    checks.removed("fzf", names)
    checks.run("Rollback", [CLI, "rollback", "-y"])
    checks.package("fzf", "Run restored package")
    checks.run("Verify package files", [CLI, "repair", "--verify-only"])
    checks.run("Final health", [CLI, "doctor"])
    checkpoint = {"boot": boot_id(), "release": args.to_release, "host": platform.node(),
                  "status": "awaiting-reboot", "checks": checks.rows}
    (checks.work / "checkpoint.json").write_text(json.dumps(checkpoint, indent=2) + "\n")


def resume(checks, args):
    checkpoint = json.loads((checks.work / "checkpoint.json").read_text())
    if checkpoint["status"] != "awaiting-reboot" or checkpoint["release"] != args.to_release:
        raise ValueError("no matching prepared release")
    if checkpoint["boot"] == boot_id() or checkpoint["host"] != platform.node():
        raise ValueError("reboot the same VM before resuming")
    checks.rows = checkpoint["checks"]
    checks.installed_bytes(checks.work / args.to_release)
    checks.run("Health after reboot", [CLI, "doctor"])
    checks.package("fzf", "Package after reboot")
    if platform.system() == "Darwin":
        checks.package("just", "Built package after reboot")
    checkpoint["status"] = "passed"
    checkpoint["resumedBoot"] = boot_id()
    (checks.work / "checkpoint.json").write_text(json.dumps(checkpoint, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=["prepare", "upgrade", "resume", "smoke"])
    parser.add_argument("--from-release", required=True, type=valid_tag)
    parser.add_argument("--to-release", required=True, type=valid_tag)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--confirm", required=True)
    args = parser.parse_args()
    require_disposable(args.confirm)
    if args.phase in ("prepare", "smoke"):
        args.work.mkdir(mode=0o700, parents=True)
    elif not args.work.is_dir():
        parser.error("prepared evidence directory is missing")
    checks = Checks(args.work.resolve())
    try:
        if args.phase == "smoke":
            if args.from_release != args.to_release:
                raise ValueError("smoke checks one current release; use prepare then upgrade across a channel publication")
            prepare(checks, args)
            upgrade(checks, args)
        else:
            {"prepare": prepare, "upgrade": upgrade, "resume": resume}[args.phase](checks, args)
    except Exception as error:
        checks.rows.append({"check": "Lifecycle validation", "status": "failed", "seconds": 0, "log": "See process error"})
        checks.save()
        raise SystemExit(f"FAIL: {error}") from error
    print("PASS: completed selected phase. See checkpoint.json for reboot status.")


if __name__ == "__main__":
    main()
