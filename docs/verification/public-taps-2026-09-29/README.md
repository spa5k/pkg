# Public-tap import verification — 2026-09-29

Status: **implementation and local verification complete**. Delivered in
[PR #82](https://github.com/spa5k/pkg/pull/82). Proven: the static preparation checks (see
[`static-checks.log`](static-checks.log) and
[`python-lint.log`](python-lint.log)); the actual native raw reader and
its isolation gate over all seven pinned taps on both hosts
([results.md section R](results.md#r-actual-native-raw-reader-and-isolation-gate-evidence--public-taps));
the final normal-mode reader over the 20-case fixture matrix per OS
([section R2](results.md#r2-final-normal-mode-native-reader--fixture-matrix-2026-09-29));
per-cask deadline kill ([section DL](results.md#dl-per-cask-deadline-kill-actual-both-hosts));
credential sentinel containment
([section CRED](results.md#cred-credential-sentinel-containment-actual-both-hosts));
the Linux product negative gate (C6/C7 Linux: `sandbox = false`,
relaxed, and fallback refused before any Ruby; the strict config under
an unsandboxed daemon hit the behavioral probe, refused, and published
no flake); the macOS product negative gate (C6/C7 macOS: the strict
local config refused `sandbox = false`, relaxed, and fallback before
any Ruby; the deliberately public daemon `TMPDIR=/tmp` caused the
behavioral reader probe to report FAIL — host read ALLOWED — and the
import was refused before export with no flake, the daemon restored,
and vendor downloads 0 — [`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl),
harness [`negative-product-gate-macos.py.txt`](negative-product-gate-macos.py.txt);
harness setup failures are not product results); the final normal-mode whole import of all seven pinned
sources on BOTH hosts with offline checker and derivation evaluation
(E1: Linux 13 eligible / 17 excluded; macOS 7 eligible / 23 excluded,
first three sources before the GitHub rate-limit reset and the last
four after it); the converter
capture regressions and the backend suite (section B); the actual
interactive consent input gate on both hosts (section CONS); the actual
native client lifecycle on both hosts
([section PKG](results.md#pkg-actual-native-client-lifecycle-both-hosts-real-pkg-client));
the client unit suites on both hosts (section CLT); the official catalog
final regression and the evaluation of every eligible official derivation
([section OR](results.md#or-official-catalog-final-regression-metadata-and-synthetic-only),
[DRV](results.md#drv-final-evaluation-of-all-eligible-official-derivations));
and the final client app exposure check
([section APP](results.md#app-client-app-exposure-check-final-targeted));
the final CI archives, the post-format static lint, and the post-format
official derivation re-evaluation (table F, DRV). The parent has
completed the read-only code and integration review, and all requested
fixes are done; no further production changes are pending. The implementation is in [PR #82](https://github.com/spa5k/pkg/pull/82). Full GUI launches and app
runtime behavior are out of scope.
One parent-measured section exists as background: the Nix-only sandbox
evidence in section C0. The `cask_catalog::tap` interface is agreed (see
the [design](../../../openspec/changes/import-public-cask-taps/design.md));
the parent runs the real imports in a clean sandbox without model
credentials.

Scope: the accepted local public-tap import contract
([implementation contract](../../../openspec/changes/import-public-cask-taps/implementation-contract.md),
[design](../../../openspec/changes/import-public-cask-taps/design.md),
[case matrix](../../../tools/public-tap-check/matrix.json),
[public-source matrix](../../../tools/public-tap-check/public-sources.json)).

## Hard rules for every run

1. Raw tap Ruby runs only in a clean verification sandbox **without Pi or
   model credentials**. Never execute it from a worker harness that holds
   model keys.
2. Never print environment variables, key material, or raw captured
   importer output. Scan captured bytes programmatically.
3. Treat importer output as hostile: cap it **while the command runs**
   (`| head -c 65536 > log`), record the exit code (`PIPESTATUS`), and
   never discard a failure with `|| true`.
4. Bound every external command with `timeout` (matrix policy: 300 s) and
   a byte cap (65536).
5. No real vendor downloads in fixture runs. The public-source matrix is
   metadata-only; its payload budget is 0. Any later native sample is a
   separate, explicitly bounded parent step.
6. Record commands, exit codes, host and Nix version, input hashes, and
   limits. Keep `UNRUN` for anything not actually executed. An expected
   exclusion is not a tested result.

## Preconditions to record per host

- Host: real native `aarch64-darwin` or `x86_64-linux` (sandbox proof must
  run on both; a Linux run never proves macOS policy).
- Nix version and effective daemon isolation, measured by behavior probes
  (canary file, host paths, loopback network), not by reading config.
- Confirmation that the environment carries no Pi/model credentials
  (state the check method, not the environment contents).
- Tools: `python3`, `cargo`, `nix`, `bsdtar`, `7zz`, `ruff`.

## Verified macOS daemon requirements (parent evidence, 2026-09-29)

The parent's Nix sandbox evidence is actual, not an importer test. On
Linux, an ordinary strict build denied all three probes: a world-readable
host-canary read, a world-writable outside-directory write, and a
positively controlled live host-loopback connect. On macOS, the daemon as
shipped ran with `sandbox = false`; an ordinary user's `--option sandbox
true` was ignored; all three probes were allowed (negative proof). The
final disposable-VM proof denied all three **only** after all of:

1. daemon `sandbox = true`;
2. `sandbox-fallback = false`;
3. a minimal `sandbox-paths` set;
4. the reader derivation sets `__darwinAllowLocalNetworking = false`;
5. the LaunchDaemon `EnvironmentVariables` gives builds a private
   `TMPDIR` (`/nix/var/nix/builds`).

The default Darwin global tmp is granted to builds and can expose `/tmp`
file reads and writes. Nested `sandbox-exec` failed and is not a solution.

These settings are documented prerequisites, applied by the person who
owns the host. The product never edits host daemon configuration
automatically and never adds root-equivalent `trusted-users` entries. It
fails closed and explains the prerequisites when behavioral probes show
insufficient denial, and it re-probes with a fresh nonce and fresh paths
before **every** raw import, so a cached earlier success can never mask a
changed daemon. The denial also holds outside `/tmp`: an outside
`/Users/Shared` 0777 directory plus a 0644 canary was denied on the final
strict macOS config. A percent-encoded catalog path with spaces preserved
the original URL through install/upgrade/rollback on both systems; raw
unencoded path references failed.

The exact raw logs are copied in this directory:
[`sandbox-linux.log`](sandbox-linux.log),
[`sandbox-macos-negative.log`](sandbox-macos-negative.log),
[`sandbox-macos-positive.log`](sandbox-macos-positive.log),
[`sandbox-macos-shared-path.log`](sandbox-macos-shared-path.log),
[`lifecycle-linux.log`](lifecycle-linux.log),
[`lifecycle-macos.log`](lifecycle-macos.log),
[`lifecycle-linux-encoded.log`](lifecycle-linux-encoded.log), and
[`lifecycle-macos-encoded.log`](lifecycle-macos-encoded.log). The
parent probe source is saved unmodified as
[`native-sandbox-probe.py.txt`](native-sandbox-probe.py.txt)
(no new runtime script). The copied lifecycle logs cover the real
directory lifecycle only; they contain no symlink-rejection observation.
Earlier `/var/folders` denial
observations were invalid: directory permissions denied them outside the
sandbox too. Nix-only observations are kept separate from product tests;
the final product stages that ran are recorded in the results page
(sections B, C, CONS, CLT, PKG, E).

## Steps

### A. Static asset checks (safe on any host; one-off commands)

```sh
python3 -m json.tool tools/public-tap-check/matrix.json > /dev/null
python3 -m json.tool tools/public-tap-check/public-sources.json > /dev/null
ruff format --check tools/public-tap-check
ruff check tools/public-tap-check
A=$(mktemp -d); B=$(mktemp -d)
python3 tools/public-tap-check/make_fixtures.py --out "$A/fx" > /dev/null
python3 tools/public-tap-check/make_fixtures.py --out "$B/fx" > /dev/null
diff -r "$A/fx" "$B/fx"
rm -rf "$A" "$B"
```

### B. Fixture generation for a sandbox run (deterministic)

```sh
FIX="$(mktemp -d)/fixtures"
python3 tools/public-tap-check/make_fixtures.py --out "$FIX"
```

The output directory must be absolute and fresh or empty; the writer
resolves canonical parent aliases (`/tmp`, `/var` on macOS) first and
refuses a leaf symlink or a nonempty directory. Record the
`matrix-manifest.json` hash. Regeneration must reproduce it byte-for-byte.
Each independent case lives in its own small tree, so a whole-source abort
cannot hide other cases; the only shared-token trees are the intentional
collision pair. The `staging`, `staging-rescued`, `broken`, and `flights`
trees each hold one valid positive-control cask (`pubtap-*-control`): the
actual reader fails a whole export when every record in a tree fails, so
those imports must emit the excluded record plus one eligible control
record. The `dup` and `flood` trees intentionally stay without controls
(whole-source duplicate rejection and fail-closed resource exhaustion).
No denial probes or sentinels are generated; sandbox
proofs run through the real reader gate (see section D).

### C. Importer execution (real interface; final stages recorded)

Per matrix case, in the clean sandbox, with timeout and capture bounds
applied, using the agreed CLI (`cask-catalog import-tap --source owner/tap
--revision <40hex> --system <system> --nix <abs-nix> --out <abs-staging>`;
on success stdout is `ImportResult` JSON with `flake_path`, catalog at
`flake_path/catalog/catalog.json`). There is no fixture-runner interface
and no production local-source bypass; how fixtures become importable
sources in the sandbox is the parent's integration concern. For each case
record: actual outcome vs the matrix expectation, exit code,
captured-byte count, capture bound (`output-exhaustion`), and
identity/collision results (`cross-tap-collision`, `duplicate-token`,
`helper-only-invalidation`).

Raw tap catalogs name `targets = [<native system>]` only; the ordinary
official JSON catalog keeps both targets. The offline checker enforces
this contract (see section G).

### D. Strict sandbox proof (real native hosts, before any safety claim)

- C1 network denial: through the real reader gate. A positively
  controlled live host-loopback listener is established first, outside the
  sandbox on the same host (literal address; no DNS name, so no resolution
  failure can masquerade as denial), then the same connect is attempted
  through the gate and must be denied. A missing listener is a failure,
  never a pass.
- C2 host-home denial: a positive-control canary is placed in a fake host
  home first and verified readable outside the gate; reads through the
  gate must be denied. A missing canary is a failure, never a pass.
- C3 credential canary: a supplied canary file is verified readable
  outside the gate; through the gate it must be unreadable and its bytes
  must never appear in captured output. A missing canary is a failure,
  never a pass.
- C4 output exhaustion: capture ≤ 64 KiB despite the fixed 2 MiB flood; no
  hang; no successful empty catalog on failure.
- C5 helper-only invalidation: v1/v2 trees re-export; both load cleanly;
  only the helper `VERSION` value differs; cache invalidates on the
  whole-tree hash while the cask hash is identical.
- C6 sandbox fallback refused: relaxed mode and added host paths rejected.
  Final status: Linux product refusals passed (`sandbox-fallback = true`,
  `sandbox = false`, relaxed, and fallback local configs, all before any
  Ruby); backend config-gate unit coverage passed on both OS; the final
  macOS product gate passed — the strict local config refused `sandbox =
  false`, relaxed, and fallback before any Ruby
  ([`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl)).
- C7 product fail-closed gate: Linux PASS (strict local config under a
  `sandbox = false` daemon hit the behavioral probe, refused before
  export, published no flake, daemon restored). macOS PASS: with the
  daemon deliberately public (`TMPDIR=/tmp`) despite the strict local
  config, the behavioral reader probe reported FAIL (host read
  ALLOWED), the import was refused before the export phase, no flake
  was published, the daemon was restored, and vendor downloads were 0
  ([`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl),
  harness [`negative-product-gate-macos.py.txt`](negative-product-gate-macos.py.txt));
  harness setup failures are not product results.

Every denial proof above runs through the real reader gate or the parent's
controlled native Nix probes. No synthetic denial fixture exists; a probe
with no positive control is discarded, not passed.

### E. Client behavior checks (final stages recorded)

- Consent refusal runs no Ruby and changes nothing.
- Noninteractive `tap add` without `--trust` stops.
- Search over saved catalogs never prompts and never runs Ruby.
- Bare tokens with collisions (including excluded matches) stay ambiguous;
  qualified IDs resolve only within their source.
- An unreadable, malformed, or corrupt saved catalog makes search and
  bare-token resolution fail with the failed source reported, never
  ignored, even when another saved source has the same token; native
  list/remove/history/rollback still work; `tap update SOURCE` can repair
  the catalog using the registry only.
- `tap remove` blocks future use, keeps installed packages. After a
  removal, `pkg upgrade --all` filters removed-source references and
  forwards the remaining entries by explicit native entry IDs (not native
  `--all`); an explicit upgrade of a removed-source entry refuses.
- Failed refresh preserves previous catalog bytes; native update/rollback
  preserves old generations.

### F. Reuse of existing suites (run directly, no wrapper)

- `python3 tools/cask-build-check/plan_tests.py` (helper/plan regressions).
- `tools/cask-build-check/catalog_plans.py --catalog <schema-3 catalog>`
  once schema-3 output exists (synthetic payload replay).

### G. Public-source matrix validation (parent-run; final passes recorded on both hosts)

- Structure: `python3 -m json.tool
  tools/public-tap-check/public-sources.json > /dev/null`.
- After the parent runs the real importer for a source, check the actual
  schema-3 catalog offline:
  `python3 tools/public-tap-check/check_sources.py --source owner/tap
  --system SYSTEM --catalog catalog.json`. It verifies the final raw-tap
  contract — `kind` `raw-ruby-tap`, `catalog.targets` exactly `[system]`,
  per-row target keys exactly native, canonical repo `url`, exact codeload
  `raw.archiveUrl` with the pinned revision, recorded `raw.archiveSha256`
  and `raw.captureSha256`, exact revision pin — plus the complete inventory
  count, qualified identity relationships, explicit native target status
  per row, and selected status expectations, and emits JSON evidence. No
  network, no imports. Simple manufactured negative-catalog checker tests
  are documented in `tools/public-tap-check/README.md`; they are checker
  tests, not import proof. The pinned inventory was verified by the parent
  on 2026-09-29 via GitHub APIs; the checker keeps that comparison offline
  from now on.
- The seven pinned taps, their revisions, expected counts, licenses, and
  expected selected statuses are recorded in
  [`public-sources.json`](../../../tools/public-tap-check/public-sources.json).
  Every `record-actual` entry must get its real outcome recorded when the
  parent runs the import. Do not force eligibility.

### H. Lint, OpenSpec, CI

- `cargo fmt`/`clippy` (changed files), Ruby and Nix lint as applicable,
  `ruff check tools/public-tap-check`, workflow lint.
- Strict OpenSpec validation. The `openspec` CLI was unavailable in the
  preparation sandbox; structural checks of the change files ran as one-off
  commands instead.
- Docs and static HTML checks; normal CI.

## Recording results

Fill [results.md](results.md): replace every `UNRUN` with the actual
outcome, command, exit code, evidence path, and limits. Parent-measured
observations are recorded only in their labeled section. Do not merge or
release without a later instruction.
