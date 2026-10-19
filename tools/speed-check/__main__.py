"""Compare pkg speed between a baseline and a candidate release binary.

Run `python3 tools/speed-check <check> --help` for the options of one check.
Each check writes one JSON report with every sample, identity, and limit.
"""

import argparse
from pathlib import Path

import catalogs
import install
import matrix
import packages
import search


def main():
    report = argparse.ArgumentParser(add_help=False)
    report.add_argument("--output", type=Path, required=True, help="JSON report path")
    report.add_argument(
        "--runs", type=int, default=8, help="measured pairs; even counts balance order"
    )
    binaries = argparse.ArgumentParser(add_help=False, parents=[report])
    binaries.add_argument(
        "--pkg", type=Path, required=True, help="candidate pkg binary"
    )
    binaries.add_argument(
        "--baseline", type=Path, required=True, help="baseline pkg binary"
    )
    parser = argparse.ArgumentParser(description=__doc__)
    checks = parser.add_subparsers(dest="check", required=True, metavar="check")
    for module in (matrix, install, packages, search):
        module.register(checks, binaries)
    catalogs.register(checks, binaries, report)
    for check in checks.choices.values():
        check.set_defaults(error=check.error)
    args = parser.parse_args()
    if args.runs < 1:
        args.error("--runs must be positive")
    args.run(args)


if __name__ == "__main__":
    main()
