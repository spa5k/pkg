#!/usr/bin/env python3
"""Measure cold Formula metadata generation and verify the exact catalog bytes.

Use a new cache path for each run. A baseline generator can be read from a
separate checkout. Both generators use the pinned input from --repo.
"""

import argparse
import gzip
import hashlib
import importlib.util
import json
import pathlib
import shutil
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, required=True)
    parser.add_argument("--generator", type=pathlib.Path)
    parser.add_argument("--cache", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    assert not args.cache.exists(), "a new cache path is required"
    generator = args.generator or args.repo / "tools/formula-catalog/generate.py"
    spec = importlib.util.spec_from_file_location("formula", generator)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    raw = gzip.decompress(
        (args.repo / "tools/formula-catalog/inputs/formula.json.gz").read_bytes()
    )
    digest = hashlib.sha256(raw).hexdigest()
    expected = json.loads(
        (args.repo / "tools/formula-catalog/inputs/input.json").read_text()
    )["sha256"]
    assert digest == expected
    formulae = json.loads(raw)
    started = time.perf_counter()
    metadata = {
        system: module.native_metadata(
            formulae, system, shutil.which("nix"), args.cache
        )
        for system in module.SYSTEMS
    }
    catalog = module.generate(formulae, metadata, digest)
    output = (
        json.dumps(catalog, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()
    committed = (args.repo / "nix/formulae/catalog/catalog.json").read_bytes()
    assert output == committed, "generated catalog bytes changed"
    report = {
        "elapsed_ms": round(1000 * (time.perf_counter() - started), 3),
        "generator_sha256": hashlib.sha256(generator.read_bytes()).hexdigest(),
        "metadata_sha256": hashlib.sha256(
            json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "catalog_sha256": hashlib.sha256(output).hexdigest(),
        "metadata_rows": {system: len(rows) for system, rows in metadata.items()},
        "input_sha256": digest,
        "matches_committed_bytes": True,
        "cache_before": "absent",
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)


if __name__ == "__main__":
    main()
