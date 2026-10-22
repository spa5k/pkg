#!/usr/bin/env python3
"""Generate pinned Formula support data without executing Homebrew recipes."""

import argparse
import gzip
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import urllib.parse
from collections import Counter

SYSTEMS = ("aarch64-darwin", "x86_64-linux")
TOKEN = re.compile(r"[a-z0-9][a-z0-9+._@-]*\Z")
REVISION = "8edc0c72e3a38faf5434e40d1f19431125fe4b30"
API = "https://formulae.brew.sh/api/formula.jws.json"
PIN = pathlib.Path(__file__).resolve().parent / "inputs/input.json"
LICENSE = "BSD-2-Clause (Homebrew Core data)"
MAX_INPUT = 128 * 1024 * 1024


def project(url):
    if not isinstance(url, str):
        return None
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        return None
    host = parsed.hostname.lower().removeprefix("www.")
    path = parsed.path.rstrip("/").removesuffix(".git")
    if host in ("github.com", "gitlab.com"):
        path = path.lower()
    return host + path


def candidates(formula):
    names = [formula["name"], *formula.get("aliases", [])]
    values = []
    for name in names:
        for candidate in (name, name.replace("-", "_"), name.replace("@", "").replace(".", "")):
            if re.fullmatch(r"[a-zA-Z0-9_+-]+", candidate) and candidate not in values:
                values.append(candidate)
    return values


def metadata_batches(names):
    offset = 0
    while offset < len(names):
        end = min(offset + 4096, len(names))
        while True:
            batch = json.dumps(json.dumps(names[offset:end])).replace("${", "\\${")
            if len(batch.encode()) <= 64 * 1024:
                break
            if end == offset + 1:
                raise ValueError("Formula candidate name exceeds the evaluation argument limit")
            end = offset + (end - offset) // 2
        yield end, batch
        offset = end


def native_metadata(formulae, system, nix, cache):
    destination = cache / f"nixpkgs-{REVISION}-{system}.json"
    if destination.exists():
        return json.loads(destination.read_text())
    names = sorted({name for formula in formulae for name in candidates(formula)})
    metadata = {}
    for checked, batch in metadata_batches(names):
        apply = f'''pkgs: let
          inspect = name: let p = pkgs.${{name}} or null;
            row = if p != null && (p.type or null) == "derivation"
              && !(p.meta.broken or false)
              && pkgs.lib.meta.availableOn pkgs.stdenv.hostPlatform p
              then {{ inherit name; version = p.version or ""; homepage = p.meta.homepage or null; }}
              else null;
            attempt = builtins.tryEval (builtins.deepSeq row row);
            in if attempt.success then attempt.value else null;
          in map inspect (builtins.fromJSON {batch})'''
        command = [nix, "--extra-experimental-features", "nix-command flakes", "eval", "--json",
                   f"github:NixOS/nixpkgs/{REVISION}#legacyPackages.{system}", "--apply", apply]
        result = subprocess.run(command, text=True, capture_output=True, check=True, timeout=120,
                                env={**os.environ, "GC_INITIAL_HEAP_SIZE": "8388608", "GC_FREE_SPACE_DIVISOR": "20"})
        metadata.update({row["name"]: row for row in json.loads(result.stdout) if row is not None})
        print(f"{system}: checked {checked} of {len(names)} names", flush=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(metadata, sort_keys=True) + "\n")
    return metadata


def equivalent(formula, metadata):
    homepage = project(formula.get("homepage"))
    if not homepage:
        return None
    for name in candidates(formula):
        row = metadata.get(name)
        if not row:
            continue
        pages = row["homepage"] if isinstance(row["homepage"], list) else [row["homepage"]]
        if homepage not in [project(page) for page in pages]:
            continue
        stable = formula.get("versions", {}).get("stable") or ""
        version = row["version"]
        major = re.match(r"v?(\d+)", stable)
        native_major = re.match(r"v?(\d+)", version)
        if not major or not native_major or major[1] != native_major[1]:
            continue
        if "@" in formula["name"]:
            family = formula["name"].rsplit("@", 1)[1]
            if not re.fullmatch(r"\d+(\.\d+)*", family):
                continue
            if not re.match(r"v?" + re.escape(family) + r"(?:\.|$)", version):
                continue
        return {"kind": "nixpkgs-equivalent", "attribute": [name], "version": version,
                "status": "eligible", "reason": None, "detail": None, "minMacos": None}
    return None


def excluded(version, reason, detail=None):
    return {"status": "excluded", "version": version, "kind": None, "reason": reason,
            "detail": detail, "minMacos": None}


def bottle_target(formula, system):
    version = formula.get("versions", {}).get("stable") or ""
    if system != "aarch64-darwin":
        return excluded(version, "unsupported-bottle-platform", "Linux bottle relocation is not supported; use a Nixpkgs equivalent")
    files = formula.get("bottle", {}).get("stable", {}).get("files", {})
    tags = [("arm64_sonoma", "14"), ("arm64_sequoia", "15"), ("arm64_tahoe", "26")]
    for tag, minimum in tags:
        if tag not in files:
            continue
        effective = {**formula, **formula.get("variations", {}).get(tag, {})}
        for field in ("dependencies", "recommended_dependencies", "optional_dependencies", "uses_from_macos", "requirements"):
            if not isinstance(effective.get(field), list):
                return excluded(version, "malformed-metadata", field)
            if effective[field]:
                return excluded(version, "formula-dependency", field)
        if effective.get("post_install_defined") is not False:
            return excluded(version, "install-hook", "post_install_defined")
        for field in ("service", "pour_bottle_only_if"):
            if field not in effective or effective[field] is not None:
                return excluded(version, "install-hook", field)
        if effective.get("post_install_steps"):
            return excluded(version, "install-hook", "post_install_steps")
        source = files[tag]
        if source.get("cellar") != ":any_skip_relocation":
            return excluded(version, "homebrew-relocation", str(source.get("cellar")))
        digest = source.get("sha256", "")
        url = source.get("url", "")
        expected = f"https://ghcr.io/v2/homebrew/core/{formula['name'].replace('@', '/')}/blobs/sha256:{digest}"
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or url != expected:
            return excluded(version, "unsupported-bottle-origin")
        if not re.fullmatch(r"[a-zA-Z0-9.+_-]+", version):
            return excluded(version, "invalid-version")
        return {"status": "eligible", "kind": "formula-bottle", "reason": None, "detail": None,
                "version": version, "minMacos": minimum,
                "url": url, "sha256": digest,
                "cellarVersion": version + (f"_{formula['revision']}" if formula.get("revision") else "")}
    return excluded(version, "no-compatible-bottle")


def generate(formulae, metadata, digest):
    if not isinstance(formulae, list) or not formulae:
        raise ValueError("Formula input must be a nonempty array")
    revisions = {formula.get("tap_git_head") for formula in formulae}
    if len(revisions) != 1 or not re.fullmatch(r"[0-9a-f]{40}", next(iter(revisions)) or ""):
        raise ValueError("Formula input must have one pinned Core revision")
    entries = {}; aliases = {}; executables = {}; deprecated = []
    for formula in formulae:
        name = formula.get("name", "")
        if not TOKEN.fullmatch(name) or formula.get("tap") != "homebrew/core" or name in entries:
            raise ValueError("invalid or duplicate Formula identity")
        if any(type(formula.get(field)) is not bool for field in ("disabled", "deprecated")):
            raise ValueError("invalid Formula support flags")
        targets = {}
        for system in SYSTEMS:
            version = formula.get("versions", {}).get("stable") or ""
            if formula.get("disabled") or formula.get("deprecated"):
                target = excluded(version, "disabled" if formula.get("disabled") else "deprecated")
            else:
                target = equivalent(formula, metadata[system]) or bottle_target(formula, system)
            targets[system] = target
        entries[name] = {"name": name, "description": formula.get("desc"), "homepage": formula.get("homepage"), "targets": targets}
        for alias in formula.get("aliases", []):
            if not TOKEN.fullmatch(alias) or alias in aliases:
                raise ValueError("invalid or duplicate Formula alias")
            aliases[alias] = name
        executables[name] = formula.get("executables")
        if formula.get("deprecated"): deprecated.append(name)
    if any(alias in entries and alias != name for alias, name in aliases.items()):
        raise ValueError("Formula alias conflicts with a canonical name")
    return {"schema": "pkg-formula-catalog/1", "generator": {"name": "formula-catalog", "version": "0.1.0"},
            "nixpkgsRevision": REVISION, "macosBaseline": "14", "targets": list(SYSTEMS),
            "inputs": {"homebrew/core": {"url": API, "revision": formulae[0]["tap_git_head"],
                                         "sha256": digest, "license": LICENSE, "kind": "official"}},
            "entries": entries, "aliases": aliases, "executables": executables, "deprecated": deprecated}


def read_snapshot(path, sha256):
    """Return the bytes of a plain or gzip snapshot that match `sha256`."""
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rb") as stream:
        raw = stream.read(MAX_INPUT + 1)
    if hashlib.sha256(raw).hexdigest() != sha256 or len(raw) > MAX_INPUT:
        raise ValueError("input hash or size mismatch")
    return raw


def pinned_input(cache):
    """Download the pinned snapshot into `cache` once and return its bytes."""
    pin = json.loads(PIN.read_text())
    if urllib.parse.urlsplit(pin["url"]).scheme != "https":
        raise ValueError("the pinned snapshot URL is not https")
    path = cache / f"formula-{pin['sha256']}.json.gz"
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        partial = path.with_suffix(".part")
        subprocess.run(["curl", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
                        "--fail", "--location", "--silent", "--show-error", "--max-time", "300",
                        "-o", str(partial), pin["url"]], check=True)
        partial.replace(path)
    return read_snapshot(path, pin["sha256"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=pathlib.Path,
                        help="checked API payload; the default is the pinned snapshot")
    parser.add_argument("--sha256", help="SHA-256 of --input")
    parser.add_argument("--pin-url", help="record --input in inputs/input.json, stored at this URL")
    parser.add_argument("--cache", type=pathlib.Path, default=pathlib.Path(".cache/formula-catalog"))
    parser.add_argument("--output", type=pathlib.Path, default=pathlib.Path("nix/formulae/catalog/catalog.json"))
    parser.add_argument("--nix", default="nix")
    args = parser.parse_args()
    if args.input is None:
        if args.sha256 or args.pin_url:
            parser.error("--sha256 and --pin-url need --input")
        raw = pinned_input(args.cache)
    elif args.sha256 is None:
        parser.error("--sha256 is required with --input")
    else:
        raw = read_snapshot(args.input, args.sha256)
    digest = hashlib.sha256(raw).hexdigest()
    formulae = json.loads(raw)
    metadata = {system: native_metadata(formulae, system, args.nix, args.cache) for system in SYSTEMS}
    catalog = generate(formulae, metadata, digest)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(catalog, sort_keys=True, separators=(",", ":")) + "\n")
    assets = args.output.parent.parent / "lib"
    assets.mkdir(parents=True, exist_ok=True)
    shared = pathlib.Path(__file__).resolve().parents[2] / "nix/casks/lib"
    for filename in ("public-fetch.nix", "safe-fetch.py"):
        shutil.copyfile(shared / filename, assets / filename)
    if args.pin_url:
        pin = {"kind": "official", "license": LICENSE, "revision": catalog["inputs"]["homebrew/core"]["revision"],
               "sha256": digest, "url": args.pin_url}
        PIN.write_text(json.dumps(pin, indent=2, sort_keys=True) + "\n")
    for system in SYSTEMS:
        print(system, dict(Counter(target["targets"][system]["kind"] or target["targets"][system]["reason"] for target in catalog["entries"].values())))


if __name__ == "__main__":
    main()
