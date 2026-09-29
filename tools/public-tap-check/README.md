# public-tap-check

Data-only fixture trees, an adversarial case matrix, and a metadata-only
matrix of seven real pinned public taps for the accepted local public-tap
import scope
([implementation contract](../../openspec/changes/import-public-cask-taps/implementation-contract.md),
[design](../../openspec/changes/import-public-cask-taps/design.md)).

This directory prepares verification assets. It is **not** an importer and
makes **no** integration claim. The preparation and static checks passed
(JSON syntax, deterministic generation, refusal bounds, lint, syntax-only
Ruby check, checker self-tests); see
`docs/verification/public-taps-2026-09-29/`. Execution status
(2026-09-29): the final normal-mode native reader and its gate ran on
both hosts over the 20-case fixture matrix and over all seven pinned
public taps; the final normal-mode whole import of all seven pins
passed on BOTH hosts (Linux: 30 records, 13 eligible / 17 excluded;
macOS: 30 records, 7 eligible / 23 excluded, first three sources before
the GitHub rate-limit reset and the last four after it; every row's
offline checker and derivation evaluation returned 0); the converter
capture regressions (7/7), the backend suite, the client unit suites,
the interactive consent gate, the actual product negative gates on both
hosts (C6/C7 Linux and macOS — the macOS record refused all three
unsafe local configs before any Ruby and failed closed on the
deliberately public daemon `TMPDIR=/tmp` with no flake, daemon restored,
vendor downloads 0), the actual native client
lifecycle, and the official catalog regression all passed. The
independent parent review is complete; only the implementation PR
stays open — see the evidence pages before
claiming it. Reader-stage success is never source acceptance
(for example, the duplicate-token fixture exports two rows at the
reader stage; the converter later rejects the duplicate identity).

## Contents

| File | Role |
| --- | --- |
| `matrix.json` | Data-only adversarial case matrix: one small tree per independent case, attack classes, explicit expected outcomes, bounds policy. |
| `public-sources.json` | Metadata-only matrix of seven real pinned public taps with expected inventory counts and expected selected statuses. |
| `make_fixtures.py` | Deterministic fixture writer (data only). Writes the per-case trees and a hash manifest. No denial probes or sentinels are generated; sandbox proofs run through the real reader gate and the parent's controlled native Nix probes. |
| `check_sources.py` | Offline actual-catalog checker: compares one real schema-3 catalog against the expected record in `public-sources.json`. No network, no imports. |

There is deliberately no parallel test framework here. Archive handling,
plan building, and catalog plan replay are covered by the existing
[`tools/cask-build-check`](../cask-build-check) suite; run it directly.

## Layout: one small tree per independent case

A source-wide failure (duplicate token) aborts its whole tree. So every
independent case lives in its own tree under `taps/`, and a whole-source
abort can hide no other case. The only token shared across trees is the
intentional collision pair `taps/collision-a` + `taps/collision-b`; the
duplicate pair `Casks/a/pubtap-dup.rb` + `Casks/b/pubtap-dup.rb` (same
basename, same declared token) lives alone in `taps/dup`. Tree names map
to synthetic source ids in `matrix.json` (`source_id`).

The `staging`, `staging-rescued`, `broken`, and `flights` trees each also
hold one valid positive-control cask (`pubtap-*-control`) with a reserved
host URL, a synthetic checksum, and plain binary artifacts. The actual
reader fails a whole export when every record in a tree fails, so each of
those trees must yield one excluded record plus one eligible control
record; both records are part of the expected inventory in `matrix.json`.
The `dup` tree intentionally stays alone (the duplicate token must fail
the whole source) and `flood` intentionally stays alone (fail-closed
resource exhaustion); neither gets a control. The collision pair keeps
its intentional token sharing unchanged.

## Safety contract

- **No third-party Ruby is executed by anything here.** Generated Ruby is
  first-party synthetic fixture text. It runs only later, under the real
  pinned reader, inside the sandboxed Nix derivation on a clean
  verification host without Pi/model credentials.
- **No real vendor downloads.** Fixture URLs use reserved hosts
  (`example.com`, `example.org`) or literal local addresses; checksums are
  synthetic constants. The public-source matrix is metadata-only; its
  payload download budget is 0.
- **Sentinels are not generated here.** Sandbox denial proofs run through
  the real reader gate and the parent's controlled native Nix probes, each
  with a positive control first. A missing canary or missing listener is a
  failure, never a pass. Never print environment variables, keys, or
  raw captured output; scan captured output programmatically.
- **The flood budget is fixed.** `pubtap-flood` emits exactly
  `output_exhaustion_emit_bytes` (2 MiB) baked in from policy. There is no
  environment override, so the fixture itself can never become unbounded.
  The runner must still cap captured output **while** the command runs.
- **Bound every external command.** Apply a timeout and cap captured bytes
  while the command runs, never after. Discarding an exit code is not
  allowed.

## Output directory contract

`make_fixtures.py --out DIR` accepts only an **absolute** path that is
fresh or empty. Canonical parent aliases (`/tmp`, `/var` on macOS) are
resolved first, so default `mktemp` targets work on macOS. The resolved
leaf must not be a symlink, and a nonempty or non-directory target is
refused with an error.

## One-off checks (safe on any host; no Ruby, no network)

```sh
python3 -m json.tool tools/public-tap-check/matrix.json > /dev/null
python3 -m json.tool tools/public-tap-check/public-sources.json > /dev/null
ruff format --check tools/public-tap-check
ruff check tools/public-tap-check
```

Determinism of the fixture writer (two runs must be byte-identical):

```sh
A=$(mktemp -d); B=$(mktemp -d)
python3 tools/public-tap-check/make_fixtures.py --out "$A/fx"
python3 tools/public-tap-check/make_fixtures.py --out "$B/fx"
diff -r "$A/fx" "$B/fx"
rm -rf "$A" "$B"
```

Helper-only invalidation invariant (cask bytes identical; only the helper
`VERSION` value differs, so the imported version/URL and the whole-tree
hash both differ):

```sh
A=$(mktemp -d)
python3 tools/public-tap-check/make_fixtures.py --out "$A/fx" > /dev/null
diff "$A/fx/taps/invalidation/alpha-v1/Casks/t/pubtap-invalidation.rb" \
     "$A/fx/taps/invalidation/alpha-v2/Casks/t/pubtap-invalidation.rb"
diff "$A/fx/taps/invalidation/alpha-v1/lib/pubtap_helper.rb" \
     "$A/fx/taps/invalidation/alpha-v2/lib/pubtap_helper.rb"
rm -rf "$A"
```

The first `diff` must print nothing (cask file identical). The second must
show exactly the `VERSION` value change (`1.4.0` → `1.4.1`). The cached
export must be invalidated by the whole-tree change even though the cask
bytes are equal; both variants must load cleanly, so the cache test cannot
pass merely because a variant failed the loader.

Existing first-party suite reuse (no wrapper, run it directly):

```sh
python3 tools/cask-build-check/plan_tests.py
```

## Offline actual-catalog checker

The parent runs the real importer first, then this checker against the
produced catalog (`flake_path/catalog/catalog.json`). It never imports,
never runs Ruby, and never touches the network:

```sh
python3 tools/public-tap-check/check_sources.py \
  --source owner/tap --system x86_64-linux --catalog catalog.json
```

It verifies the final raw-tap backend contract: `inputs[source].kind` is
`raw-ruby-tap`; `catalog.targets` is exactly `[system]` (a raw tap catalog
names only the imported native system; the ordinary official JSON catalog
keeps both targets and is out of scope here); every entry's targets map has
exactly that one native key; `inputs[source].url` is the canonical approved
repository URL and `inputs[source].raw.archiveUrl` is the exact codeload URL
for the pinned revision, checked independently and never assumed equal;
`raw.archiveSha256` and `raw.captureSha256` are recorded; the exact revision
pin matches; plus complete inventory count, qualified
`owner/tap/token` key/source/token agreement, an explicit native target
status for every row, and the selected source-derived status expectations
(`record-actual` entries are recorded, not enforced). Note: the `reason`
and `codes` strings in `public-sources.json` entries are descriptive
source notes for review; the checker enforces status, inventory, and
provenance, and records reasons in its JSON evidence — it does not
assert those code strings. It prints actual
statuses, reasons, and provenance as JSON evidence on stdout and exits 1 on
any problem. Structural syntax of the data files is covered by the
`json.tool` one-offs above.

### Checker tests (manufactured catalogs; not import proof)

These one-offs manufacture small negative catalogs and assert the checker
refuses them. They test the checker only. They prove nothing about imports;
the final normal-mode import matrix is tracked separately in the
evidence pages:

```sh
T=$(mktemp -d); S=goreleaser/tap
BASE='{
  "schema": "pkg-cask-catalog/3", "targets": ["x86_64-linux"],
  "inputs": {"goreleaser/tap": {
    "kind": "raw-ruby-tap",
    "url": "https://github.com/goreleaser/homebrew-tap",
    "revision": "77e442b6000587bf57e543081e95fd908b918bd7",
    "raw": {
      "archiveUrl": "https://codeload.github.com/goreleaser/homebrew-tap/tar.gz/77e442b6000587bf57e543081e95fd908b918bd7",
      "archiveSha256": "'$(printf 'a%.0s' $(seq 64))'",
      "captureSha256": "'$(printf 'b%.0s' $(seq 64))'"}}},
  "entries": {"goreleaser/tap/mcp": {
    "source": "goreleaser/tap", "token": "mcp",
    "origin": {"path": "Casks/mcp.rb"},
    "targets": {"x86_64-linux": {"status": "eligible"}}}}}'
python3 - "$T" "$S" "$BASE" <<'EOF'
import json, subprocess, sys, pathlib
out, source, base = sys.argv[1], sys.argv[2], sys.argv[3]
checker = "tools/public-tap-check/check_sources.py"
sysname = "x86_64-linux"

def run(cat):
    p = out + "/cat.json"
    pathlib.Path(p).write_text(json.dumps(cat))
    return subprocess.run([sys.executable, checker, "--source", source,
                           "--system", sysname, "--catalog", p],
                          capture_output=True).returncode

base = json.loads(base)
assert run(base) == 1, "partial inventory must fail"
full = dict(base)
full["entries"] = {f"goreleaser/tap/{t}": {
    "source": "goreleaser/tap", "token": t,
    "origin": {"path": f"Casks/{t}.rb"},
    "targets": {sysname: {"status": "eligible"}}}
    for t in ["goreleaser-pro", "goreleaser-pro@2", "goreleaser", "mcp",
              "mcp@0", "sponsors"]}
assert run(full) == 0, "complete manufactured catalog must pass the checker"
bad_kind = json.loads(json.dumps(full))
bad_kind["inputs"][source]["kind"] = "official-json"
assert run(bad_kind) == 1, "non-raw kind must fail"
bad_targets = json.loads(json.dumps(full))
bad_targets["targets"] = [sysname, "aarch64-darwin"]
assert run(bad_targets) == 1, "raw catalog with two targets must fail"
bad_url = json.loads(json.dumps(full))
bad_url["inputs"][source]["url"] = bad_url["inputs"][source]["raw"]["archiveUrl"]
assert run(bad_url) == 1, "url == archiveUrl must fail the canonical URL check"
print("checker tests OK")
EOF
rm -rf "$T"
```

All inputs above are manufactured strings; no catalog came from a real
import, and the pass line above is a checker result, not an import result.

## Sandbox denial proofs

No denial probe is generated here. The old optional generated Ruby probe
and the synthetic sentinels are removed: they duplicated the parent's
controlled native Nix probes and the backend's real reader-gate probe, and
a probe whose controls are absent cannot prove denial. The required proofs
are:

- the backend's reader-gate probes: a positive-control canary and a
  positively controlled live host-loopback listener are established
  first, outside the sandbox, on the same host; the same operations are
  then attempted through the real reader gate. Every probe must report
  denied. A missing canary or a missing listener is a failure, never a
  pass;
- the parent's controlled native Nix probes (see the verification pages).

## Redirect and DNS policy

Fixtures cover **literal** local/private destinations only
(`localhost`, `192.168.1.9`, `169.254.169.254`, `[::1]`): the destination
policy must refuse them without a lookup. No fixture can prove redirect
handling or DNS-based redirects; that policy is verified by backend tests.
The expectation is documented in `matrix.json`, not faked here.

## Real importer interface (agreed)

The agreed seam is `cask_catalog::tap::{ImportRequest, import}` with the
CLI mirror `cask-catalog import-tap --source owner/tap --revision
<40-hex> --system <system> --nix <abs-nix> --out <abs-staging>`. On
success, stdout is `ImportResult` JSON containing `flake_path`; the
catalog is at `flake_path/catalog/catalog.json`. There is **no**
fixture-runner interface and no production local-source bypass; do not
invent one. The parent runs the real imports in a clean sandbox without
model credentials.

Bounded-run pattern for any importer command (bash; capture is capped
while the command runs, and the exit code is recorded, never discarded):

```sh
timeout 300 env -i PATH=/usr/bin:/bin HOME=/nonexistent \
  cask-catalog import-tap --source OWNER/TAP --revision REVISION \
  --system SYSTEM --nix /abs/nix --out /abs/staging 2>&1 \
  | head -c 65536 > "$LOG"
status=${PIPESTATUS[0]}
printf 'exit=%s bytes=%s\n' "$status" "$(wc -c < "$LOG")"
```

Record `status`, byte count, host, and Nix version per run.

## Generated layout

```text
out/
  matrix-manifest.json          # per-file hashes + case coverage (deterministic)
  harness/policy.json           # generator/matrix versions + bounds
  taps/
    release-archive/ raw-binary/ app-bundle/ branches/   # supported subset
    staging/ staging-rescued/ flights/ steps/ installer/ # conversion limits
    dup/ broken/ flood/                                  # source failure, load error, flood
    url-localhost/ url-private/ url-linklocal/ url-ipv6/ # literal destinations
    collision-a/ collision-b/                            # intentional two-source pair
    invalidation/alpha-v1, -v2                            # helper-only commit variants
```

Expected results per case live in `matrix.json`; expected inventory and
selected statuses for the real taps live in `public-sources.json`. Record
actual outcomes in
[docs/verification/public-taps-2026-09-29/results.md](../../docs/verification/public-taps-2026-09-29/results.md),
replacing each `UNRUN` marker. Evidence instructions are in that
directory's [README.md](../../docs/verification/public-taps-2026-09-29/README.md).
