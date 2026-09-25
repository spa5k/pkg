#!/usr/bin/env python3
"""Render GitHub results without changing or hiding failed/skipped checks."""
import argparse
import html
import json
import os
from pathlib import Path
import sys

LABELS = {
    "g-ambient-time-audit": "Static audits",
    "g-hermetic": "Isolated tests · Linux",
    "g-hermetic-macos": "Isolated tests · macOS (network denial is a probe)",
    "g-lint": "Format, lint, build, and dependencies · Linux",
    "g-lint-macos": "Format, lint, and tests · macOS",
    "g-test": "Workspace tests and documentation examples",
    "g-quality": "Code quality · Linux and macOS",
    "bootstrap": "Installer and release tool checks · Linux and macOS",
    "dry-run-sign": "Test-key signing and publication checks",
    "linux-alpha-proof": "Linux installation and package lifecycle",
    "production-linux": "Manual production Linux build",
}
MARKS = {"success": "✅ Passed", "failure": "❌ Failed", "cancelled": "⏹ Cancelled", "skipped": "➖ Skipped"}


def cell(value):
    return html.escape(str(value)).replace("|", "&#124;").replace("\n", " ").replace("\r", " ")


def summary(title, results):
    if not isinstance(results, dict) or not results:
        raise ValueError("check results are missing")
    lines = [f"## {cell(title)}", "", "| Check | Result |", "| :--- | :--- |"]
    passed = True
    for key, value in results.items():
        status = value.get("result", value.get("outcome", "unknown"))
        passed &= status == "success"
        label = LABELS.get(key, key.replace("_", " ").replace("-", " "))
        lines.append(f"| {cell(label)} | {MARKS.get(status, '❔ Unknown')} |")
    lines += ["", "Open a failed check for its log. Download retained proof artifacts from this run.", ""]
    return "\n".join(lines), passed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", default="pkg · Check results")
    parser.add_argument("--require-success", action="store_true")
    args = parser.parse_args()
    text, passed = summary(args.title, json.loads(os.environ["RESULTS_JSON"]))
    print(text)
    if destination := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(destination).open("a") as stream:
            stream.write(text)
    return 1 if args.require_success and not passed else 0


if __name__ == "__main__":
    sys.exit(main())
