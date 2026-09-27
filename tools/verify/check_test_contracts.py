#!/usr/bin/env python3
"""Fail CI when a known product defect stops failing its behavior tests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib

from contract_cases import cases

ROOT = Path(__file__).resolve().parents[2]


def test_outcome(command, work, environment, log):
    """Run real tests. Build/import errors, skips and empty runs are not proof."""
    try:
        result = subprocess.run(command, cwd=work, env=environment,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, timeout=600)
        output, code = result.stdout, result.returncode
    except subprocess.TimeoutExpired as error:
        output = error.stdout or b""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        output += "\nTest command timed out.\n"
        code = 124
    log.write_text(output)
    if command[0] == "cargo":
        summaries = re.findall(
            r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", output)
        passed = failed = ignored = 0
        for counts in summaries:
            passed += int(counts[0])
            failed += int(counts[1])
            ignored += int(counts[2])
        healthy = code == 0 and passed > 0 and failed == ignored == 0
        caught = code == 101 and failed > 0 and ignored == 0 and "panicked at" in output
    else:
        count = re.search(r"Ran (\d+) tests? in", output)
        executed = count is not None and int(count[1]) > 0
        skipped = re.search(r"skipped[ =]", output) is not None
        errors = re.search(
            r"(?:^ERROR:|errors=|(?:SyntaxError|IndentationError|TabError|ImportError|ModuleNotFoundError):)",
            output, re.MULTILINE) is not None
        healthy = code == 0 and executed and not skipped and not errors
        caught = (code == 1 and executed and not skipped and not errors
                  and "AssertionError" in output and "FAIL:" in output)
    return {"exit_code": code, "healthy": healthy, "assertion_failed": caught,
            "log": log.name, "command": command}


def check_case(case, work, environment, output):
    name, filename, before, after, command = case
    path = work / filename
    original = path.read_bytes()
    source = original.decode()
    if source.count(before) != 1:
        raise ValueError(f"{name}: mutation target changed; update its behavior proof")
    baseline = test_outcome(command, work, environment, output / f"{name}-baseline.log")
    if not baseline["healthy"]:
        raise ValueError(f"{name}: baseline must run non-skipped tests and pass")
    try:
        mutated = source.replace(before, after, 1)
        if path.suffix == ".py":
            try:
                compile(mutated, str(path), "exec")
            except SyntaxError as error:
                raise ValueError(f"{name}: mutation produced invalid Python: {error}") from error
        path.write_text(mutated)
        mutant = test_outcome(command, work, environment, output / f"{name}-mutant.log")
    finally:
        path.write_bytes(original)
    restored = test_outcome(command, work, environment, output / f"{name}-restored.log")
    return {"id": name, "file": filename, "source_sha256": hashlib.sha256(original).hexdigest(),
            "baseline": baseline, "mutant": mutant, "restored": restored,
            "passed": mutant["assertion_failed"] and restored["healthy"]}


def snapshot(destination):
    files = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT)
    for name in set(os.fsdecode(files).split("\0")):
        source = ROOT / name
        if not name or not source.is_file():
            continue
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target, follow_symlinks=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True,
                        help="new directory for command logs and results")
    args = parser.parse_args()
    if not cases:
        parser.error("the behavior contract registry must not be empty")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    environment = os.environ | {"RUSTUP_TOOLCHAIN": channel,
        "CARGO_TARGET_DIR": str(ROOT / "target"), "CARGO_BUILD_JOBS": "1",
        "CARGO_PROFILE_TEST_DEBUG": "0", "CARGO_PROFILE_DEV_DEBUG": "0"}
    results = []
    with tempfile.TemporaryDirectory(prefix="pkg-test-contracts-") as temporary:
        work = Path(temporary)
        snapshot(work)
        for case in cases:
            try:
                result = check_case(case, work, environment, output)
            except (OSError, ValueError) as error:
                result = {"id": case[0], "passed": False, "error": str(error)}
            results.append(result)
            (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
            print(f"{case[0]}: {'PASS' if result['passed'] else 'FAIL'}", flush=True)
    return 0 if all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
