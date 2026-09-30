#!/usr/bin/env python3
"""Offline audit of the cask catalog against raw pinned data and Homebrew analytics.

This is a metadata audit only. It never downloads anything, never installs a
cask, and never executes catalog plans. "Eligible" rows were not installed;
the checks here are bounded structural re-derivations from the pinned raw
record, not a proof of classifier equivalence.

Inputs are explicit paths:
  --input      raw pinned cask.json (list of cask records)
  --catalog    generated catalog.json (nix/casks/catalog/catalog.json)
  --analytics  local copy of api/analytics/cask-install/homebrew-cask/90d.json
  --output     directory for corpus-report.json / top1000.csv

Provenance flags --analytics-url and --retrieved are recorded verbatim; this
script performs no network access. Exit codes: 0 clean, 1 invariant violation,
2 invalid input. Documented exclusion reasons in the catalog are outcomes to
report, not failures.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import sys
import tempfile
from collections import Counter
from datetime import datetime, timezone

TARGETS = ["aarch64-darwin", "x86_64-linux"]
SCHEMA = "pkg-cask-catalog/4"
OFFICIAL_SOURCE = "homebrew/cask"
VARIATION_KEY = {"aarch64-darwin": "arm64_sequoia", "x86_64-linux": "x86_64_linux"}
PLATFORM_TAG = {"aarch64-darwin": "arm64_sequoia", "x86_64-linux": "x86_64_linux"}
PLAN_KINDS = {
    "app",
    "binary",
    "pkg",
    "appimage",
    "manpage",
    "bash-completion",
    "zsh-completion",
    "fish-completion",
}
ARCHIVE_KINDS = {"auto", "raw-binary", "appimage"}
TOKEN_RE = re.compile(r"^[a-z0-9][a-z0-9+._@-]*$")


# ---------------------------------------------------------------- self-checks


def official_input_problem(pin: object) -> str | None:
    """Return the reason the official json-snapshot input shape is invalid, else None."""
    if not (
        isinstance(pin, dict)
        and isinstance(pin.get("url"), str)
        and isinstance(pin.get("license"), str)
    ):
        return f"catalog input provenance for {OFFICIAL_SOURCE} is missing"
    if pin.get("kind") != "json-snapshot":
        return f"official input kind must be json-snapshot, not {pin.get('kind')!r}"
    if "raw" not in pin:
        return "official input must carry an explicit raw key with value null"
    if pin["raw"] is not None:
        return "official input raw must be null (final schema3 shape)"
    return None


def catalog_inputs_problem(inputs: object) -> str | None:
    """Return the reason the catalog inputs map is invalid, else None.

    The official audit inputs map contains exactly homebrew/cask; extra
    provenance entries are invalid input.
    """
    if not isinstance(inputs, dict) or not inputs:
        return "catalog has no inputs map"
    if set(inputs) != {OFFICIAL_SOURCE}:
        return (
            f"catalog inputs must be exactly {OFFICIAL_SOURCE!r}, "
            f"not {sorted(map(str, inputs))}"
        )
    return official_input_problem(inputs[OFFICIAL_SOURCE])


def entry_source_counts(entries: dict) -> dict[str, int]:
    """Count catalog rows per source with explicit malformed buckets.

    Nonobject rows and rows without a string source field are counted
    under explicit buckets, so malformed catalogs cannot crash report
    generation and mixed None/string values are never sorted.
    """
    counts: dict[str, int] = {}
    for entry in entries.values():
        if not isinstance(entry, dict):
            key = "<malformed-row>"
        else:
            src = entry.get("source")
            key = src if isinstance(src, str) else "<nonstring-or-missing-source>"
        counts[key] = counts.get(key, 0) + 1
    return dict(sorted(counts.items()))


def _selfcheck() -> None:
    """Tiny inline examples that pin ranking aggregation and input validation."""
    # Final schema3 official input shape: kind json-snapshot and raw null.
    ok_pin = {
        "kind": "json-snapshot",
        "url": "https://example.com/cask.json",
        "license": "MIT",
        "sha256": "a" * 64,
        "revision": "b" * 40,
        "raw": None,
    }
    assert official_input_problem(ok_pin) is None, ok_pin
    for bad in (
        {**ok_pin, "kind": "tarball"},
        {**ok_pin, "raw": {"path": "cask.json", "sha256": "a" * 64}},
        {**ok_pin, "raw": "cask.json"},
        {k: v for k, v in ok_pin.items() if k != "raw"},
        {"url": "https://example.com"},
    ):
        assert official_input_problem(bad) is not None, bad
    # The inputs map contains exactly homebrew/cask; extra provenance is
    # invalid input.
    assert catalog_inputs_problem({OFFICIAL_SOURCE: ok_pin}) is None
    assert catalog_inputs_problem({}) is not None
    assert (
        catalog_inputs_problem({OFFICIAL_SOURCE: ok_pin, "other/tap": ok_pin})
        is not None
    )
    ranked = rank_analytics(
        {
            "b": [{"cask": "b", "count": "10"}],
            "a": [{"cask": "a", "count": "10"}],
            "c": [{"cask": "c", "count": "1,000"}, {"cask": "c", "count": "1,000"}],
        }
    )
    # Integer counts (comma-stripped), array rows summed, descending; stable token tie-break.
    assert [t for t, _ in ranked] == ["c", "a", "b"], ranked
    # Top-N truncates exactly; no backfill of lower ranks when entries are missing.
    top = top_tokens(ranked, 2)
    assert top == [("c", 2000), ("a", 10)], top
    assert len(top_tokens(ranked, 99)) == 3  # never invents entries
    # Negative or non-numeric install counts are invalid input, never silently used.
    for bad in (-1, "1;2", "many"):
        try:
            rank_analytics({"t": [{"cask": "t", "count": bad}]})
        except ValueError:
            pass
        else:
            raise AssertionError(f"count {bad!r} must be rejected")
    # Effective merge: variation replaces fields, token/variations never taken.
    eff = effective_record(
        {
            "token": "t",
            "url": "u",
            "sha256": "a",
            "supported_platforms": ["arm64_sequoia"],
        },
        "aarch64-darwin",
        {"arm64_sequoia": {"sha256": "b", "token": "EVIL"}},
    )
    assert eff["sha256"] == "b" and eff["url"] == "u" and eff["token"] == "t", eff
    # Platform gate re-derivation, first decision stages only.
    assert (
        gate_reason({"supported_platforms": ["x86_64_linux"]}, "aarch64-darwin")
        == "unsupported-platform"
    )
    assert (
        gate_reason(
            {"supported_platforms": ["arm64_sequoia"], "depends_on": {"linux": []}},
            "aarch64-darwin",
        )
        == "unsupported-platform"
    )
    assert (
        gate_reason(
            {"supported_platforms": ["arm64_sequoia", "x86_64_linux"]}, "x86_64-linux"
        )
        is None
    )
    # Token validity mirrors the generator's contract (charset, no "..").
    for ok in ["iterm2", "1password-cli@beta", "xournal++", "v2.1"]:
        assert is_valid_token(ok), ok
    for bad in ["", "Foo", "..", "a..b", "-lead", "a/b", "a b", "@x"]:
        assert not is_valid_token(bad), bad
    # Artifact names: relative sources, single-component targets, no controls.
    assert (
        safe_source("dir/tool") and not safe_source("../x") and not safe_source("a\x1b")
    )
    # Tilde-prefixed relative sources stay valid; the generator accepts them.
    assert safe_source("~/.config/tool/rc")
    assert (
        safe_target("Tool.app")
        and not safe_target("bin/Tool")
        and not safe_target("T\x00ol")
    )
    # agg: a two-platform report counts each matched token once, not once per
    # target; a and b have analytics, c does not.
    agg_entries = {
        t: {
            "targets": {
                s: {
                    "status": status,
                    "reason": reason,
                    "kind": kind,
                    "plan": None,
                }
                for s in TARGETS
            }
        }
        for t, status, reason, kind in (
            ("a", "eligible", None, "binary"),
            ("b", "excluded", "disabled", None),
            ("c", "excluded", "disabled", None),
        )
    }
    rep = agg(agg_entries, {"a": 10, "b": 5}, list(agg_entries))
    assert rep["tokensWithAnalytics"] == 2, rep["tokensWithAnalytics"]
    assert rep["perTarget"]["aarch64-darwin"]["statuses"] == {
        "eligible": 1,
        "excluded": 2,
    }
    assert rep["popularityWeighted"]["aarch64-darwin"] == {
        "eligible": 10,
        "excluded": 5,
    }
    # Schema-3 identity: the qualified key must equal source + "/" + token.
    # Unexpected sources and duplicate bare tokens are reported, never
    # silently dropped and never overwritten (first entry wins).
    by_tok, probs = normalize_entries(
        {
            "homebrew/cask/a": {"source": "homebrew/cask", "token": "a"},
            "other/tap/b": {"source": "other/tap", "token": "b"},
        }
    )
    assert set(by_tok) == {"a", "b"} and len(probs) == 1, (by_tok, probs)
    assert probs[0].startswith("unexpected-source:"), probs
    by_tok, probs = normalize_entries(
        {
            "homebrew/cask/a": {"source": "homebrew/cask", "token": "a", "k": 1},
            "other/tap/a": {"source": "other/tap", "token": "a", "k": 2},
        }
    )
    assert by_tok["a"]["k"] == 1, by_tok  # duplicate never overwrites
    assert {p.split(":", 1)[0] for p in probs} == {
        "unexpected-source",
        "duplicate-token-across-sources",
    }, probs
    by_tok, probs = normalize_entries(
        {"homebrew/cask/a": {"source": "homebrew/cask", "token": "z"}}
    )
    assert probs == [
        (
            "qualified-key-mismatch: key 'homebrew/cask/a' != "
            "source/token 'homebrew/cask'/'z'"
        )
    ], probs
    by_tok, probs = normalize_entries({"homebrew/cask/a": "not-an-object"})
    assert by_tok == {} and probs[0].startswith("entry-not-object:"), probs
    # End-to-end: malformed catalog rows (nonobject entry, missing source)
    # must yield a violation report (audit exit 1 in main), never a crash.
    with tempfile.TemporaryDirectory(prefix="audit-self-") as tmp:
        raw_path = os.path.join(tmp, "cask.json")
        with open(raw_path, "w", encoding="utf-8") as fh:
            json.dump([{"token": "a"}], fh)
        catalog = {
            "schema": SCHEMA,
            "entries": {
                "homebrew/cask/a": {
                    "source": OFFICIAL_SOURCE,
                    "token": "a",
                    "targets": {
                        s: {
                            "status": "excluded",
                            "reason": "disabled",
                            "plan": None,
                            "kind": None,
                        }
                        for s in TARGETS
                    },
                },
                "homebrew/cask/b": "not-an-object",
                "homebrew/cask/c": {"token": "c"},
            },
            "targets": TARGETS,
            "macosBaseline": "15.7.7",
            "generator": {"name": "gen", "version": "1"},
            "inputs": {
                OFFICIAL_SOURCE: {
                    "kind": "json-snapshot",
                    "url": "https://example.com/cask.json",
                    "license": "MIT",
                    "sha256": sha256_file(raw_path),
                    "revision": "b" * 40,
                    "raw": None,
                }
            },
        }
        cat_path = os.path.join(tmp, "catalog.json")
        with open(cat_path, "w", encoding="utf-8") as fh:
            json.dump(catalog, fh)
        ana_path = os.path.join(tmp, "analytics.json")
        with open(ana_path, "w", encoding="utf-8") as fh:
            json.dump({"formulae": {}}, fh)
        rep = audit(raw_path, cat_path, ana_path, tmp, None, None)
        assert rep["invariants"]["violationCount"] >= 1, rep["invariants"]
        vids = {msg.split(":", 1)[0] for msg in rep["invariants"]["violations"]}
        assert {"entry-not-object", "entry-identity-missing"} <= vids, vids
        srcs = rep["inputs"]["catalog"]["entrySources"]
        assert srcs.get("<malformed-row>") == 1, srcs
        assert srcs.get("<nonstring-or-missing-source>") == 1, srcs
        assert srcs.get(OFFICIAL_SOURCE) == 1, srcs


# ------------------------------------------------------------- raw re-derivation


def effective_record(rec: dict, system: str, variations: dict | None = None) -> dict:
    """Base record merged with the target variation (variation wins; token/variations never taken)."""
    eff = dict(rec)
    var = (variations or rec.get("variations") or {}).get(VARIATION_KEY[system])
    if isinstance(var, dict):
        for k, v in var.items():
            if k not in ("token", "variations"):
                eff[k] = v
    return eff


def gate_reason(eff: dict, system: str) -> str | None:
    """Re-derive the catalog's first decision stages: disabled, deprecated, platform scope.

    This is a bounded check. Arch constraints, download specs, artifact
    handling and everything after them are not re-derived here; the report's
    invariants note states this boundary explicitly.
    """
    for flag in ("disabled", "deprecated"):
        if eff.get(flag) not in (None, False):
            return flag
    tags = eff.get("supported_platforms")
    if not isinstance(tags, list):
        return "malformed-record"
    if any(not isinstance(t, str) for t in tags):
        return "malformed-record"
    if PLATFORM_TAG[system] not in tags:
        return "unsupported-platform"
    if (
        system == "aarch64-darwin"
        and isinstance(eff.get("depends_on"), dict)
        and eff["depends_on"].get("linux") is not None
    ):
        return "unsupported-platform"
    return None


# --------------------------------------------------------------- analytics


def parse_count(raw: object) -> int:
    """One analytics install count: comma-stripped nonnegative integer."""
    if raw is None:
        return 0
    text = str(raw).replace(",", "").strip()
    if not re.fullmatch(r"\d+", text):
        raise ValueError(f"install count is not a nonnegative integer: {raw!r}")
    return int(text)


def rank_analytics(formulae: dict) -> list[tuple[str, int]]:
    """Aggregate per-token install counts, then sort count desc, token asc."""
    out = []
    for token, rows in formulae.items():
        if not isinstance(rows, list):
            rows = [rows]  # tolerate a single object
        total = 0
        for row in rows:
            if not isinstance(row, dict):
                raise ValueError(  # noqa: TRY004 - routes to invalid input, exit 2
                    f"analytics entry {token!r} holds a non-object row"
                )
            total += parse_count(row.get("count"))
        out.append((str(token), total))
    out.sort(key=lambda tc: (-tc[1], tc[0]))
    return out


def top_tokens(ranked: list[tuple[str, int]], n: int) -> list[tuple[str, int]]:
    """First n of the ranking, exactly; missing catalog entries are never replaced."""
    return ranked[:n]


# ------------------------------------------------------------------- helpers


def is_valid_token(token: str) -> bool:
    """Mirror the generator's catalog-key contract: charset plus no `..`."""
    return bool(TOKEN_RE.fullmatch(token)) and ".." not in token


def has_ascii_controls(name: str) -> bool:
    return any(ord(c) < 0x20 or ord(c) == 0x7F for c in name)


def safe_source(name: object) -> bool:
    """A safe relative artifact source: nonempty, no traversal, no controls.

    Tilde-prefixed sources are valid because the generator accepts them.
    """
    if not isinstance(name, str) or not name or has_ascii_controls(name):
        return False
    if name.startswith(("/", "$")) and not name.startswith("$APPDIR/"):
        return False
    path = name.removeprefix("$APPDIR/")
    if path.startswith("/"):
        return False
    return all(part not in ("", ".", "..") for part in path.split("/"))


def safe_target(name: object) -> bool:
    """A safe artifact target: one nonempty path component, no controls."""
    return (
        isinstance(name, str)
        and name != ""
        and name not in (".", "..")
        and "/" not in name
        and not has_ascii_controls(name)
    )


def normalize_entries(entries: dict) -> tuple[dict, list[str]]:
    """Validate schema-3 qualified identity; normalize entries to bare tokens.

    Each catalog key is the qualified ``source/token`` identity. The key
    must agree with the row's ``source`` and ``token`` fields. Rows from an
    unexpected source are reported, never silently dropped. A bare token
    seen more than once is reported and the first entry is kept; a duplicate
    never overwrites an earlier row.
    """
    problems: list[str] = []

    def p(vid: str, msg: str) -> None:
        if len(problems) < 200:
            problems.append(f"{vid}: {msg}")

    by_token: dict[str, dict] = {}
    for key, entry in entries.items():
        if not isinstance(entry, dict):
            p("entry-not-object", key)
            continue
        source, token = entry.get("source"), entry.get("token")
        if not isinstance(source, str) or not isinstance(token, str):
            p("entry-identity-missing", f"{key}: source/token fields missing")
            continue
        if key != f"{source}/{token}":
            p(
                "qualified-key-mismatch",
                f"key {key!r} != source/token {source!r}/{token!r}",
            )
        if not is_valid_token(token):
            p("catalog-invalid-token-key", key)
        if source != OFFICIAL_SOURCE:
            p("unexpected-source", f"{key}: source {source!r} is not official")
        if token in by_token:
            p(
                "duplicate-token-across-sources",
                f"{token}: {key} duplicates a token already normalized",
            )
            continue  # first entry wins; never overwrite
        by_token[token] = entry
    return by_token, problems


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def fail_invalid(msg: str) -> None:
    print(f"INVALID INPUT: {msg}", file=sys.stderr)
    sys.exit(2)


def is_hex64(text: object) -> bool:
    return isinstance(text, str) and re.fullmatch(r"[0-9a-fA-F]{64}", text) is not None


def agg(entries: dict, counts: dict, tokens: list[str] | None) -> dict:
    """Aggregate per-target statuses, kinds, and popularity-weighted counts.

    tokensWithAnalytics counts each selected catalog token at most once, not
    once per platform target.
    """
    scope = list(entries) if tokens is None else tokens
    matched = sum(1 for t in scope if t in entries and counts.get(t) is not None)
    per_target = {}
    weighted = {}
    for system in TARGETS:
        reasons = Counter()
        kinds = Counter()
        statuses = Counter()
        art_kinds = Counter()
        w_elig = w_exc = 0
        for token in scope:
            if token not in entries:
                continue
            st = entries[token]["targets"][system]
            statuses[st["status"]] += 1
            reasons[(st["status"], st["reason"])] += 1
            if st["kind"]:
                kinds[st["kind"]] += 1
            if st["status"] == "eligible" and isinstance(st.get("plan"), dict):
                for a in st["plan"].get("artifacts") or []:
                    art_kinds[a.get("kind")] += 1
            count = counts.get(token)
            if count is not None:
                if st["status"] == "eligible":
                    w_elig += count
                else:
                    w_exc += count
        per_target[system] = {
            "statuses": dict(statuses),
            "reasons": {
                f"{s}/{r}": n
                for (s, r), n in sorted(reasons.items(), key=lambda kv: -kv[1])
            },
            "kinds": dict(kinds),
            "planArtifactKinds": dict(art_kinds),
        }
        weighted[system] = {"eligible": w_elig, "excluded": w_exc}
    return {
        "perTarget": per_target,
        "popularityWeighted": weighted,
        "tokensWithAnalytics": matched,
        "weightingNote": "counts come from the pinned analytics only; tokens absent from analytics contribute 0",
    }


# ----------------------------------------------------------------- audit core


def audit(
    input_path: str,
    catalog_path: str,
    analytics_path: str,
    out_dir: str,
    analytics_url: str | None,
    retrieved: str | None,
) -> dict:
    try:

        def load_json(path: str):
            with open(path, encoding="utf-8") as fh:
                return json.load(fh)

        raw = load_json(input_path)
        catalog = load_json(catalog_path)
        analytics = load_json(analytics_path)
    except (OSError, json.JSONDecodeError) as exc:
        fail_invalid(f"cannot load inputs: {exc}")
    # ---- schema: raw snapshot
    if not isinstance(raw, list) or not all(
        isinstance(r, dict) and isinstance(r.get("token"), str) for r in raw
    ):
        fail_invalid("raw input must be a list of records with string tokens")
    # ---- schema: catalog envelope (schema3 only; no schema2 fallback)
    if catalog.get("schema") != SCHEMA:
        fail_invalid(f"catalog schema must be {SCHEMA}, not {catalog.get('schema')!r}")
    entries = catalog.get("entries")
    if not isinstance(entries, dict):
        fail_invalid("catalog has no entries object")
    if catalog.get("targets") != TARGETS:
        fail_invalid(f"catalog targets must be {TARGETS}")
    for field in ("macosBaseline",):
        if not isinstance(catalog.get(field), str):
            fail_invalid(f"catalog {field} is missing or not a string")
    gen = catalog.get("generator")
    if not (
        isinstance(gen, dict)
        and isinstance(gen.get("name"), str)
        and isinstance(gen.get("version"), str)
    ):
        fail_invalid("catalog generator identity is missing")
    inputs = catalog.get("inputs")
    if problem := catalog_inputs_problem(inputs):
        fail_invalid(problem)
    pin = inputs[OFFICIAL_SOURCE]
    # ---- schema: analytics
    formulae = analytics.get("formulae")
    if not isinstance(formulae, dict):
        fail_invalid("analytics has no formulae object")
    try:
        ranked = rank_analytics(formulae)
    except ValueError as exc:
        fail_invalid(str(exc))

    violations: list[str] = []

    def v(vid: str, msg: str) -> None:
        if len(violations) < 200:
            violations.append(f"{vid}: {msg}")

    # ---- provenance: the catalog must name exactly the audited raw bytes
    raw_sha = sha256_file(input_path)
    if not is_hex64(pin.get("sha256")):
        v("catalog-input-sha256-shape", str(pin.get("sha256")))
    elif pin["sha256"].lower() != raw_sha:
        v(
            "catalog-input-sha256-mismatch",
            f"catalog pins {pin['sha256']} but {input_path} hashes to {raw_sha}",
        )
    if not (
        isinstance(pin.get("revision"), str)
        and re.fullmatch(r"[0-9a-f]{40}", pin["revision"])
    ):
        v("catalog-input-revision-shape", str(pin.get("revision")))
    if not str(pin.get("url", "")).startswith(("https://", "http://")):
        v("catalog-input-url", str(pin.get("url")))
    if not re.fullmatch(r"\d+(\.\d+)*", str(catalog.get("macosBaseline", ""))):
        v("macos-baseline-shape", str(catalog.get("macosBaseline")))

    # ---- structural invariants over every catalog row (all tokens, not top1000).
    # Schema-3 rows carry a qualified source/token identity; normalize to bare
    # tokens after validating that identity. Identity problems are violations,
    # never silent drops or overwrites.
    raw_by_token: dict[str, dict] = {}
    for rec in raw:
        token = rec["token"]
        if not is_valid_token(token):
            v("raw-invalid-token", token)
        if token in raw_by_token:
            v("raw-duplicate-token", token)
        raw_by_token[token] = rec

    by_token, norm_problems = normalize_entries(entries)
    for msg in norm_problems:
        vid, _, detail = msg.partition(":")
        v(vid, detail.strip())

    dest_owners: dict[str, dict[str, list[str]]] = {t: {} for t in TARGETS}
    collisions: list[str] = []
    for token, entry in by_token.items():
        rec = raw_by_token.get(token)
        if rec is None:
            v("token-not-in-raw", token)
        tgts = entry.get("targets")
        if not isinstance(tgts, dict) or set(tgts) != set(TARGETS):
            v(
                "targets-shape",
                f"{token}: {sorted(tgts) if isinstance(tgts, dict) else tgts}",
            )
            continue
        for system in TARGETS:
            st = tgts[system]
            status, reason, plan, kind = (
                st.get("status"),
                st.get("reason"),
                st.get("plan"),
                st.get("kind"),
            )
            if status not in ("eligible", "excluded"):
                v("status-value", f"{token}/{system}: {status!r}")
            if status == "eligible":
                if reason is not None:
                    v("eligible-with-reason", f"{token}/{system}: {reason!r}")
                if not isinstance(plan, dict):
                    v("eligible-without-plan", f"{token}/{system}")
                if not isinstance(kind, str):
                    v("eligible-without-kind", f"{token}/{system}")
            else:
                if not isinstance(reason, str) or not reason:
                    v("excluded-without-reason", f"{token}/{system}")
                if plan is not None:
                    v("excluded-with-plan", f"{token}/{system}")
                if kind is not None:
                    v("excluded-with-kind", f"{token}/{system}")
            if isinstance(plan, dict):
                src = plan.get("source") or {}
                if not isinstance(src.get("url"), str) or not src["url"].startswith(
                    ("https://", "http://")
                ):
                    v("plan-source-url", f"{token}/{system}")
                if not is_hex64(src.get("sha256")):
                    v("plan-source-sha256", f"{token}/{system}")
                if plan.get("archive", {}).get("kind") not in ARCHIVE_KINDS:
                    v("plan-archive-kind", f"{token}/{system}: {plan.get('archive')}")
                arts = plan.get("artifacts")
                if not isinstance(arts, list) or not arts:
                    v("plan-artifacts", f"{token}/{system}")
                else:
                    seen: set[str] = set()
                    for a in arts:
                        if not isinstance(a, dict) or a.get("kind") not in PLAN_KINDS:
                            v("plan-artifact-kind", f"{token}/{system}: {a}")
                            continue
                        if not safe_source(a.get("source")):
                            v("unsafe-artifact-name", f"{token}/{system}: {a}")
                        if a["kind"] == "pkg":
                            if a.get("target") is not None:
                                v("pkg-with-target", f"{token}/{system}: {a}")
                            continue  # pkgs have no destination by design
                        if not safe_target(a.get("target")):
                            v("unsafe-artifact-name", f"{token}/{system}: {a}")
                        key = f"{a['kind']}:{a.get('target')}"
                        if key in seen:
                            v(
                                "duplicate-destination-in-plan",
                                f"{token}/{system}: {key}",
                            )
                        seen.add(key)
                        bucket = dest_owners[system].setdefault(
                            str(a.get("target")), []
                        )
                        if token not in bucket:
                            bucket.append(token)
            # Independent bounded platform re-derivation from explicit raw metadata.
            if rec is not None:
                eff = effective_record(rec, system)
                expected = gate_reason(eff, system)
                if reason in (
                    "disabled",
                    "deprecated",
                    "unsupported-platform",
                    "malformed-record",
                ):
                    if (
                        reason == "unsupported-platform"
                        and expected != "unsupported-platform"
                    ):
                        v(
                            "platform-mismatch",
                            f"{token}/{system}: catalog unsupported-platform, gate says {expected!r}",
                        )
                    elif (
                        reason == "malformed-record"
                        and expected == "unsupported-platform"
                    ):
                        v(
                            "platform-mismatch",
                            f"{token}/{system}: catalog malformed-record, gate says unsupported-platform",
                        )
                elif expected in (
                    "disabled",
                    "deprecated",
                    "unsupported-platform",
                    "malformed-record",
                ):
                    v(
                        "gate-not-applied",
                        f"{token}/{system}: gate says {expected!r}, catalog says {status}/{reason!r}",
                    )
    for token in raw_by_token:
        if token not in by_token:
            v("raw-token-missing-from-catalog", token)
    for system in TARGETS:
        for dest, owners in dest_owners[system].items():
            if len(owners) > 1 and len(collisions) < 100:
                collisions.append(f"{system}: {dest!r} owned by {owners}")

    # ---- aggregates; popularity weights always come from actual analytics counts
    counts = dict(ranked)
    top = top_tokens(ranked, 1000)
    matched = [t for t, _ in top if t in by_token]
    missing = [t for t, _ in top if t not in by_token]

    report = {
        "generatedAtUtc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "scope": "metadata audit only; no installs were attempted or validated",
        "inputs": {
            "raw": {
                "path": input_path,
                "sha256": raw_sha,
                "records": len(raw),
                "catalogPinnedSha256Matches": pin.get("sha256", "").lower() == raw_sha,
            },
            "catalog": {
                "path": catalog_path,
                "entries": len(entries),
                "entrySources": entry_source_counts(entries),
                "targets": TARGETS,
                "schema": catalog.get("schema"),
                "macosBaseline": catalog.get("macosBaseline"),
                "generator": catalog.get("generator"),
                "inputProvenance": {
                    "source": OFFICIAL_SOURCE,
                    "kind": pin.get("kind"),
                    "url": pin.get("url"),
                    "revision": pin.get("revision"),
                    "sha256": pin.get("sha256"),
                    "license": pin.get("license"),
                    "raw": pin.get("raw"),
                },
            },
            "analytics": {
                "path": analytics_path,
                "sha256": sha256_file(analytics_path),
                "sourceUrl": analytics_url,
                "retrievedAtUtc": retrieved,
                "dateRange": {
                    "start": analytics.get("start_date"),
                    "end": analytics.get("end_date"),
                },
                "category": analytics.get("category"),
                "totalItems": analytics.get("total_items"),
                "totalCount": analytics.get("total_count"),
                "retrievalNote": "url and retrieval time are caller-supplied provenance; script never fetches",
            },
        },
        "ranking": {
            "rule": "integer install count descending, token ascending as stable tie-break",
            "analyticsTokens": len(ranked),
            "topN": 1000,
            "boundaryCount": top[-1][1] if top else None,
            "boundaryTieTruncation": sum(
                1 for t, c in ranked if top and c == top[-1][1]
            )
            - sum(1 for _, c in top if c == top[-1][1]),
        },
        "top1000": {
            "matched": len(matched),
            "missing": len(missing),
            "missingTokens": missing,
            "note": "missing analytics tokens are reported, never replaced with lower-ranked tokens",
            **agg(by_token, counts, [t for t, _ in top]),
        },
        "wholeCatalog": agg(by_token, counts, None),
        "invariants": {
            "checked": [
                "catalog keys are qualified source/token identities that agree with each row's source and bare token fields and with the raw records both ways (keys and raw tokens are valid catalog tokens, charset, no '..'); unexpected sources and duplicate bare tokens are violations, never silent drops or overwrites",
                "the catalog inputs map contains exactly homebrew/cask; the official input has kind json-snapshot and an explicit raw null key",
                "targets present exactly aarch64-darwin+x86_64-linux",
                "catalog input provenance names the audited bytes: revision is 40 hex, url is http(s), and the official input sha256 equals the raw file hash",
                "eligible rows have plan+kind and null reason; excluded rows have reason and null plan/kind",
                "plan source urls absolute http(s), sha256 64-hex, archive kind known; artifact sources relative without traversal, targets single components without ASCII controls (pkg targets null by design)",
                "platform scope re-derived for the first decision stages (disabled/deprecated/supported_platforms/depends_on.linux) on the effective variation-merged record; a bounded check, not full classifier equivalence",
                "no duplicate plan-internal destinations",
            ],
            "violations": violations,
            "violationCount": len(violations),
            "crossTokenDestinationSharing": {
                "count": len(collisions),
                "examples": collisions,
                "note": "informational conflict, not an invariant failure: stable/beta variant casks legitimately write the same bundle name; serializing such installs is the native Nix runtime's concern. Plan-internal duplicates remain fatal",
            },
            "interpretation": "bounded structural metadata checks over the whole catalog, not successful installs and not proof of classifier equivalence",
        },
    }

    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "corpus-report.json"), "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=1)
    with open(
        os.path.join(out_dir, "top1000.csv"), "w", newline="", encoding="utf-8"
    ) as fh:
        w = csv.writer(fh, lineterminator="\n")
        w.writerow(
            [
                "rank",
                "token",
                "count",
                "in_catalog",
                "aarch64_darwin_status",
                "aarch64_darwin_kind",
                "aarch64_darwin_reason",
                "x86_64_linux_status",
                "x86_64_linux_kind",
                "x86_64_linux_reason",
            ]
        )
        for i, (token, count) in enumerate(top, 1):
            row = [i, token, count, "yes" if token in by_token else "no"]
            for system in TARGETS:
                st = by_token.get(token, {}).get("targets", {}).get(system, {})
                row += [st.get("status"), st.get("kind"), st.get("reason")]
            w.writerow(row)
    return report


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--input", default="/tmp/cask-audit/cask.json")
    ap.add_argument("--catalog", default="nix/casks/catalog/catalog.json")
    ap.add_argument("--analytics", default="/tmp/cask-audit/analytics-90d.json")
    ap.add_argument("--output", default="/tmp")
    ap.add_argument(
        "--analytics-url", default=None, help="recorded as provenance only; no fetch"
    )
    ap.add_argument(
        "--retrieved", default=None, help="ISO-8601 retrieval time, recorded only"
    )
    args = ap.parse_args()
    _selfcheck()
    report = audit(
        args.input,
        args.catalog,
        args.analytics,
        args.output,
        args.analytics_url,
        args.retrieved,
    )
    t = report["top1000"]
    print(
        f"top1000: matched={t['matched']} missing={t['missing']} "
        f"violations={report['invariants']['violationCount']}"
    )
    for system in TARGETS:
        print(f"  {system}: {t['perTarget'][system]['statuses']}")
    sys.exit(1 if report["invariants"]["violationCount"] else 0)


if __name__ == "__main__":
    main()
