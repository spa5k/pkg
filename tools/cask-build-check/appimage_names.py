#!/usr/bin/env python3
"""AppImage launcher-name regression for nix/casks/lib/builders.nix.

Evaluates the real builder (flake import, pinned nixpkgs) and runs ONLY
the extracted rename `mv` line under /bin/sh in an isolated fake $out
whose cwd is the output directory, so hostile `touch MARKER` payloads
land exactly there. No vendor payload is built or downloaded. Nix may
fetch its pinned nixpkgs input if it is not cached.

1. beaver-notes / unity-hub: mainProgram + rename round-trip;
2. hostile-but-legal names (spaces, quotes, $(), backticks, `;`, dash,
   Unicode): rename succeeds, payload survives, no stray file in $out;
3. invalid targets (null, int, "", ".", "..", "a/b", control chars):
   evaluation fails with the builder's diagnostic, not a parser error.

Run: python3 tools/cask-build-check/appimage_names.py
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
FLAKE = os.path.join(REPO, "nix", "casks")
BUILDERS = os.path.join(FLAKE, "lib", "builders.nix")
VALID = [
    "Beaver Notes",
    "Kamran's Notes",
    'Editor "Quoted" Pro',
    "$(touch m1) notes",
    "back `touch m2` tick",
    "semi; touch m3; colon",
    "-leading-dash",
    "Café ☃ 中文",
]
INVALID = [None, 7, "", ".", "..", "a/b", "a\nb", "a\rb", "a\tb", "a\x7fb"]
PASS = 0
FAIL = 0


def ok(name):
    global PASS
    PASS += 1
    print(f"PASS: {name}")


def bad(name, why):
    global FAIL
    FAIL += 1
    print(f"FAIL: {name}: {why}")


def nix_eval(args, expect_ok=True):
    proc = subprocess.run(
        ["nix", "eval", "--json", "--no-write-lock-file", *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if expect_ok and proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip().splitlines()[-1] or "nix eval failed")
    return proc


def expected_diagnostic(value):
    if not isinstance(value, str):
        return "must be a string"
    return "needs a target" if value == "" else "not a safe launcher name"


def extract_mv(command, case):
    """Unique `mv` rename line from the real buildCommand (hook only)."""
    lines = [ln.strip() for ln in command.splitlines() if ln.strip().startswith("mv ")]
    if len(lines) != 1:
        bad(case, f"expected exactly one 'mv' line in buildCommand, got {len(lines)}")
        return None
    return lines[0]


def run_mv(line, wrapper, expected, case):
    """Execute the extracted rename line; cwd = fake $out, so stray
    files created by injection land directly in $out and are detected."""
    out = tempfile.mkdtemp(prefix="appimage-fx-")
    try:
        bindir = os.path.join(out, "bin")
        os.mkdir(bindir)
        write(os.path.join(bindir, wrapper), "WRAPPED-PAYLOAD")
        proc = subprocess.run(
            ["/bin/sh", "-c", line],
            cwd=out,
            env={"out": out, "PATH": "/usr/bin:/bin"},
            capture_output=True,
            text=True,
            check=False,
        )
        stray = [f for f in os.listdir(out) if f != "bin"]
        files = os.listdir(bindir)
        if proc.returncode != 0:
            bad(case, f"rename line failed: {proc.stderr.strip()}")
        elif stray:
            bad(case, f"marker/stray file executed in $out: {stray}")
        elif files != [expected]:
            bad(case, f"expected bin/ [{expected!r}], got {files!r}")
        else:
            with open(os.path.join(bindir, expected), encoding="utf-8") as fh:
                if fh.read() != "WRAPPED-PAYLOAD":
                    bad(case, "renamed file lost the wrapper payload")
                else:
                    ok(f"rename: {case}")
    finally:
        shutil.rmtree(out, ignore_errors=True)


def write(path, text):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)


CORE = """let
  flake = builtins.getFlake "path:{flake}";
  pkgs = import flake.inputs.nixpkgs {{ system = "x86_64-linux"; config.allowUnfree = true; }};
  names = builtins.fromJSON (builtins.readFile {names_json});
  fixed = import {builders} {{ inherit pkgs; }};
  mk = t: fixed.buildEntry {{
    token = "stub"; system = "x86_64-linux"; version = "1.0";
    description = null; homepage = null; baseline = "15.7.7";
    plan = {{
      source.url = "https://example.invalid/p.appimage";
      source.sha256 = "{zeros}"; archive.kind = "appimage";
      artifacts = [ {{ kind = "appimage"; source = "p.appimage"; target = t; }} ];
      minMacos = null;
    }};
  }};
in
{{ inherit mk names; }}"""

PROBE = """let c = import ./core.nix; in
{ fixedCommands = map (t: (c.mk t).buildCommand) c.names.valid;
   fixedInvalidOk = map (t: (builtins.tryEval ((c.mk t).drvPath)).success) c.names.invalid;
}"""


def main():
    # 1. real packages: declared target survives the real flake evaluation
    for token, target in (("beaver-notes", "Beaver Notes"), ("unity-hub", "Unity Hub")):
        try:
            pkg = json.loads(
                nix_eval(
                    [
                        f"path:{FLAKE}#packages.x86_64-linux.{token}",
                        "--apply",
                        "p: { c = p.buildCommand; m = p.meta.mainProgram; }",
                    ]
                ).stdout
            )
        except RuntimeError as err:
            bad(token, f"eval failed: {err}")
            continue
        if pkg["m"] != target:
            bad(token, f"mainProgram {pkg['m']!r} != {target!r}")
            continue
        if line := extract_mv(pkg["c"], token):
            run_mv(line, f"cask-{token}", target, f"real {token}")

    work = tempfile.mkdtemp(prefix="appimage-nix-")
    try:
        write(
            os.path.join(work, "names.json"),
            json.dumps({"valid": VALID, "invalid": INVALID}, ensure_ascii=False),
        )
        write(
            os.path.join(work, "core.nix"),
            CORE.format(
                flake=FLAKE,
                builders=json.dumps(BUILDERS),
                names_json=json.dumps(os.path.join(work, "names.json")),
                zeros="0" * 64,
            ),
        )
        write(os.path.join(work, "probe.nix"), PROBE)

        # 2. one eval of the real builder for valid and invalid names
        try:
            probe = json.loads(
                nix_eval(["--file", os.path.join(work, "probe.nix")]).stdout
            )
        except RuntimeError as err:
            bad("probe eval", str(err))
            probe = None
        if probe is not None:
            commands = probe["fixedCommands"]
            if len(commands) != len(VALID):
                bad(
                    "valid names",
                    f"got {len(commands)} commands for {len(VALID)} names",
                )
            else:
                for name, command in zip(VALID, commands, strict=True):
                    if line := extract_mv(command, f"target {name!r}"):
                        run_mv(line, "cask-stub", name, f"target {name!r}")
            invalid_ok = probe["fixedInvalidOk"]
            if len(invalid_ok) != len(INVALID):
                bad(
                    "invalid names",
                    f"got {len(invalid_ok)} results for {len(INVALID)} names",
                )
            else:
                for value, good in zip(INVALID, invalid_ok, strict=True):
                    if good:
                        bad(f"invalid {value!r}", "evaluation unexpectedly succeeded")
                    else:
                        ok(f"invalid {value!r}: drvPath eval threw as required")

        # 3. per-case eval: the builder's own diagnostic, never a parser error
        case_json = os.path.join(work, "case.json")
        write(
            os.path.join(work, "case.nix"),
            "let c = import ./core.nix; in\n"
            f"(c.mk (builtins.fromJSON (builtins.readFile {json.dumps(case_json)}))).drvPath\n",
        )
        for value in INVALID:
            write(case_json, json.dumps(value, ensure_ascii=False))
            proc = nix_eval(["--file", os.path.join(work, "case.nix")], expect_ok=False)
            want = expected_diagnostic(value)
            if proc.returncode == 0:
                bad(f"diagnostic {value!r}", "evaluation unexpectedly succeeded")
            elif "syntax error" in proc.stderr or "parse error" in proc.stderr:
                bad(
                    f"diagnostic {value!r}", "parser error instead of the builder check"
                )
            elif want not in proc.stderr:
                bad(f"diagnostic {value!r}", f"missing {want!r} in nix error output")
            else:
                ok(f"diagnostic {value!r}: {want}")
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print("----")
    print(f"pass={PASS} fail={FAIL}")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
