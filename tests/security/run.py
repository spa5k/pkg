#!/usr/bin/env python3
"""Run the closed AC-S1..S10 hermetic security-test manifest."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_name("cases.json")
EXPECTED_IDS = tuple(f"AC-S{number}" for number in range(1, 11))
CASE_KEYS = {"id", "description", "commands", "externalGates"}
ALLOWED_EXTERNAL_GATES = {"cargo-audit", "cargo-deny"}
SPIKE_MANIFEST = "spikes/s2-tough/Cargo.toml"


class ManifestError(ValueError):
    """Raised when the closed security manifest is malformed or widened."""


def load_manifest(path: Path = MANIFEST) -> list[dict[str, Any]]:
    """Load and validate the exact security case set."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ManifestError("security manifest is unreadable") from error
    return validate_manifest(document)


def validate_manifest(document: Any) -> list[dict[str, Any]]:
    """Validate a decoded manifest and return its ordered cases."""
    if not isinstance(document, dict) or set(document) != {"schemaVersion", "cases"}:
        raise ManifestError("security manifest shape is invalid")
    if document["schemaVersion"] != 1 or not isinstance(document["cases"], list):
        raise ManifestError("security manifest version is unsupported")

    cases = document["cases"]
    ids = tuple(case.get("id") for case in cases if isinstance(case, dict))
    if ids != EXPECTED_IDS:
        raise ManifestError("security manifest must contain AC-S1..AC-S10 in order")
    for case in cases:
        validate_case(case)
    return cases


def validate_case(case: Any) -> None:
    """Validate one case without executing it."""
    if not isinstance(case, dict) or set(case) != CASE_KEYS:
        raise ManifestError("security case shape is invalid")
    description = case["description"]
    commands = case["commands"]
    external_gates = case["externalGates"]
    if (
        not isinstance(description, str)
        or not description
        or len(description) > 160
        or not isinstance(commands, list)
        or len(commands) > 8
        or not isinstance(external_gates, list)
        or any(not isinstance(gate, str) for gate in external_gates)
        or any(gate not in ALLOWED_EXTERNAL_GATES for gate in external_gates)
        or len(set(external_gates)) != len(external_gates)
    ):
        raise ManifestError("security case values are invalid")
    if case["id"] == "AC-S8":
        if commands or set(external_gates) != ALLOWED_EXTERNAL_GATES:
            raise ManifestError("AC-S8 must be owned by deny and audit gates")
    elif not commands or external_gates:
        raise ManifestError("only AC-S8 may use external gates")
    for command in commands:
        validate_command(command)


def validate_command(command: Any) -> None:
    """Restrict cases to offline, locked Cargo tests with no shell surface."""
    if (
        not isinstance(command, list)
        or not command
        or len(command) > 32
        or any(not isinstance(value, str) or not value for value in command)
        or any(len(value) > 256 for value in command if isinstance(value, str))
        or command[0:2] != ["cargo", "test"]
        or "--offline" not in command
        or "--locked" not in command
        or any("\n" in value or "\r" in value or "\0" in value for value in command)
    ):
        raise ManifestError("security command is not an offline locked Cargo test")
    manifest_positions = [index for index, value in enumerate(command) if value == "--manifest-path"]
    if manifest_positions:
        if manifest_positions != [4] or len(command) <= 5 or command[5] != SPIKE_MANIFEST:
            raise ManifestError("security command names an unapproved manifest")
    if any(Path(value).is_absolute() or ".." in Path(value).parts for value in command[2:]):
        raise ManifestError("security command contains an unsafe path")


def run_cases(cases: list[dict[str, Any]], selected: set[str]) -> None:
    """Run selected cases in manifest order with Cargo forced offline."""
    environment = os.environ.copy()
    environment["CARGO_NET_OFFLINE"] = "true"
    environment["RUSTUP_TOOLCHAIN"] = "1.96.1"
    for case in cases:
        if selected and case["id"] not in selected:
            continue
        print(f"[{case['id']}] {case['description']}", flush=True)
        if case["externalGates"]:
            if environment.get("PKG_SECURITY_EXTERNAL_GATES") != "passed":
                raise ManifestError("dependency security gates were not proven")
            print("  external gates: cargo-deny, cargo-audit", flush=True)
            continue
        for command in case["commands"]:
            print("  " + " ".join(command), flush=True)
            subprocess.run(command, cwd=ROOT, env=environment, check=True)


def parse_args() -> argparse.Namespace:
    """Parse the intentionally small runner CLI."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--validate-only", action="store_true")
    return parser.parse_args()


def main() -> int:
    """Validate the manifest, optionally list it, and execute selected cases."""
    try:
        args = parse_args()
        cases = load_manifest()
        selected = set(args.case)
        unknown = selected.difference(EXPECTED_IDS)
        if unknown:
            raise ManifestError("unknown security case requested")
        if args.list:
            for case in cases:
                print(f"{case['id']}\t{case['description']}")
            return 0
        if args.validate_only:
            print("security manifest valid: AC-S1..AC-S10")
            return 0
        run_cases(cases, selected)
        print("security lane passed")
        return 0
    except (ManifestError, subprocess.CalledProcessError) as error:
        print(f"security lane failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
