#!/usr/bin/env python3
"""Replay real generated catalog plans through the real plan.py helper.

Builds tiny synthetic payloads per plan (zip archive of declared artifact
staging paths, or a naked script for raw-binary), runs the actual
`plan.py unpack` + `plan.py install` subprocesses, and asserts the public
output contract (Applications/<target>, bin/<target>, share/man, completions).
This is helper replay with synthetic payloads, NOT a vendor-install claim.

Selection: without --tokens the whole catalog is replayed (all-catalog
mode, keeping the original qualified source/token identities). With
--tokens, the supplied list contains bare OFFICIAL tokens (resolved to
homebrew/cask/<token>) or full source/token identities (matched by exact
lookup; a bare name is never resolved to some arbitrary public source);
identities absent from the catalog are reported as missing. No popularity
subset is ever invented from catalog order.

Exit status: nonzero if any plan failed, any harness-error or harness-skip
occurred, zero applicable plans were executed, or the processed count does
not match the expected applicable count. The report is written first.

Usage: catalog_plans.py --catalog CATALOG.json [--tokens JSON_LIST]
                        [--jobs N] [--output /tmp/catalog-plan-replay.json]
"""

import argparse
import concurrent.futures as cf
import hashlib
import json
import os
import plistlib
import stat
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
PLAN_PY = os.path.abspath(
    os.path.join(HERE, "..", "..", "nix", "casks", "lib", "plan.py")
)
PY = sys.executable
MARKER = b"PLANREPLAY-OK"
SCRIPT = b"#!/bin/sh\necho PLANREPLAY-OK\n"
COMP_DIRS = {
    "bash-completion": "share/bash-completion/completions",
    "zsh-completion": "share/zsh/site-functions",
    "fish-completion": "share/fish/vendor_completions.d",
}
SCOPE = "synthetic helper replay, not vendor installs"
SCHEMA = "pkg-cask-catalog/3"
OFFICIAL_SOURCE = "homebrew/cask"


class FixtureError(Exception):
    """Fixture construction refused an unsafe or malformed plan input."""


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def safe_join(base, rel, what):
    """Join rel under base, refusing absolute or escaping paths."""
    base = os.path.abspath(base)
    if os.path.isabs(rel):
        raise FixtureError(f"{what}: absolute source path: {rel!r}")
    p = os.path.normpath(os.path.join(base, rel))
    if os.path.commonpath([base, p]) != base:
        raise FixtureError(f"{what}: escaping source path: {rel!r}")
    return p


def write_file(path, data, mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    if mode is not None:
        os.chmod(path, mode)


def build_payload(plan, srcdir):
    """Create declared artifact staging paths. Returns (ok, note)."""
    apps = [a for a in plan["artifacts"] if a["kind"] == "app"]
    for a in apps:  # minimal bundles under their declared source path
        b = safe_join(srcdir, a["source"].rstrip("/"), "app source")
        write_file(
            os.path.join(b, "Contents", "Info.plist"),
            plistlib.dumps({"CFBundleName": a["target"][: -len(".app")]}),
        )
    for a in plan["artifacts"]:
        kind, src = a["kind"], a["source"]
        if kind in ("app", "pkg"):
            continue
        if kind == "binary" and src.startswith("$APPDIR/"):
            # exercise the helper's source->target mapping: create the
            # path inside the DECLARED source app bundle, renamed targets
            # must be bridged by the helper itself
            rest = src[len("$APPDIR/") :]
            declared, _, inner = rest.partition("/")
            owner = [
                x
                for x in apps
                if os.path.basename(x["source"].rstrip("/")) == declared
                or x["target"] == declared
            ]
            if not owner:
                # unresolved $APPDIR is a fixture failure; never fabricate
                # an undeclared app just to go green
                return False, f"unresolved $APPDIR anchor (no declared app): {src}"
            p = safe_join(
                os.path.join(srcdir, owner[0]["source"].rstrip("/")),
                inner,
                "$APPDIR binary source",
            )
            write_file(p, SCRIPT, 0o755)
        else:
            p = safe_join(srcdir, src, f"{kind} source")
            write_file(
                p,
                SCRIPT if kind == "binary" else b"replay\n",
                0o755 if kind == "binary" else None,
            )
    return True, ""


def make_zip(srcdir, payload):
    with zipfile.ZipFile(payload, "w") as zf:
        for root, _dirs, files in os.walk(srcdir):
            for n in files:
                p = os.path.join(root, n)
                zi = zipfile.ZipInfo.from_file(p, os.path.relpath(p, srcdir))
                zi.external_attr = (
                    0o100755 if os.access(p, os.X_OK) else 0o100644
                ) << 16
                with open(p, "rb") as f:
                    zf.writestr(zi, f.read())


def run_helper(args):
    return subprocess.run(
        [PY, PLAN_PY, *args], capture_output=True, text=True, timeout=60, check=False
    )


def assert_output(out, plan):
    """Independent checks against the public artifact contract."""
    for a in plan["artifacts"]:
        kind, tgt = a["kind"], a["target"]
        if kind == "app":
            p = os.path.join(out, "Applications", tgt, "Contents", "Info.plist")
            if not os.path.isfile(p):
                return f"missing app bundle contents: {tgt}"
        elif kind == "binary":
            p = os.path.join(out, "bin", tgt)
            if not os.path.lexists(p):
                return f"missing binary: bin/{tgt}"
            if os.path.islink(p):
                r = os.path.realpath(p)
                if not (
                    os.path.isfile(r)
                    and os.path.commonpath([os.path.realpath(out), r])
                    == os.path.realpath(out)
                ):
                    return f"bad bin/{tgt} symlink -> {r}"
            elif not os.stat(p).st_mode & stat.S_IXUSR:
                return f"bin/{tgt} not executable"
            # execute both regular and symlink outputs; require exit 0
            # and the exact marker
            run = subprocess.run([p], capture_output=True, timeout=10, check=False)
            if run.returncode != 0:
                return f"bin/{tgt} exit {run.returncode}: {run.stderr[:60]!r}"
            if run.stdout.strip() != MARKER:
                return f"bin/{tgt} output mismatch: {run.stdout[:60]!r}"
        elif kind == "manpage":
            n = tgt.rsplit(".", 1)[1]
            if not os.path.isfile(os.path.join(out, "share", "man", f"man{n}", tgt)):
                return f"missing manpage: {tgt}"
        else:
            if not os.path.isfile(os.path.join(out, COMP_DIRS[kind], tgt)):
                return f"missing completion: {kind}/{tgt}"
    return ""


def resolve_identity(name, entries):
    """Map a supplied name to a catalog identity.

    A full source/token identity must match a catalog key exactly. A bare
    name is resolved to the OFFICIAL source only; a random public source is
    never picked. Returns None when no catalog entry matches.
    """
    if name in entries:
        return name
    if "/" in name:
        return None  # a supplied full identity must match exactly
    ident = f"{OFFICIAL_SOURCE}/{name}"
    return ident if ident in entries else None


def replay(identity, system, plan, baseline, rec):
    # Fixed safe prefix: qualified identities contain '/' and must never
    # leak into filesystem paths; the identity stays in report fields.
    with tempfile.TemporaryDirectory(prefix="replay-") as td:
        srcdir = os.path.join(td, "src")
        os.makedirs(srcdir)
        ok, note = build_payload(plan, srcdir)
        if not ok:
            rec.update(status="harness-skip", reason=note)
            return
        raw = plan["archive"]["kind"] == "raw-binary"
        payload = (
            os.path.join(td, "payload") if raw else os.path.join(td, "payload.zip")
        )
        if raw:
            bins = [a for a in plan["artifacts"] if a["kind"] == "binary"]
            with (
                open(safe_join(srcdir, bins[0]["source"], "raw source"), "rb") as f,
                open(payload, "wb") as g,
            ):
                g.write(f.read())
            os.chmod(payload, 0o755)
        else:
            make_zip(srcdir, payload)
        url_name = os.path.basename(plan["source"]["url"].split("?")[0])
        bins = [a for a in plan["artifacts"] if a["kind"] == "binary"]
        raw_name = bins[0]["source"] if raw else ""
        fallback = bins[0]["source"] if len(bins) == 1 and not raw else ""
        staging, out = os.path.join(td, "staging"), os.path.join(td, "out")
        r = run_helper(["unpack", payload, staging, url_name, raw_name, fallback])
        if r.returncode != 0:
            rec.update(status="fail", stage="unpack", stderr=r.stderr.strip()[-500:])
            return
        ctx = os.path.join(td, "plan.json")
        with open(ctx, "w") as f:
            json.dump(
                {
                    "token": identity,
                    "system": system,
                    "plan": plan,
                    "baseline": baseline,
                },
                f,
            )
        r = run_helper(["install", ctx, staging, out])
        if r.returncode != 0:
            rec.update(status="fail", stage="install", stderr=r.stderr.strip()[-500:])
            return
        err = assert_output(out, plan)
        if err:
            rec.update(status="fail", stage="assert", stderr=err)
        else:
            rec["status"] = "pass"


def task(job):
    """Worker: replay one (identity, system) plan; returns the record."""
    identity, system, plan, baseline = job
    rec = {"token": identity, "system": system, "archive": plan["archive"]["kind"]}
    try:
        replay(identity, system, plan, baseline, rec)
    except Exception as e:  # noqa: BLE001 - any harness crash must fail, never pass
        rec.update(
            status="harness-error",
            stage="fixture",
            stderr=f"{type(e).__name__}: {e}",
        )
    return rec


def collect_jobs(entries, order, baseline):
    """Select (identity, system) jobs; count exclusions per system."""
    jobs, skipped, excluded = [], [], {}
    missing = []

    def exclude(identity, system, reason, note=None):
        d = excluded.setdefault(system, {})
        d[reason] = d.get(reason, 0) + 1
        if note is not None:
            skipped.append({"token": identity, "system": system, "reason": note})

    for identity in order:
        entry = entries.get(identity)
        if entry is None:
            missing.append(identity)
            continue
        for system, t in entry.get("targets", {}).items():
            plan = t.get("plan")
            if t.get("status") != "eligible" or not plan:
                exclude(identity, system, "not-eligible")
                continue
            kinds = {a["kind"] for a in plan["artifacts"]}
            ak = plan["archive"]["kind"]
            if ak == "appimage":
                exclude(
                    identity,
                    system,
                    "skip-appimage",
                    "appimage: separate appimageTools fixtures",
                )
                continue
            if "pkg" in kinds:
                exclude(
                    identity,
                    system,
                    "skip-pkg",
                    "pkg: separate xar-relocatable fixtures",
                )
                continue
            if ak not in ("auto", "raw-binary"):
                exclude(identity, system, f"skip-archive-{ak}")
                continue
            jobs.append((identity, system, plan, baseline))
    return jobs, skipped, excluded, missing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalog", required=True)
    ap.add_argument("--tokens", default="", help="JSON list; replay only these tokens")
    ap.add_argument("--jobs", type=int, default=2, help="worker processes (default 2)")
    ap.add_argument("--output", default="/tmp/catalog-plan-replay.json")
    args = ap.parse_args()

    with open(args.catalog) as f:
        cat = json.load(f)
    if cat.get("schema") != SCHEMA:
        print(
            f"catalog schema is {cat.get('schema')!r}, expected {SCHEMA!r}",
            file=sys.stderr,
        )
        sys.exit(1)
    entries = cat.get("entries", {})
    for key, entry in entries.items():
        if (
            not isinstance(entry, dict)
            or not isinstance(entry.get("source"), str)
            or not isinstance(entry.get("token"), str)
            or key != f"{entry['source']}/{entry['token']}"
        ):
            print(
                f"catalog entry {key!r} does not agree with its "
                "source/token identity fields",
                file=sys.stderr,
            )
            sys.exit(1)
    baseline = cat.get("macosBaseline") or "11.0.0"

    selected, tokens_sha, supplied = None, None, None
    if args.tokens:
        with open(args.tokens) as f:
            supplied = json.load(f)
        tokens_sha = sha256_file(args.tokens)
        selected, seen = [], set()
        for tok in supplied:  # dedupe, preserve caller order
            if tok not in seen:
                seen.add(tok)
                selected.append(tok)
        # bare official tokens resolve explicitly; full identities match exactly
        selected = [resolve_identity(t, entries) or t for t in selected]

    jobs, skipped, excluded, missing = collect_jobs(
        entries, selected if selected is not None else list(entries), baseline
    )

    results, fatal = [], ""
    try:
        with cf.ProcessPoolExecutor(max_workers=max(1, args.jobs)) as ex:
            for rec in ex.map(task, jobs, chunksize=8):
                results.append(rec)  # noqa: PERF402 - keep partial results if the executor dies
    except Exception as e:  # noqa: BLE001 - any worker crash must fail, never pass
        fatal = f"executor: {type(e).__name__}: {e}"

    counts, by_system, failures = {}, {}, []
    for rec in results:
        st = rec["status"]
        counts[st] = counts.get(st, 0) + 1
        d = by_system.setdefault(rec["system"], {})
        d[st] = d.get(st, 0) + 1
        if st != "pass":
            failures.append(rec)
    if missing:
        counts["missing-token"] = len(missing)
    for system, d in excluded.items():
        bd = by_system.setdefault(system, {})
        for reason, n in d.items():
            bd[reason] = bd.get(reason, 0) + n

    processed = len(results)
    count_ok = not fatal and processed == len(jobs)
    report = {
        "scope": SCOPE,
        "mode": "selected-tokens" if selected is not None else "all-catalog",
        "identity": "qualified source/token identities (bare official tokens resolved to homebrew/cask/<token>)",
        "catalog_sha256": sha256_file(args.catalog),
        "tokens_sha256": tokens_sha,
        "summary": counts,
        "by_system": by_system,
        "missing_tokens": missing,
        "expected_applicable": len(jobs),
        "processed": processed,
        "count_check_ok": count_ok,
        "failures": failures,
        "skipped": skipped,
    }
    if fatal:
        report["harness_fatal"] = fatal
    with open(args.output, "w") as f:
        json.dump(report, f, indent=1)

    problems = []
    if not count_ok:
        problems.append(f"count check failed (expected {len(jobs)}, got {processed})")
    for kind in ("fail", "harness-error", "harness-skip"):
        if counts.get(kind):
            problems.append(f"{counts[kind]} {kind}")
    if not jobs:
        problems.append("zero applicable plans executed")
    print(json.dumps(counts, indent=1), f"-> {args.output}", file=sys.stderr)
    if problems:
        print("REPLAY NOT GREEN: " + "; ".join(problems), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
