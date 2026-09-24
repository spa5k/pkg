#!/usr/bin/env python3
"""Sign and notarize a staged macOS release. Never modifies the input binaries."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

BINARIES = ("pkg", "pkg-install", "pkg-nix-broker", "pkg-root-helper")
ROOT = Path(__file__).resolve().parent


def validate(args):
    if re.fullmatch(r"[A-Z0-9]{10}", args.team) is None:
        raise ValueError("an Apple Team ID is required")
    for value, kind in [(args.application_identity, "Application"), (args.installer_identity, "Installer")]:
        if not value.startswith(f"Developer ID {kind}: ") or not value.endswith(f"({args.team})"):
            raise ValueError(f"a matching Developer ID {kind} identity is required")
    if not args.profile or not args.keychain.is_file():
        raise ValueError("a keychain and stored notarytool profile are required")
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[1-9][0-9]*)?", args.version) is None:
        raise ValueError("invalid release version")
    if args.output.exists() or args.output.is_symlink() or not args.output.parent.is_dir():
        raise ValueError("output must be a new directory under an existing parent")
    for name in BINARIES:
        path = args.assets / name
        if path.is_symlink() or not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError(f"missing executable release input: {name}")


def run(*command):
    return subprocess.run(command, check=True, text=True, capture_output=True,
                          env=os.environ | {"LANG": "C", "LC_ALL": "C"}).stdout


def publish(args, command=run):
    validate(args)
    with tempfile.TemporaryDirectory(prefix="pkg-sign-", dir=args.output.parent) as directory:
        work = Path(directory)
        staged = work / "release"
        staged.mkdir(mode=0o700)
        for name in BINARIES:
            target = staged / name
            shutil.copy2(args.assets / name, target)
            command("/usr/bin/codesign", "--force", "--sign", args.application_identity,
                    "--keychain", str(args.keychain), "--options", "runtime", "--timestamp", str(target))
            command("/usr/bin/codesign", "--verify", "--strict", str(target))
        scripts = work / "scripts"
        scripts.mkdir(mode=0o700)
        shutil.copy2(staged / "pkg-install", scripts / "pkg-install")
        shutil.copy2(ROOT / "postinstall", scripts / "postinstall")
        unsigned = work / "unsigned.pkg"
        package = staged / f"pkg-{args.version}.pkg"
        command("/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts),
                "--identifier", "org.pkg.installer", "--version", args.version, str(unsigned))
        command("/usr/bin/productsign", "--sign", args.installer_identity,
                "--keychain", str(args.keychain), "--timestamp", str(unsigned), str(package))
        command("/usr/sbin/pkgutil", "--check-signature", str(package))
        submission = json.loads(command("/usr/bin/xcrun", "notarytool", "submit", str(package),
                                        "--keychain-profile", args.profile, "--keychain", str(args.keychain),
                                        "--wait", "--output-format", "json"))
        if submission.get("status") != "Accepted":
            raise ValueError("Apple did not accept the package; no release output was created")
        command("/usr/bin/xcrun", "stapler", "staple", str(package))
        command("/usr/bin/xcrun", "stapler", "validate", str(package))
        command("/usr/sbin/spctl", "--assess", "--type", "install", "--verbose=2", str(package))
        # The notarized container includes the signed setup program. Other
        # binaries need their own notarized archive before standalone distribution.
        archive = work / "pkg-binaries.zip"
        command("/usr/bin/ditto", "-c", "-k", "--keepParent", str(staged), str(archive))
        binary_submission = json.loads(command("/usr/bin/xcrun", "notarytool", "submit", str(archive),
                                               "--keychain-profile", args.profile, "--keychain", str(args.keychain),
                                               "--wait", "--output-format", "json"))
        if binary_submission.get("status") != "Accepted":
            raise ValueError("Apple did not accept the command binaries; no release output was created")
        evidence = {"schemaVersion": 1, "version": args.version, "team": args.team,
                    "packageSubmission": submission.get("id"), "binarySubmission": binary_submission.get("id"),
                    "status": "accepted", "cleanMacProof": "required", "sha256": {}}
        for path in staged.iterdir():
            with path.open("rb") as stream:
                evidence["sha256"][path.name] = hashlib.file_digest(stream, "sha256").hexdigest()
        (staged / "notarization.json").write_text(json.dumps(evidence, indent=2) + "\n")
        staged.rename(args.output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--team", required=True)
    parser.add_argument("--application-identity", required=True)
    parser.add_argument("--installer-identity", required=True)
    parser.add_argument("--keychain", required=True, type=Path)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    validate(args)
    if args.check_only:
        print("Signing inputs are present. Key access, notarization, and clean-Mac proof remain unchecked.")
    else:
        publish(args)
        print(f"Notarized output: {args.output}. Clean-Mac testing is still required.")


if __name__ == "__main__":
    main()
