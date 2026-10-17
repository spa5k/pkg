# CLI test cleanup

Date: 2026-10-17.
Baseline: `ddac23afc19a3185fb73e8b2a3db51b55862c849`, fetched from `origin/main`.
Host: macOS, Apple silicon. Compiler: Rust 1.99.0. Nix: 2.35.2, Determinate Nix 3.22.1.
Branch: `feat/test-behavior-cleanup`.
The later [native end-to-end extension](native-e2e-2026-10-17.md) records the
additional Linux, concurrency, build-failure, and expired-generation proof.
The pre-existing untracked paths `.agents/skills/.agents/` and
`.agents/skills/skills-lock.json` were excluded from this change.

## Findings and cutover

The JSON version test used the production constant as its expected value.
Changing that constant from 1 to 2 left the unchanged test green. Parser checks
repeated behavior that the compiled CLI can own. Config checks compared defaults
with the same defaults after serialization. Some CLI checks inherited the user's
home and config.

The cutover removes 11 redundant internal declarations. It moves their useful
assertions to the compiled CLI and the existing real Nix check. The CLI helpers
now use temporary homes and explicit XDG bases. No product behavior changes.
No test-only production API or new test framework was added.

The [decision ledger](test-cleanup-2026-10-17.tsv) records 12 old declarations.
It has 13 rows because the config roundtrip and typed-cause assertions have
different decisions. `C` rows became deletion approvals only after keeper proof.
`D` removes the default self-comparison and serialization fixture. `R` keeps the
typed TOML cause check under `config_load_retains_the_decode_cause`.

That retained check is a useful exception to broad unit-test removal. It reads
an invalid file and checks the original error type. Rendered CLI text cannot
prove a downcastable cause. Existing ownership, process, and hostile-filesystem
tests were not selected for deletion.

## Keeper contracts

| Keeper | Observable contract and defect detected | Why another layer does not replace it |
| --- | --- | --- |
| `invalid_command_combinations_fail_before_loading_config` | Sixteen invalid argument combinations return status 2 and no stdout. A malformed config cannot mask the usage error. | Parser-only checks cannot prove process exits or dispatch order. |
| `prune_requires_an_explicit_positive_age` | Zero, negative, empty, missing-unit and overflowing ages fail before runtime work. Missing age returns status 2. | The successful native journey does not supply these invalid inputs. |
| `relative_xdg_bases_are_rejected` | Each relative state, config and cache base fails with the named diagnostic. | Native positive paths cannot prove refusal for each variable. |
| `invalid_config_is_reported_without_replacing_the_file` | Invalid syntax, wrong types and unknown fields at every config level fail with unchanged file bytes. | A direct decoder call does not prove config loading and command refusal. |
| `history_color_policy_applies_to_native_output` | The real CLI preserves raw child output by default. Both the CLI flag and config remove CSI sequences, including a partial sequence. Other escapes remain. | Real Nix does not reliably emit the malformed escape inputs. |
| `tools/native-cli-check.py::main` | Real Nix profiles prove literal schema 1, list provenance against the manifest, mixed alias installs, exact entry selection, option refusal without mutation, fresh and broken doctor state, and retention of recent generations. | Controlled runtime fixtures do not prove Nix profile behavior. Existing native assertions did not check these output and refusal details. |

The existing native journey still owns package execution, upgrade, rollback,
removal, real signal cancellation, Formula eligibility, macOS bounds, and macOS
helper installation and app sync. It uses local derivations and disposable
profiles. The changed list assertion reads Nix's manifest independently of the
CLI's JSON serialization.

The doctor checks preserve config, registry, manifest, and active-profile link
state. They forbid new state files and links before list runs. Real Nix creates
empty profile directories during its probes. `pkg list` can create its own lock
file. Those facts do not establish a repair or a package mutation.

## Mutation evidence

Every mutation ran in `/tmp/pkg-test-audit-7vw6lly6/source`, an archive of the
pinned baseline. The CLI keepers were copied into that isolated tree. No shared
checkout source was mutated. Each selected test executed once and failed on an
assertion. No compile failure, skip, or empty selection counted as proof.

| Mutation | Unchanged old test | New keeper |
| --- | --- | --- |
| `JSON_SCHEMA` changes from 1 to 2 | `json_envelope_is_versioned` passes, one test. | The native check fails at `assert envelope["schema"] == 1` on a real doctor process result. |
| Remove the upgrade-all conflict with entry names | Not rerun. | The usage keeper fails. |
| Remove root config `deny_unknown_fields` | Not rerun. | The invalid-config keeper fails. |
| Remove the zero-day age refusal | Not rerun. | The prune-age keeper fails. |
| Return native color text without CSI removal | Not rerun. | The history-color keeper fails. |

The normal candidate CLI suite passed with all 21 tests. The expanded native
candidate passed before old declarations were removed. Mutation logs and
`mutations.json` remain in `/tmp/pkg-test-audit-7vw6lly6` on the execution host.
The restored isolated `output.rs` SHA-256 is
`7105867706e4095e9d6c703e8085d3c341b1222f3b20457e92d513a3a5ab889e`.
The mutation runner restored each changed file byte for byte.

## Baseline and test corrections

The original CLI target passed all 19 tests. The original library target passed
153 tests and ignored four native-only tests. The original native journey passed
on macOS. No baseline product defect was found in this batch.

Early candidate runs exposed two wrong fixture assumptions. Nix creates empty
state directories during read-only probes. Verbose removal retains an ignored
Nix telemetry permission diagnostic. The final assertions permit empty
directories and keep the compact-removal filter check in compact mode. These
were test corrections. No failing baseline test was removed to obtain a pass.

## Validation

All commands below passed on the local macOS host with the locked dependency
graph. Cargo commands selected Rust 1.99.0 explicitly because the host environment
still set `RUSTUP_TOOLCHAIN=1.96.1`.

| Command | Result |
| --- | --- |
| `cargo +1.99.0 build --locked --workspace --all-targets --all-features` | Passed. |
| `cargo +1.99.0 test --locked --workspace --all-features` | Passed. Client library: 142 passed, four native-only tests ignored. Compiled CLI target: 21 passed. |
| `cargo +1.99.0 test --locked -p pkg-cli --test cli` | Final CLI target passed all 21 tests after the assertion lint fix. |
| `cargo +1.99.0 test --locked -p pkg-cli --lib --all-features native_flake_discovery -- --ignored` | All four native discovery tests passed without skips. |
| `python3 tools/native-cli-check.py --pkg target/debug/pkg` | Final built candidate passed the full native journey after internal test removal. |
| `cargo +1.99.0 fmt --all --check` | Passed. |
| `cargo +1.99.0 clippy --locked --workspace --all-targets --all-features -- -D warnings` | Passed after replacing one empty-value assertion with `assert_eq!` for its failure diagnostic. |
| `RUSTDOCFLAGS="-D warnings" cargo +1.99.0 doc --locked --workspace --all-features --no-deps --document-private-items` | Passed. |
| `cargo +1.99.0 fetch --locked` and `cargo +1.99.0 deny --locked --offline check bans licenses sources` | Passed the same dependency policy as CI. |
| `sh -n install.sh` and `shellcheck install.sh` | Passed. |
| `python3 -m unittest discover -s docs/tests -v` | 37 passed. Seven Linux-only tests skipped on macOS. |
| `python3 -m unittest discover -s .github/scripts/tests -v` | All 15 passed. |
| `python3 .github/scripts/check_docs_links.py` and `git diff --check` | Passed. |

Logs remain under `/tmp/pkg-test-audit-7vw6lly6/checks` on the execution host.
`native-final.log` records the final native result. Successful native runs remove
their disposable profile directories. The retained log is execution evidence.
It is not a release or clean-host installation report.

Ledger validation codes refer to these commands. `C` is the compiled CLI target.
`L` is its owning library test in the workspace run. `N` is the real Nix journey.
`M` is the isolated schema mutation comparison.

Production code: zero changed lines. Rust tests: 130 added lines and 273 removed
lines, including the CLI fixtures. Native test support: 96 added lines and 11
removed lines. Test and support code has a net reduction of 58 lines. Audit and
OpenSpec documents are separate from those counts.

Edits used unique test-module anchors and ordinary file patches.
`skip: the operation has no rename preview.`
`skip: no edit was interrupted, so reconciliation was not needed.`
The PR records the final commit and hosted CI status.

## CI and evidence limits

The Linux Rust job and macOS smoke job run the compiled CLI suite. Both smoke
jobs run the existing native CLI check after Nix installation. CI routing and
security gates stay unchanged. Hosted Linux and macOS checks are separate from
this local macOS result.

All declarations outside the ledger and the named keeper additions remain `U`.
This is one coherent cleanup batch. It is not a complete subsystem campaign or
a review of every test. The prune addition proves retention of recent generations.
It does not prove removal of expired generations. This run does not prove reboot,
a clean-host product installation, Gatekeeper, or release publication.

The audit skill's Rust 1.96.1 requirement and old engine references are stale.
Current CONTRIBUTING.md and rust-toolchain.toml select Rust 1.99.0. The old plan
is superseded. `tools/verify/TEST-CONTRACTS.md` and `spikes/s2-tough` are absent.
No deleted engine test machinery was restored.

TraceDecay served the indexed main ancestor. Its dependent-file lookup was
partial. Its combined diff lookup failed because a test helper has two scoped
platform declarations. Direct source and caller searches, CI reads, and native
execution supplied the missing evidence. No graph result was treated as executed
coverage.
