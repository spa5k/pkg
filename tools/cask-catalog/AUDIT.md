# cask-catalog audit tool

`audit.py` checks local snapshots with the Python standard library.
It makes no network requests.

## Usage

Run this command without inline comments. Comments would break the line continuations.

```
python3 tools/cask-catalog/audit.py \
  --input /tmp/cask-audit/cask.json \
  --catalog nix/casks/catalog/catalog.json \
  --analytics /tmp/cask-audit/analytics-90d.json \
  --output /tmp \
  --analytics-url https://formulae.brew.sh/api/analytics/cask-install/homebrew-cask/90d.json \
  --retrieved 2026-09-28T18:27:51Z
```

The optional `--input`, `--catalog`, and `--analytics` arguments default to
the paths shown above. They name a raw Cask API payload, the generated
catalog, and 90-day cask-install analytics.
The report records `--analytics-url` and `--retrieved` as supplied.
Download each snapshot before the audit.
Record its SHA-256, date range, retrieval time, and source URL.

## Input checks

The catalog must declare schema `pkg-cask-catalog/4` and an `inputs` map.
`inputs["homebrew/cask"]` must have kind `json-snapshot` and null `raw`.
Its `sha256` must match the raw input file's bytes.
Invalid input exits with code 2.

Entry keys must match each row's `source/token` identity.
The audit reports sources other than `homebrew/cask` as invariant violations.
Duplicate bare tokens also fail this check.
It then uses bare tokens to join analytics and compare the raw snapshot with the catalog.
Both inputs must contain the same token set.

## Ranking

The audit removes commas from integer install counts and sums array rows per token.
It sorts counts in descending order, then tokens in ascending order for ties.
The top 1,000 report keeps that ranking.
It reports missing tokens without substituting lower-ranked tokens.
Negative or nonnumeric counts are invalid input.

Popularity weighting uses each matched token's analytics count across the whole catalog.
Tokens absent from analytics contribute zero.
The report includes the matched-token count.

## Catalog checks

The audit checks every catalog row, including rows outside the top 1,000:

- Qualified keys, sources, and tokens must agree. Tokens must use the allowed
  characters and contain no `..` component.
- The official catalog must declare both targets. Single-target tap catalogs
  are outside this audit's scope.
- Provenance must have a 40-hex revision and an HTTP or HTTPS URL.
  The official input hash must match the raw file's bytes.
- Eligible entries must have a plan and kind. Excluded entries must have a
  reason and null plan and kind.
- Plan source URLs must be absolute HTTP or HTTPS URLs with 64-hex checksums.
- Artifact sources must be relative and contain no traversal. Targets must
  have one component and no ASCII control characters. A pkg artifact's target is null.
- Plans must have no duplicate destinations.
- Platform checks use `disabled`, `deprecated`, `supported_platforms`, and
  `depends_on.linux` after merging the target variation.

The platform check covers only the first classifier decisions.
It does not reproduce the full classifier.
The report lists destinations shared by different tokens as information.
For example, stable and beta variants can use the same bundle name.
The native Nix runtime handles install conflicts.

Exit code 0 means no violations. Code 1 means invariant violations.
Code 2 means invalid input.
The report counts exclusions such as `unsupported-platform`, `missing-checksum`,
and `deprecated` as catalog outcomes.

## Provenance note

The audit checks metadata and popularity coverage of pinned data.
It does not download or install casks, execute plans, or prove that eligible
packages install successfully.
`_selfcheck` assertions run on every invocation.
They cover ranking, invalid counts, variation merges, platform checks, token
and artifact names, and schema-4 identity checks.

## Fixtures

[`fixtures/upstream/records.json`](fixtures/upstream/records.json) holds six
small data-driven records adapted from Homebrew/brew test fixtures (revision
`cc9ff034b1d98cdd4061f68cf715bb967c483a8f`, BSD-2-Clause; the full upstream
[`LICENSE.txt`](fixtures/upstream/LICENSE.txt) is copied next to them). The
adaptation makes these changes:

- Replace `file://#{TEST_FIXTURE_DIR}/...` with synthetic
  `https://fixtures.invalid/...` URLs. Tests do not download them.
- Keep artifact source names and relative renames as `{"target": "..."}`
  options in the artifact list.
- Use `{"type": "arm", "bits": 64}` for `depends_on arch`.
- Keep the pkg fixture's entry-level `allow_untrusted` key. The plan builder
  ignores that key and entry-level `target`.

Active item options still cause exclusions.
Installer `choices` use `installer-script`.
Unknown option keys and pkg renames use `malformed-record`.
The fixtures include a Linux AppImage and a whole-array variation replacement.
A missing variation uses the base record. `supported_platforms` is required.

[`expectations.json`](fixtures/upstream/expectations.json) was derived
manually from the classify/plan/effective rules before the generator ran,
then compared against `cargo run generate` output with a temporary pin for
the fixture file's SHA-256.
`tests/upstream.rs` runs `classify_target` for every record on both targets in CI.
Run it with `cargo test -p cask-catalog --test upstream`.
The audit notes record unsupported behavior, including `old_tokens` migration,
`--no-binaries` plan flags, `conflicts_with` serialization, and pkg installer execution.
