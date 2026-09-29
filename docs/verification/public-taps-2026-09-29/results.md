# Public-tap verification results — 2026-09-29

Status: **ALL IMPLEMENTATION AND VERIFICATION TASKS COMPLETE; ONLY THE
IMPLEMENTATION PR (6.3) REMAINS OPEN**. Proven stages (details and links
below):

- Static preparation checks A1–A3, A6–A9 and F4
  ([`static-checks.log`](static-checks.log),
  [`python-lint.log`](python-lint.log)).
- Final normal-mode native reader + isolation gate, 20 fixture cases per
  OS, fresh positive controls and denied probe in every run (section R2).
- Reader-gate network and host-home denial with positive controls
  (C1/C2), credential sentinel containment (C3), output-exhaustion
  containment (C4), helper-only cache invalidation (C5), per-cask
  deadline kill (section DL), and the actual Linux product refusal of
  `sandbox-fallback = true` before any Ruby (C6, Linux).
- Final normal-mode whole import of all seven pinned sources on BOTH
  hosts. Linux: 30 records, 13 eligible / 17 excluded. macOS: 30 records,
  7 eligible / 23 excluded; the first three sources ran before the
  GitHub rate-limit reset, the last four after it; every row's import,
  strict offline `check_sources.py`, and Nix derivation evaluation
  returned 0; zero vendor payloads (E1, section GH).
- Actual Linux product negative gate: `sandbox = false`, relaxed, and
  fallback local configs refused before any Ruby; the strict local
  config under a `sandbox = false` daemon hit the behavioral probe
  (FAIL: host read ALLOWED), the export phase never ran, no flake was
  published, and the daemon was restored (C6/C7, Linux).
- Actual macOS product negative gate
  ([`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl),
  harness record [`negative-product-gate-macos.py.txt`](negative-product-gate-macos.py.txt)):
  all 4 cases pass — the strict local config refused `sandbox = false`,
  relaxed, and `sandbox-fallback = true` before any Ruby; the deliberately
  public daemon `TMPDIR=/tmp` with a strict local config caused the actual
  reader probe to report FAIL (host read ALLOWED), the export phase never
  ran, no flake was published, the daemon was restored, reader-phase-never-
  started held for the refusals, and vendor downloads were 0. Earlier
  harness setup failures are not product results.
- Converter stage closed for the corresponding fixture classes by seven
  actual capture-case regressions plus the backend library suite
  (section B).
- Actual interactive consent input gate, both OS, real PTY (section
  CONS).
- Actual native client lifecycle on both hosts through the real `pkg`
  client (section PKG; table D), and the client unit suites on both OS
  (section CLT).
- Official catalog retention in schema 3, final metadata audit, synthetic
  replay, existing suite reuse (section OR; table E), and final
  evaluation of every eligible official derivation with IFD and flake
  config disabled (section DRV).
- Final client app exposure gate check (section APP): the actual native
  Gatekeeper status was `assessments enabled`, and codesign plus the
  spctl assessment of the cached signed Raycast bundle passed (accepted,
  Notarized Developer ID). No native globally-disabled retest is
  claimed; the reject-disabled clause is unit-proven on both OS (APP).
- Final workflow, Python, Nix, docs, and CI records (table F): the final
  Linux CI archive passes every check; the macOS archive retains its
  first strict-docs failure, closed afterwards by two documentation-link
  repairs (`docs-final.log`, `fmt-final.log`), and its other logs passed;
  the post-format static lint (7 Nix + 7 Python + ShellCheck) passes; the
  post-format official derivation evaluation reproduces the identical
  result sha256.

Remaining open item:

1. 6.3: open the single reviewable implementation PR. The parent has
   completed the read-only code and integration review, and all requested
   fixes are done; no further production changes are pending.

C6/C7 macOS and the independent parent review are now closed with the
records above.

Out of scope, not pending acceptance: full GUI launches, application
runtime behavior, and app malware analysis (see APP).

The historical developer-mode reader archives stay historical. They are
not final reader or eligibility results.

Prepared scope: [README.md](README.md) ·
[matrix](../../../tools/public-tap-check/matrix.json) ·
[implementation contract](../../../openspec/changes/import-public-cask-taps/implementation-contract.md)

## A. Static asset checks

| ID | Check | Host | Command | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| A1 | data-file syntax | any | `python3 -m json.tool` on both matrix JSON files | PASS (static) | [`static-checks.log`](static-checks.log) |
| A2 | deterministic fixture generation (two temp dirs, no Ruby) | any | `make_fixtures.py --out` twice + `diff -r` | PASS (static): two runs byte-identical | [`static-checks.log`](static-checks.log) |
| A3 | output-path refusals | any | `make_fixtures.py --out` refusal behavior | PASS (static): fresh absolute accepted; relative, nonempty, and leaf-symlink refused. Helper-only variant bytes: cask identical, helper different (fixture property; the actual cache invalidation is C5 and passed) | [`static-checks.log`](static-checks.log) |
| A4 | reserved-host and literal-local URL scan | any | fixture read + `matrix.json` expectations | COVERED by B15–B18 (reader-stage loads both OS plus converter capture case `local_destination_urls_are_excluded`); no separate static record exists | sections B, R2 |
| A5 | cask-build-check plan tests reused | sandbox | `python3 tools/cask-build-check/plan_tests.py` | PASS: all plan.py regression checks passed | [`official-regression/plan-tests.log`](official-regression/plan-tests.log) |
| A6 | docs/HTML/OpenSpec checks | any | docs link checker; one-off HTML parse | PASS (static); rerun after doc updates (see final checks) | [`static-checks.log`](static-checks.log) |
| A7 | Python lint and format | any | `ruff format --check` / `ruff check` | PASS: final helper lint covers 5 Python files, all pass; recorded SHA256 values in the log | [`official-regression/helper-lint.log`](official-regression/helper-lint.log) |
| A8 | generated Ruby syntax-only check | any | `/usr/bin/ruby -c`; no evaluation | PASS (static, syntax-only): only the intended `pubtap-broken.rb` fails; no file was evaluated or executed | [`static-checks.log`](static-checks.log) |
| A9 | offline `check_sources.py` checker self-tests | any | manufactured valid catalog plus 5 negatives | PASS (checker-only); the actual imported catalogs are checked in E1 | [`static-checks.log`](static-checks.log) |

## B. Fixture cases (importer; agreed interface)

Reader stage (final normal mode): all 20 cases per OS ran with fresh
canary/host-loopback positive controls and a denied probe; see section
R2 and [`normal-reader-summary.json`](normal-reader-summary.json).
Reader observations per case (both OS): duplicate-token exports two rows
at the reader stage — the converter later rejects the duplicate
identity, so reader success is not source acceptance; staging, rescued
staging, broken, and collision-a viewer records fail with
`ruby-load-error` details (`staged_path`, `staged-path-during-source-eval`,
syntax error) beside healthy controls; the flights record loads with
preflight/postflight keys; steps, installer, flood, url-literal, app,
binary, branches, release-archive, and both invalidation variants load.
The branches fixture uses `depends_on macos: :ventura`.

Converter stage: closed for the corresponding fixture classes by two
actual records. First, the final normal-mode whole import of all seven
pinned sources on Linux (E1) converted real captures with the expected
codes. Second, the converter capture-case suite
([`converter-captures.log`](converter-captures.log)) passes 7/7:
`duplicate_token_refuses_the_whole_capture`,
`native_macos_capture_converts_on_linux_host`,
`normal_mode_legacy_preflight_postflight_capture_excludes_the_callback_cask`,
`staged_path_error_row_retains_validated_origin`,
`archive_and_raw_binary_captures_are_eligible`,
`helper_only_change_bumps_the_catalog_version`, and
`local_destination_urls_are_excluded`. These are unit tests over captured
data. They do not prove native runtime execution of every fixture tree.
The backend suite
([`backend-rust-tests.log`](backend-rust-tests.log)) adds 100 library, 1
binary, and 1 fixture test, including the config-gate rejections below.
Updated expected codes in
[`matrix.json`](../../../tools/public-tap-check/matrix.json): legacy
preflight/postflight → excluded `unsupported-artifact` (detail names the
callback keys); structured steps and installer script → excluded
`installer-script`; literal localhost/private/link-local/IPv6 URLs →
excluded `unsupported-url`. Fail-closed exclusions only; no permissive
statuses were added. Normal mode means `HOMEBREW_DEVELOPER` is explicitly
unset; upstream may still print a developer-command warning, which is not
a failure. Internal path loading is explicitly permitted for our staged
trusted reader.

| ID | Case | Expected (matrix) | Converter status |
| --- | --- | --- | --- |
| B01 | gh-release-archive | eligible both targets | PASS: capture case `archive_and_raw_binary_captures_are_eligible` + E1 Linux whole import |
| B02 | raw-binary | eligible both targets | PASS: same capture case + E1 Linux whole import |
| B03 | app-bundle-dmg | darwin eligible, linux excluded `unsupported-platform` | E1 Linux: excluded `unsupported-platform` rows observed (for example `aerospace@*` on Linux); E1 macOS: eligible `app` rows observed (`hashicorp-boundary-desktop`) |
| B04 | os-arch-language-branches | eligible both targets (`:ventura` fixture) | Reader-stage PASS; converter replay covered by library tests; native fixture whole-import not separately recorded (explicit limit) |
| B05 | helper-only-invalidation | both variants load; cache invalidates | PASS: reader gate (C5) + capture case `helper_only_change_bumps_the_catalog_version` |
| B06 | staging-accessor | excluded `ruby-load-error`, detail `staged_path`, plus healthy control | PASS: reader-stage observation + capture case `staged_path_error_row_retains_validated_origin` |
| B07 | staging-accessor-rescued | excluded `ruby-load-error`, detail `staged-path-during-source-eval`, sticky, plus control | PASS: reader-stage observation + same provenance capture case (sticky detail preserved) |
| B08 | preflight-postflight | excluded `unsupported-artifact`, detail names callback keys, plus control | PASS: reader-stage load observed + capture case `normal_mode_legacy_preflight_postflight_capture_excludes_the_callback_cask` |
| B09 | postflight-steps-run | excluded `installer-script` | Library-suite scope; no dedicated capture-case name. Explicit limit: no native fixture whole-import recorded |
| B10 | installer-script | excluded `installer-script` | Same limit as B09 |
| B11 | duplicate-token | source failure; prior catalog preserved | PASS: capture case `duplicate_token_refuses_the_whole_capture`; reader exports both rows first (section R2) |
| B12 | malformed-load-error | excluded `ruby-load-error`, plus control | Reader-stage PASS; library provenance case covers the error-row shape |
| B14 | output-exhaustion | contained: no hang, bounded capture, no empty success | PASS (C4) |
| B15–B18 | url-literal localhost/private/linklocal/ipv6 | excluded `unsupported-url` both targets | PASS: capture case `local_destination_urls_are_excluded` (unit scope); reader-stage load observed |
| B19 | cross-tap-collision | qualified identities distinct; bare tokens ambiguous incl. excluded | Client unit PASS both OS (`token_sources_include_excluded_matches`, `entries_resolve_eligible_excluded_and_unknown_tokens`); native two-tree import not separately recorded (explicit limit) |

The optional synthetic denial-probe case and its generated Ruby are
removed (they duplicated the parent's controlled native Nix probes and
the backend's reader-gate probe).

## C0. Parent-measured Nix sandbox evidence (background; not product tests)

Nix-only observations, kept as background. Rows and raw logs are
unchanged; see
[`sandbox-linux.log`](sandbox-linux.log),
[`sandbox-macos-negative.log`](sandbox-macos-negative.log),
[`sandbox-macos-positive.log`](sandbox-macos-positive.log),
[`sandbox-macos-shared-path.log`](sandbox-macos-shared-path.log), and
the lifecycle logs
([linux](lifecycle-linux.log), [macos](lifecycle-macos.log),
[encoded linux](lifecycle-linux-encoded.log)/[macos](lifecycle-macos-encoded.log),
[safeattr linux](lifecycle-linux-safeattr.log)/[macos](lifecycle-macos-safeattr.log)).
Summary of what they prove: strict Linux build denial of host read,
outside write, and controlled loopback (C0.1); macOS default daemon
allows all three and ignores a user `--option sandbox true` (C0.2,
negative); the final disposable-VM proof denies all three only with
daemon `sandbox = true`, `sandbox-fallback = false`, minimal
`sandbox-paths`, `__darwinAllowLocalNetworking = false`, and a private
build `TMPDIR` (C0.3); install/upgrade/rollback URL preservation
(C0.4), denial outside `/tmp` (C0.5), encoded-path references (C0.6),
and escaped-attribute lifecycle (C0.7). Default Darwin global tmp is
granted to builds; nested `sandbox-exec` failed. The product must fail
closed, explain the prerequisites, re-probe with a fresh nonce and fresh
paths before every raw import, never edit host configuration
automatically, and never add `trusted-users`. The parent probe source is
saved unmodified as [`native-sandbox-probe.py.txt`](native-sandbox-probe.py.txt).

## R. Actual native raw-reader and isolation-gate evidence — public taps

Final actual raw reader and its gate over all seven exact
`public-sources.json` pins, both hosts, with positive controls and
denied probes. Not downstream eligibility proof. Summary:
[`public-reader-summary.json`](public-reader-summary.json); captures:
[`public-reader-linux-evidence.tar.gz`](public-reader-linux-evidence.tar.gz)
(sha256 `f28d4162b71805965c3589ac9e3d9b214a12171061c3b007f726ee60a5de4b63`),
[`public-reader-macos-evidence.tar.gz`](public-reader-macos-evidence.tar.gz)
(sha256 `07e8ee96aa65d5993d3312c2228cc22e4f706760e1a6fd309e0c191060849f8f`).
Pinned Homebrew `cc9ff034b1d98cdd4061f68cf715bb967c483a8f`, Ruby 4.0.5.
Totals: Linux 28 exported + 2 load errors; macOS 27 exported + 3 load
errors (`aerospace`, `aerospace@0.20.3` both targets;
`hashicorp-vagrant` macOS only — `staged_path` during source eval).
These stayed allowed explicit `ruby-load-error` exclusions downstream,
and the final normal-mode matrices on both hosts confirmed them (E1).

## R2. Final normal-mode native reader — fixture matrix (2026-09-29)

Actual final normal-mode native reader over the 20 fixture cases on each
OS, every run with fresh canary/host-loopback positive controls and a
denied probe (`denied: host-read, host-write, loopback`). Read from
[`normal-reader-summary.json`](normal-reader-summary.json) only;
captures:
[`runtime-normal-linux-evidence.tar.gz`](runtime-normal-linux-evidence.tar.gz),
[`runtime-normal-macos-evidence.tar.gz`](runtime-normal-macos-evidence.tar.gz).
Key observations (both OS): the `dup` tree exports two identical
`pubtap-dup` rows at the reader stage — the converter later rejects the
duplicate identity, so this reader success is not source acceptance; the
staging, staging-rescued, broken, and collision-a `viewer` records fail
with the expected `ruby-load-error` details beside healthy controls; the
flights, steps, installer, flood, url-literal, and all supported-subset
records load. The two-MiB stdout flood is contained: parent-measured raw
stderr/capture sizes are Mac 1308/1967 bytes and Linux 1447/1971 bytes —
exact byte observations, kept separate from the `normalizedCaptureBytes`
fields in the reader summary JSON. Normal mode means `HOMEBREW_DEVELOPER`
explicitly unset; upstream may still print a developer-command warning.

## FX. Reader runtime fixture log — resolved

The earlier v4 Linux failure (`Encoding::CompatibilityError` under
US-ASCII on `String#strip` in `pubtap-broken`) is **FIXED**. The old
[`runtime-linux-fixtures-v4.log`](runtime-linux-fixtures-v4.log) is
historical, not a current failure. Current evidence is section R2: on
both OS the `broken` tree yields the excluded `pubtap-broken` record
with a bounded syntax-error detail plus the healthy
`pubtap-broken-control`.

## DL. Per-cask deadline kill (actual; both hosts)

Evidence: [`deadline-linux.json`](deadline-linux.json),
[`deadline-macos.log`](deadline-macos.log), probe source
[`deadline-probe.py.txt`](deadline-probe.py.txt). A stalled per-cask
Ruby child was killed after 60 s by a process-group kill; the whole
source failed; no capture was produced; this was not an empty success.
Linux: 63.73 s elapsed, 2650 diagnostic bytes. macOS: 66.18 s, 2398
bytes. Each run's probe phase printed `denied: host-read, host-write,
loopback`. The wrapper command returns success for these
expected-negative checks; the underlying Nix build exits 1.

## CRED. Credential sentinel containment (actual; both hosts)

Evidence: [`credential-linux.json`](credential-linux.json),
[`credential-macos.log`](credential-macos.log), probe source
[`credential-probe.py.txt`](credential-probe.py.txt). Both hosts:
positive-control host HOME file verified readable outside the gate;
through the gate the HOME read and write were denied; the sentinel was
absent from captured output (`sentinel_not_disclosed: true`). Result
`pass` on both.

## C. Strict sandbox proof (product gate)

| ID | Probe | Host | Status | Evidence |
| --- | --- | --- | --- | --- |
| C1 | network denial through the real reader gate, positively controlled loopback first; a missing listener is a failure | darwin | PASS (every normal-mode reader run and both credential/deadline runs) | sections R2, CRED, DL |
| C1 | same | linux | PASS (same scope) | sections R2, CRED, DL |
| C2 | host-home read denial through the gate with a positive-control canary | darwin | PASS | section CRED |
| C2 | same | linux | PASS | section CRED |
| C3 | credential canary unreadable; canary bytes absent from capture | darwin | PASS | section CRED |
| C3 | same | linux | PASS | section CRED |
| C4 | output exhaustion contained (fixed 2 MiB flood; no hang; no empty success) | darwin | PASS: raw stderr/capture 1308/1967 bytes | section R2 |
| C4 | same | linux | PASS: raw stderr/capture 1447/1971 bytes | section R2 |
| C5 | helper-only cache invalidation (both variants load; same cask source hash; helper VERSION 1.4.0 → 1.4.1) | both | PASS | [`helper-invalidation-native.json`](helper-invalidation-native.json) |
| C6 | sandbox fallback/relaxed mode refused | linux | PASS (product): the actual importer refused `sandbox-fallback = true` and the `sandbox = false`, relaxed, and fallback local configs before any Ruby, with no flake published — [`linux-config-negative-product.json`](linux-config-negative-product.json), [`negative-product-gate-linux.json`](negative-product-gate-linux.json), script [`negative-product-gate.py.txt`](negative-product-gate.py.txt) |
| C6 | same | macos | PASS (product): the strict local config refused `sandbox = false`, relaxed, and `sandbox-fallback = true` before any Ruby, with no flake published; backend config-gate unit coverage on both OS (see [`backend-rust-tests.log`](backend-rust-tests.log)) — [`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl), harness [`negative-product-gate-macos.py.txt`](negative-product-gate-macos.py.txt); earlier harness setup failures are not product results | — |
| C7 | product gate fails closed on insufficient denial; fresh nonce per import | linux | PASS (product): with the daemon at `sandbox = false` and a strict local config, the importer's behavioral probe reported FAIL (host read ALLOWED), refused before the export phase, and published no flake; the daemon was restored ([`negative-product-gate-linux.json`](negative-product-gate-linux.json)); failed `NIX_CONFIG` harness attempts are not product tests — the code correctly clears `NIX_CONFIG` | section C6 |
| C7 | same | macos | PASS (product): with the daemon deliberately public (`TMPDIR=/tmp`) despite the strict local config, the importer's behavioral reader probe reported FAIL (host read ALLOWED), the build was refused before the export phase, no flake was published, the daemon was restored, reader-phase-never-started held for the refusal cases, and vendor downloads were 0 ([`negative-product-gate-macos.jsonl`](negative-product-gate-macos.jsonl), harness [`negative-product-gate-macos.py.txt`](negative-product-gate-macos.py.txt)); harness setup failures are not product results | section C6 |

## CONS. Actual interactive consent input gate (both hosts; real PTY)

Evidence: [`interactive-consent-linux.json`](interactive-consent-linux.json),
[`interactive-consent-macos.json`](interactive-consent-macos.json); script
kept as data:
[`interactive-consent.py.txt`](interactive-consent.py.txt). A real PTY
drove both answers on each OS. Answer `n`: the prompt shows the source,
the canonical repository, and the approval scope; the command exits 1,
reports `not approved; nothing was fetched or changed`, and no approval
registry file is created. Answer `yes`: approval is recorded in the run,
execution proceeds to the Nix-executable check, fails there on the test
path (`/does/not/exist/pkg-test-nix`) before any import, and again no
registry file is created. This proves the input gate on both OS. The
successful `--trust` path and later stored-consent reuse remain proven by
the native lifecycle archives (section PKG). The noninteractive refusal
was already proven (D2).

## PKG. Actual native client lifecycle (both hosts; real `pkg` client)

Evidence: [`pkg-lifecycle-linux-evidence.tar.gz`](pkg-lifecycle-linux-evidence.tar.gz)
and [`pkg-lifecycle-macos-evidence.tar.gz`](pkg-lifecycle-macos-evidence.tar.gz),
each containing `commands.json` (exact calls, stdout/stderr, exit codes)
and `result.json` with `"result": "pass"`. Runner source kept as data:
[`pkg-lifecycle.py.txt`](pkg-lifecycle.py.txt).
Source `goreleaser/tap` (revisions `8751abbb…` → `77e442b6…`, commits
recorded in the result JSON). Vendor archives: four unique in total, two
per OS (budget recorded in `result.json`). Full GUI launches are not
proved. Both actual native clients exercised:

- `tap add` without `--trust` refuses (exit 1) before any import;
  with `--trust` it adds 6 eligible / 0 excluded.
- Qualified `info`/`search` on `goreleaser/tap/mcp` succeed; `--json`
  output recorded.
- `install` succeeds; profile and `--json list`/`info` show version
  0.5.1; `--help` (the execute/MCP help surface) exits 0.
- Failed update to an all-zero revision exits 1 and reports "the
  published generation and the registry are unchanged" — previous
  catalog preserved.
- Update to `77e442b6…` succeeds; explicit native `upgrade current`
  moves 0.5.1 → 0.6.0 (old/new store outputs recorded).
- `history` and `rollback` work; generations preserved.
- Corrupt saved capture: `info` fails reporting the malformed
  `index.json` and instructs `tap update` to repair; `tap update`
  repairs from the registry.
- `tap remove` blocks the source and keeps the installed binary;
  explicit `upgrade current` refuses with the removed-source guard;
  `upgrade --all` skips the removed-source entry by explicit native
  entry IDs (locked outputs kept).
- Native `remove current` uninstalls cleanly.

| ID | Check | Status | Evidence |
| --- | --- | --- | --- |
| D1 | consent refusal (interactive): no Ruby, no change | PASS: real PTY `n` on both OS exits 1 before any fetch/import; no registry created | section CONS |
| D2 | noninteractive add without `--trust` stops | PASS | section PKG |
| D3 | search: no prompt, no Ruby | PASS (noninteractive `--json search` exit 0) | section PKG |
| D4 | bare-token ambiguity incl. excluded entries | PASS (unit, both OS): `token_sources_include_excluded_matches` and `entries_resolve_eligible_excluded_and_unknown_tokens` | section CLT |
| D5 | qualified ID resolves only in its source | PASS (unit, both OS) plus native positive resolution in PKG | sections CLT, PKG |
| D6 | unknown qualified source instructs explicit add | PASS (unit, both OS): `unknown_tap_sources_name_the_add_command` | section CLT |
| D7 | `tap remove` blocks source, keeps packages | PASS | section PKG |
| D8 | failed refresh preserves previous catalog | PASS (native, section PKG) and unit `publication_failure_and_old_corruption_leave_the_active_generation` (both OS) | sections PKG, CLT |
| D9 | native update/rollback preserves generations | PASS | section PKG |
| D10 | after `tap remove`: explicit upgrade refuses; `--all` skips removed-source entries by explicit IDs | PASS | section PKG |
| D11 | corrupt saved catalog: failure reported with source; `tap update` repairs | Native PASS: corrupt capture reported with repair instruction and repaired via registry; unit `corrupt_saved_captures_fail_closed_before_any_use` (both OS) closes the fail-closed reading rule; the bare-token/search failure clauses during corruption are unit-covered (`load_fails_closed_on_missing_or_malformed_saved_catalogs`) | sections PKG, CLT |

## CLT. Client unit suites (final; both hosts)

Evidence: [`client-checks-linux.tar.gz`](client-checks-linux.tar.gz) and
[`client-checks-macos.tar.gz`](client-checks-macos.tar.gz). Each archive
holds `results.json` (all commands exit 0: `rustc`, `cargo test --locked
-p pkg-cli`, `clippy --all-targets`, `fmt --check`, `git diff --check`,
strict `cargo doc` with `-D warnings`) and the logs. Linux: 71 library +
10 CLI tests pass. macOS: 70 library + 10 CLI tests pass. Both snapshots
carry the same source hash. Coverage includes: excluded-token ambiguity,
corrupt-catalog fail-closed reading, the exact-enabled `spctl --status`
gate (`status_gate_accepts_only_the_exact_enabled_line`), read-only
assessment behavior, qualified-ID resolution, unknown-source handling,
atomic registry/exchange state (including real directory exchange and
non-UTF-8 byte paths), failed-refresh preservation, encoded-attribute
round trips, and consent flag behavior. Linux additionally performed a
real invalid-UTF-8 directory exchange; macOS APFS cannot create that
filename, and the pure-byte tests ran on both OS. Two docs-only
intra-doc-link fixes landed after the snapshot; the strict rustdoc
follow-up passed ([`docs-final.log`](docs-final.log)). These are unit
results. They do not replace the native lifecycle evidence in PKG.

## E. Real public taps, official catalog, and native sample

| ID | Check | Status | Evidence |
| --- | --- | --- | --- |
| E1 | final normal-mode pinned public taps imported; offline checker passes with JSON evidence | PASS (both hosts). Linux: all seven pins, 30 records, 13 eligible / 17 excluded — [`public-import-linux-final-evidence.tar.gz`](public-import-linux-final-evidence.tar.gz). macOS: all seven pins, 30 records, 7 eligible / 23 excluded — [`public-import-macos-final-evidence.tar.gz`](public-import-macos-final-evidence.tar.gz), [`public-import-macos-final-summary.json`](public-import-macos-final-summary.json); the first three sources ran before the GitHub rate-limit reset and the last four after it (section GH). On both hosts every row's `returncode`, `matrix_check.returncode`, and `derivation_evaluation.returncode` is 0, and zero vendor payloads were downloaded. The developer-mode archives stay historical | sections GH, B |
| E2 | synthetic payload replay via cask-build-check on schema 3 | PASS: full synthetic replay 2821/2821 applicable, count check OK; top-1000 replay 575/575 applicable, count check OK | [`official-regression/replay-all.json`](official-regression/replay-all.json), [`official-regression/replay-top1000.json`](official-regression/replay-top1000.json) |
| E3 | bounded native sample install/use/update/rollback | PASS (both OS; 2 unique vendor archives per OS; explicit native upgrade 0.5.1 → 0.6.0; history/rollback; GUI not launched) | section PKG |
| E4 | official JSON source retained and schema 3 | PASS: catalog sha256 `1017381d657fd026c6ddd25032974b91bc0b7a6bde4e0ef2aa3cc0a80c92692e`, schema `pkg-cask-catalog/3`, 7709 entries all `homebrew/cask` | [`official-regression/official/corpus-report.json`](official-regression/official/corpus-report.json) |

Linux E1 per-source outcomes (13 eligible / 17 excluded): `goreleaser/tap`
6 eligible / 0 excluded; `gitfool/vpinball` 7 eligible / 1 excluded
(`unsupported-download-spec`, the nightly token); `nikitabobko/tap` 0 /
11 (`ruby-load-error` for `aerospace` and `aerospace@0.20.3`;
`unsupported-platform` for the other nine pinned versions);
`hashicorp/tap` 0 / 2 and `gcenx/wine`, `dotsecenv/tap`
(`formula-dependency`), `sourcemeta/apps` (both
`unsupported-platform`) 0 / 1 each.

macOS E1 per-source outcomes (7 eligible / 23 excluded): `goreleaser/tap`
6 eligible / 0 excluded; `gitfool/vpinball` 0 / 8 (seven `installer-script`
records including `vpxtool`, plus `unsupported-download-spec` for the
nightly token); `nikitabobko/tap` 0 / 11 (`ruby-load-error` for
`aerospace` and `aerospace@0.20.3`; `installer-script` for
`aerospace@0.11.2`; `malformed-record` for the other eight pinned
versions); `hashicorp/tap` 1 / 1 (`hashicorp-boundary-desktop` eligible;
`hashicorp-vagrant` excluded `ruby-load-error`); `gcenx/wine` 0 / 1
(`installer-script`); `dotsecenv/tap` 0 / 1 (`formula-dependency`);
`sourcemeta/apps` 0 / 1 (`installer-script`).

## DRV. Final evaluation of all eligible official derivations

Evidence: [`official-all-derivations-final.json`](official-all-derivations-final.json).
Every eligible official derivation was evaluated with IFD and flake
configuration disabled. Result `pass`: schema `pkg-cask-catalog/3`, 3143
`aarch64-darwin` + 160 `x86_64-linux` = 3303 derivations evaluated, zero
vendor downloads. The final official catalog stays sha256
`1017381d657fd026c6ddd25032974b91bc0b7a6bde4e0ef2aa3cc0a80c92692e` with
7709 entries (E4). The existing final metadata and synthetic evidence
remains correct. After the final formatting follow-up, the parent
re-ran the evaluation ([`post-format-official-eval.json`](post-format-official-eval.json)):
all 3303 derivations still evaluate with result `pass` and the identical
evaluation sha256
`e4236969da8ae9249a5fd9f7b46df8c101cd0ad5ed2c93af52685f0e26ba9f2f`;
zero vendor downloads. The earlier
[`mechanical-lint-final.log`](mechanical-lint-final.log) run stays
history; the current state is all-pass.

## GH. GitHub API rate limit (practical limit; macOS final matrix)

Evidence: [`github-macos-rate-limit.log`](github-macos-rate-limit.log).
Unauthenticated GitHub API calls are limited to 60 per hour. During the
final macOS normal-mode matrix the limit was exhausted
(`x-ratelimit-remaining: 0`), which can stop `tap add`/`tap update`. The
retry runs after the server reset. This is a practical transport limit,
not an importer defect: a failed import preserves the prior saved state,
and saved-search and saved-install do not refresh or run raw Ruby. No
token support is promised, and there is no hosted API service.

## OR. Official catalog final regression (metadata and synthetic only)

Evidence directory: [`official-regression/`](official-regression/);
exact calls in [`official-regression/commands.json`](official-regression/commands.json).
Final catalog sha256 `1017381d657fd026c6ddd25032974b91bc0b7a6bde4e0ef2aa3cc0a80c92692e`;
report covers 7709 entries; 3143 Darwin-eligible and 160
Linux-eligible entries; zero metadata violations. Full
synthetic replay: 2821 applicable, all processed, count check OK.
Top-1000 replay: 575 applicable, all processed, count check OK. Ranked
tokens: 997 of 1000 present; 3 missing (`homebrew-app`, `crisp`,
`unsloth`), reported and never backfilled. No vendor payloads were
downloaded. Existing synthetic archive suite: 37 passes; AppImage name
checks: 30 passes (each scope stated in its log). The final URL policy
newly excludes the mycard Darwin port-444 destination. Final helper
lint: 5 Python files pass
([`official-regression/helper-lint.log`](official-regression/helper-lint.log)).
The CSV output uses LF line endings. Logs:
[`audit.log`](official-regression/audit.log),
[`plan-tests.log`](official-regression/plan-tests.log),
[`appimage-names.log`](official-regression/appimage-names.log),
[`replay-all.log`](official-regression/replay-all.log),
[`replay-top1000.log`](official-regression/replay-top1000.log).

## APP. Client app exposure check (final; targeted)

Final proof: [`apps-native-check4.log`](apps-native-check4.log) (JSONL
commands plus final result object). Check script preserved as data:
[`apps-native-check.py.txt`](apps-native-check.py.txt). On a real Nix
profile, `pkg apps sync` exposed an already-cached signed Raycast
bundle through the real helper launcher; adding an unsigned test app
made `codesign --verify --deep --strict` reject the sync, and the
previous launcher bytes and symlink targets were preserved (9 launcher
files unchanged). No new vendor payload was downloaded; no GUI
execution. The earlier `apps-native-check3.log` is historical.
The actual native Gatekeeper status was recorded as `assessments
enabled`, and both `codesign --verify` and the spctl assessment of the
cached signed Raycast bundle passed (accepted, Notarized Developer ID)
([`gatekeeper-native-enabled.log`](gatekeeper-native-enabled.log)); this
closes the native-enabled observation. The exact-enabled `spctl
--status` gate clause is unit-proven on both OS
(`status_gate_accepts_only_the_exact_enabled_line`, section CLT). No
native globally-disabled-Gatekeeper retest is claimed as passed; the
reject-disabled clause is unit-proven on both OS, and it stays an
explicit native limit. Scope limits: targeted app exposure gate over
vendor `.app` bundles, not public-tap import/lifecycle proof; CLI
binaries and ordinary Nixpkgs apps are outside this gate. Full GUI
launch, application runtime behavior, and app malware analysis are out
of scope, not pending acceptance.

## F. Lint, OpenSpec, CI

| ID | Check | Status | Evidence |
| --- | --- | --- | --- |
| F1 | Rust fmt/clippy changed files | PASS (both OS). Linux final CI archive: every check exits 0 — locked `build --all-targets`, `test`, `fmt --check`, `clippy --all-targets -D warnings`, strict docs, `fetch`, `deny` (bans/licenses/sources), and release build ([`pkg-final-ci-linux-evidence.tar.gz`](pkg-final-ci-linux-evidence.tar.gz)). macOS archive retains its first strict-docs failure (exit 101); the two after-test code changes were documentation links only, and the repair is closed by `docs-final.log` (loose in this directory) and the archive's `fmt-final.log` (both pass); the other macOS logs (build/test/clippy/deny/release/help/completion) passed per the parent. The original failure is kept as history, not hidden and not treated as current | section CLT, [`ci-macos-evidence.tar.gz`](ci-macos-evidence.tar.gz), [`docs-final.log`](docs-final.log) |
| F2 | Ruby lint (reader assets) | COVERED WITH LIMITS: all 9 bounded-reader checks pass under pinned Ruby 4.0.5 with `LC_ALL=C` ([`ruby-reader-checks.json`](ruby-reader-checks.json)); syntax-only `/usr/bin/ruby -c` over the generated fixtures (A8, only the intended `pubtap-broken.rb` fails) plus the recorded actual reader runs and the seven capture-converter cases; a full style-lint run is not claimed. Ruby reader tests and cases are recorded separately (sections R, R2, B) | sections A8, B, R2 |
| F3 | Nix lint | PASS (current): the parent's post-format record — nixfmt, statix, and deadnix on the final 7 Nix files, `ruff check`/`format` on the 7 Python files, and ShellCheck on `probe.sh` and `brew-boot-adapter.sh` — all pass ([`post-format-static-lint.json`](post-format-static-lint.json)). The earlier follow-up run stays history ([`mechanical-lint-final.log`](mechanical-lint-final.log), [`nixfmt-final.log`](nixfmt-final.log)) | [`post-format-static-lint.json`](post-format-static-lint.json) |
| F4 | strict OpenSpec validation | PASS (static) | [`static-checks.log`](static-checks.log) |
| F5 | workflow lint and normal CI | PASS: `actionlint` empty output; 57 offline fetch tests pass with `ruff check`/`format`; final Linux CI archive all checks exit 0 (F1); macOS CI closed as recorded in F1 | [`workflow-actionlint.log`](workflow-actionlint.log), [`workflow-python-tests.log`](workflow-python-tests.log), [`pkg-final-ci-linux-evidence.tar.gz`](pkg-final-ci-linux-evidence.tar.gz) |
| F6 | independent parent review | PASS: the parent completed the read-only code and integration review and all requested fixes; no further production changes are pending | parent record; table F text |

Additional final lint records:
[`workflow-python-tests.log`](workflow-python-tests.log) — 57 offline
safe-fetch tests, `ruff check` and `ruff format --check` pass;
[`docs-final.log`](docs-final.log) — strict macOS rustdoc success;
[`mechanical-lint-final.log`](mechanical-lint-final.log) — nixfmt,
statix, deadnix, and ruff for the fetch/test files;
[`nixfmt-final.log`](nixfmt-final.log) — the closed formatting follow-up
with `nix-instantiate --parse` and `git diff --check`;
[`official-regression/helper-lint.log`](official-regression/helper-lint.log)
— the five owned Python helpers pass with recorded SHA256 values;
[`post-format-static-lint.json`](post-format-static-lint.json) — the
current all-pass static-lint record over the final Nix, Python, and
shell files; [`post-format-official-eval.json`](post-format-official-eval.json)
— the post-format re-evaluation of all 3303 official derivations with
the identical result sha256 (section DRV);
[`pkg-final-ci-linux-evidence.tar.gz`](pkg-final-ci-linux-evidence.tar.gz)
and [`ci-macos-evidence.tar.gz`](ci-macos-evidence.tar.gz) — the final
CI archives.

## Recording rules

Replace a `UNRUN`/`PENDING` marker only with an actual outcome, command,
exit code, evidence path, and limits. Parent-measured observations stay
in their labeled sections. Do not merge or release without a later
instruction.
