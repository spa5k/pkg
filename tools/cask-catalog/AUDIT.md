# cask-catalog audit tool

`audit.py` is a stdlib-only, offline, rerunnable maintainer audit. It never
downloads anything; every input is an explicit local path.

## Usage

One command, no inline comments (they would swallow the continuation
backslashes):

```
python3 tools/cask-catalog/audit.py \
  --input /tmp/cask-audit/cask.json \
  --catalog nix/casks/catalog/catalog.json \
  --analytics /tmp/cask-audit/analytics-90d.json \
  --output /tmp \
  --analytics-url https://formulae.brew.sh/api/analytics/cask-install/homebrew-cask/90d.json \
  --retrieved 2026-09-28T18:27:51Z
```

The three data paths above are optional arguments with exactly these
defaults (verified in the `argparse` parser; none is required): a pinned raw
brew-api snapshot, the generated catalog, and a local copy of the 90d
cask-install analytics. `--analytics-url`/`--retrieved` are provenance fields recorded
verbatim in the report; the script performs no network access. Fetch each
snapshot yourself and pin it (sha256, date range, retrieval time, source URL)
before running.

## Semantics

- Ranking: per-token integer install counts (comma-stripped, array rows
  summed), sorted count-descending with token-ascending stable tie-break.
  Top-1000 truncates the ranking exactly; missing tokens are reported, never
  replaced by lower-ranked ones. Negative or non-numeric counts are rejected
  as invalid input (exit 2), never silently used.
- Structural invariants run over **all** catalog rows (7709), not just the
  top1000: token key/field/raw agreement both ways with full token-key
  validity (charset, no `..`), exact target set, provenance agreement (the
  catalog's `input.sha256` must equal the raw file's actual bytes; revision
  40-hex, http(s) URL), eligible⇔plan+kind, excluded⇔reason with null
  plan/kind, absolute http(s) source urls with 64-hex checksums, relative
  non-traversing artifact sources and single-component ASCII-control-free
  targets (pkg `target` is null by design), a bounded platform-scope
  re-derivation (disabled/deprecated/`supported_platforms`/`depends_on.linux`
  on the effective variation-merged record), and no plan-internal duplicate
  destinations. The platform check is a bounded re-derivation of the first
  decision stages, not full classifier equivalence.
- Cross-token shared destinations (stable/beta variants writing the same
  bundle name) are reported informationally. They are not a catalog invariant
  failure; deciding how such installs are coordinated (for example, refusing
  or serializing them) belongs to the native Nix runtime, which this tool
  does not model.
- Whole-catalog popularity weighting uses actual analytics counts for every
  token that appears in the pinned analytics; tokens absent from analytics
  contribute zero and the count of matched tokens is reported, so a zero
  never masquerades as "no data".
- Exit codes: `0` clean, `1` invariant violations, `2` invalid input.
  Documented exclusion reasons (`unsupported-platform`, `missing-checksum`,
  `deprecated`, ...) are outcomes to aggregate, not failures.

## Provenance note

This audit is a **metadata** audit. No cask was downloaded, unpacked,
installed, or executed; "eligible" rows were never executed — pkg plans are
recorded, never run. The audit validates structural invariants and popularity
coverage of the pinned data, not successful installs and not full classifier
equivalence. Inline `_selfcheck` assertions (ranking aggregation, tie-breaks,
truncation, count rejection, variation merge, platform gate, token and
artifact-name validation) run on every invocation — no test framework.

## Fixtures

[`fixtures/upstream/records.json`](fixtures/upstream/records.json) holds six
small data-driven records adapted from Homebrew/brew test fixtures (revision
`cc9ff034b1d98cdd4061f68cf715bb967c483a8f`, BSD-2-Clause; the full upstream
[`LICENSE.txt`](fixtures/upstream/LICENSE.txt) is copied next to them). The
adaptation into our raw input shape: `file://#{TEST_FIXTURE_DIR}/...` URLs
become synthetic `https://fixtures.invalid/...` URLs (nothing downloads; the
generator refuses non-http(s) URLs by design), source artifact names and the
declared relative rename intent are preserved (`{"target": "..."}` item
options inside the artifact list, never absolute paths), `depends_on arch`
uses the brew-api `{"type": "arm", "bits": 64}` shape, and the pkg fixture's
entry-level `allow_untrusted` side key stays where the API puts it — the plan
builder treats that one inert side key as ignorable. This is not a claim that
unsupported .pkg options are handled: active item options still refuse (an
installer `choices` object excludes as `installer-script`; unknown option
keys or a pkg rename are `malformed-record`), and only the entry-level side
keys `target` and `allow_untrusted` are inert. One Linux AppImage record and
one whole-array variation replacement record are included; a missing
`variations` entry is valid (the base record serves that target) while
`supported_platforms` is required.
[`expectations.json`](fixtures/upstream/expectations.json) was derived
manually from the classify/plan/effective rules before the generator ran,
then compared against the actual `cargo run generate` output over a temporary
pin carrying the fixture file's exact sha256; `tests/upstream.rs` now re-runs
the real `classify_target` over every record on both targets inside normal
CI (`cargo test -p cask-catalog --test upstream`), so the adapted Homebrew
cases are exercised continuously instead of sitting as unused data. Upstream
behaviors we do not implement (old_tokens migration, `--no-binaries` plan
flags, `conflicts_with` serialization, executing pkg installers) are recorded
as out-of-scope/refused in the audit notes, not claimed as supported.
