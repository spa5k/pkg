# public-tap-check

Data-only fixture trees, an adversarial case matrix, and a metadata-only matrix of seven real pinned public taps for the accepted local public-tap import scope ([implementation contract](../../openspec/changes/import-public-cask-taps/implementation-contract.md), [design](../../openspec/changes/import-public-cask-taps/design.md)). Execution status and limits are summarized in [docs/verification/public-taps.md](../../docs/verification/public-taps.md).

This directory prepares verification assets. It is **not** an importer and makes **no** integration claim. Reader-stage success is never source acceptance (for example, the duplicate-token fixture exports two rows at the reader stage; the converter later rejects the duplicate identity).

## Contents

| File | Role |
| --- | --- |
| `matrix.json` | Data-only adversarial case matrix: one small tree per independent case, attack classes, explicit expected outcomes, bounds policy. |
| `public-sources.json` | Metadata-only matrix of seven real pinned public taps with expected inventory counts and expected selected statuses. |
| `make_fixtures.py` | Deterministic fixture writer (data only). Writes the per-case trees and a hash manifest. Generates no denial probes or sentinels. |
| `check_sources.py` | Offline actual-catalog checker: compares one real schema-3 catalog against the expected record in `public-sources.json`. No network, no imports. |

There is deliberately no parallel test framework here. Archive handling, plan building, and catalog plan replay are covered by the existing [`tools/cask-build-check`](../cask-build-check) suite; run it directly.

## Layout: one small tree per independent case

A source-wide failure (duplicate token) aborts its whole tree, so every independent case lives in its own tree under `taps/`. The only token shared across trees is the intentional collision pair `taps/collision-a` + `taps/collision-b`; the duplicate pair lives alone in `taps/dup` (whole-source failure) and `flood` stays alone (fail-closed resource exhaustion). The `staging`, `staging-rescued`, `broken`, and `flights` trees each also hold one valid positive-control cask, because the reader fails a whole export when every record in a tree fails; both records are part of the expected inventory in `matrix.json`.

## Safety contract

- **No third-party Ruby is executed by anything here.** Generated Ruby is first-party synthetic fixture text. It runs only under the real pinned reader, inside the sandboxed Nix derivation on a clean verification host without Pi/model credentials.
- **No real vendor downloads.** Fixture URLs use reserved hosts (`example.com`, `example.org`) or literal local addresses; checksums are synthetic constants. The public-source matrix is metadata-only; its payload download budget is 0.
- **Sentinels are not generated here.** Sandbox denial proofs run through the real reader gate and the parent's controlled native Nix probes, each with a positive control first. A missing canary or missing listener is a failure, never a pass.
- **The flood budget is fixed.** `pubtap-flood` emits exactly 2 MiB baked in from policy; there is no environment override. The runner must still cap captured output **while** the command runs.
- **Bound every external command.** Apply a timeout and cap captured bytes while the command runs, never after. Discarding an exit code is not allowed.

## Output directory contract

`make_fixtures.py --out DIR` accepts only an **absolute** path that is fresh or empty. Canonical parent aliases (`/tmp`, `/var` on macOS) are resolved first. The resolved leaf must not be a symlink; a nonempty or non-directory target is refused.

## One-off checks (safe on any host; no Ruby, no network)

```sh
python3 -m json.tool tools/public-tap-check/matrix.json > /dev/null
python3 -m json.tool tools/public-tap-check/public-sources.json > /dev/null
ruff format --check tools/public-tap-check
ruff check tools/public-tap-check
```

Determinism (two runs must be byte-identical):

```sh
A=$(mktemp -d); B=$(mktemp -d)
python3 tools/public-tap-check/make_fixtures.py --out "$A/fx"
python3 tools/public-tap-check/make_fixtures.py --out "$B/fx"
diff -r "$A/fx" "$B/fx"; rm -rf "$A" "$B"
```

Helper-only invalidation invariant (cask bytes identical; only the helper `VERSION` differs, so the whole-tree hash differs):

```sh
A=$(mktemp -d)
python3 tools/public-tap-check/make_fixtures.py --out "$A/fx" > /dev/null
diff "$A/fx/taps/invalidation/alpha-v1/Casks/t/pubtap-invalidation.rb" \
     "$A/fx/taps/invalidation/alpha-v2/Casks/t/pubtap-invalidation.rb"
diff "$A/fx/taps/invalidation/alpha-v1/lib/pubtap_helper.rb" \
     "$A/fx/taps/invalidation/alpha-v2/lib/pubtap_helper.rb"
rm -rf "$A"
```

Existing first-party suite reuse (no wrapper): `python3 tools/cask-build-check/plan_tests.py`.

## Offline actual-catalog checker

Run the real importer first, then this checker against the produced catalog. It never imports, never runs Ruby, and never touches the network:

```sh
python3 tools/public-tap-check/check_sources.py \
  --source owner/tap --system x86_64-linux --catalog catalog.json
```

It verifies the backend contract: `inputs[source].kind` is `raw-ruby-tap`; `catalog.targets` is exactly `[system]`; `inputs[source].url` is the canonical approved repository URL and `raw.archiveUrl` the exact codeload URL for the pinned revision, checked independently; `archiveSha256` and `captureSha256` recorded; the revision pin matches; plus inventory counts, qualified key/source/token agreement, native target status per row, and the selected status expectations. The `reason` and `codes` strings in `public-sources.json` are descriptive source notes; the checker enforces status, inventory, and provenance, and exits 1 on any problem. Checker self-tests with manufactured negative catalogs are preserved at the immutable baseline commit <https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/tools/public-tap-check/README.md>.

## Sandbox denial and redirect policy

No denial probe is generated here. The required proofs are the backend's reader-gate probes (positive-control canary and positively controlled live loopback first, then denied through the gate; a missing control is a failure) and the parent's controlled native Nix probes. Fixtures cover **literal** local/private destinations only (`localhost`, `192.168.1.9`, `169.254.169.254`, `[::1]`); redirect and DNS policy is verified by backend tests and documented in `matrix.json`, not faked here.

## Real importer interface (agreed)

The seam is `cask_catalog::tap::{ImportRequest, import}` with the CLI mirror `cask-catalog import-tap --source owner/tap --revision <40-hex> --system <system> --nix <abs-nix> --out <abs-staging>`. There is no fixture-runner interface and no production local-source bypass; do not invent one. Bounded-run pattern (capture capped while running; exit code recorded, never discarded):

```sh
timeout 300 env -i PATH=/usr/bin:/bin HOME=/nonexistent \
  cask-catalog import-tap --source OWNER/TAP --revision REVISION \
  --system SYSTEM --nix /abs/nix --out /abs/staging 2>&1 \
  | head -c 65536 > "$LOG"
status=${PIPESTATUS[0]}
printf 'exit=%s bytes=%s\n' "$status" "$(wc -c < "$LOG")"
```

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

Expected results per case live in `matrix.json`; expected inventory and selected statuses for the real taps live in `public-sources.json`.
