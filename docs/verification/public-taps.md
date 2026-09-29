# Public-tap import verification — 2026-09-29

Status: **implementation and local verification complete**. Delivered in [PR #82](https://github.com/spa5k/pkg/pull/82). Not merged, not released. Raw logs, archives, and per-row tables are immutable history at the pinned baseline commit `913dedb3131a3f1db4c3ab385e3dc4ca495c21f3`:

- [Reader, gate, converter, lifecycle, and CI results](https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/docs/verification/public-taps-2026-09-29/results.md)
- [Real Linux installs from three public taps](https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/docs/verification/public-taps-real-installs-2026-09-29/README.md)

This page keeps the summary and the scope limits. It does not duplicate the full old logs.

## Whole import — seven pinned public taps, 30 casks

Normal-mode whole import of all seven `public-sources.json` pins on both hosts. Every row's real import, strict offline `check_sources.py`, and Nix derivation evaluation returned 0. Zero vendor payloads were downloaded.

| Host | Records | Eligible | Excluded | Notes |
| --- | --- | --- | --- | --- |
| Linux (x86_64) | 30 | 13 | 17 | all seven pins |
| macOS (aarch64) | 30 | 7 | 23 | first three sources before the GitHub rate-limit reset, last four after it |

Reader-stage success is not source acceptance; the duplicate-token fixture exports two rows at the reader stage and the converter then rejects the duplicate identity. All exclusions were fail-closed; the stable classification codes and the per-entry outcomes of the 30 public-source records are converter-scope detail, tabulated in the immutable results report.

## Reader, sandbox gate, and fixture matrix

- Final normal-mode native reader ran the 20 fixture cases per OS, each run with fresh positive controls and a denied probe (host read, host write, loopback). Pinned Homebrew `cc9ff034…`, Ruby 4.0.5, `HOMEBREW_DEVELOPER` explicitly unset.
- Consent gate: real PTY on both OS. Answer `n` exits 1 before any fetch or import. Answer `yes` proceeded to an intentionally failed import (the Nix check failed), and that failed run published no registry file; a successful approved import does publish one. Noninteractive `tap add` without `--trust` stops.
- Isolation: host-home read/write denied through the reader gate; credential sentinel bytes absent from captures; 2 MiB output flood contained (no hang, no empty success); stalled per-cask Ruby killed after 60 s by process-group kill, whole source failed, no capture.
- Product negative gate (both hosts): the importer refused `sandbox = false`, relaxed, and `sandbox-fallback = true` local configs before any Ruby. Under an unsafe daemon (Linux `sandbox = false`; macOS public `TMPDIR=/tmp`), the behavioral probe reported FAIL (host read ALLOWED), refused before export, and published no flake. Fail-closed; fresh nonce per import; no automatic host config change; no `trusted-users`. The verification harness, not the product, restored the disposable daemon afterward; the product never reconfigures the host daemon.
- Helper-only cache invalidation: same cask bytes, helper `VERSION` 1.4.0 → 1.4.1; whole-tree identity changed and cached output invalidated.
- Converter: 7/7 capture-case regressions and the backend suite (100 library + 1 binary + 1 fixture test). Explicit limits: the installer-script and branch fixture whole-imports and the native two-tree collision import were not recorded natively; they stay unit/converter scope.

## Native lifecycle

Real `pkg` client on both hosts, source `goreleaser/tap`: consent, qualified `info`/`search` (no prompt, no Ruby), install, failed update preserving the prior catalog, update, explicit native upgrade 0.5.1 → 0.6.0, history/rollback, corrupt saved catalog repair via `tap update`, `tap remove` blocking the source while keeping the installed binary, removed-source upgrade guard, `--all` skipping removed-source entries, and native uninstall. Client unit suites: Linux 71 library + 10 CLI tests, macOS 70 + 10, all passing. Old generations are preserved through the native profile lifecycle.

## Real Linux installs — four apps, three taps, five payloads

Four applications from three public taps (GoReleaser 2.18.1/2.18.2, vpxtool 0.34.7, DNSControl 5.2.0, PinOne Config Tool 2.2.2) installed in a clean Linux sandbox. The five unique vendor files total 215,554,198 bytes. Substantive checks: GoReleaser validated a release config, built a tiny Go program with module downloads disabled, and ran upgrade/rollback to exact Nix outputs; vpxtool created, verified, and extracted a VPX table; DNSControl rendered A, CNAME, and TXT records and rejected an invalid IPv4 address; the AppImage Electron ran without host FUSE (`ELECTRON_RUN_AS_NODE=1`).

Three actual packaging defects were fixed and retested: compressed manual pages (one known compression suffix after sections 1–8), a binary AppImage routed through the Nix wrapper (no host FUSE), and shell completion names per the pinned Homebrew rules — Bash, Zsh (`compinit` `registered=_goreleaser`), and Fish all loaded real completions.

## Official catalog and prior evaluation

The official catalog stays schema `pkg-cask-catalog/3`, source `homebrew/cask`, 7709 entries; its current sha256 is in the immutable baseline archives. Metadata audit, synthetic replay (2821/2821 and 575/575 applicable), and the prior official evaluation of all 3303 eligible derivations (3143 `aarch64-darwin` + 160 `x86_64-linux`, IFD and flake config disabled, zero vendor downloads, identical result sha256 after formatting) remain metadata/synthetic records. No vendor runtime execution is claimed.

## App exposure gate

On a real Nix profile, `pkg apps sync` exposed a cached signed Raycast bundle; native Gatekeeper status was `assessments enabled`; `codesign --verify --deep --strict` and the spctl assessment passed (Notarized Developer ID). An unsigned test app made the sync reject. Launcher bytes were preserved. CLI binaries and ordinary Nixpkgs apps are outside this gate.

## Limits (truthful scope)

- No GUI launch, hardware use, DNS publishing, or release publishing is claimed.
- vpxtool prints a malformed-file message on stderr and returns exit 0 for invalid input; that is an upstream CLI limitation, not proof.
- Unauthenticated GitHub API is limited to 60 calls per hour; a failed import preserves the prior saved state; no token support is promised.
- No fresh macOS payload-install rerun was done; the macOS records are the 2026-09-29 import matrix and supplemental checks.
- Ruff still reports 6 pre-existing diagnostics; there are no new ones.
- Ordinary search and install never run Ruby; only explicit `tap add`/`tap update` imports do.

Scope of proof: unit suites and converter capture cases cover their classes; reader-gate and sandbox probes cover the gate; whole-import counts cover the seven pinned sources; the real-install report covers the four Linux applications. Keep these scopes separate when citing results.

## Final cleanup and attack pass

The cleanup removed 88 log, archive, and duplicate plan files from this PR.
The history links above retain the original evidence. Existing regression
tests and fixtures are unchanged. Runtime edits remove an unused constant,
a redundant import, and stale comments.

GLM 5.3 ran a new attack pass in E2B. The parent reviewed the scripts,
corrected test setup errors, and replayed the final cases in a separate
sandbox without model credentials. All 762 checks passed: 28 payload-path
cases, 15 source-archive cases, 41 URL/DNS cases, 7 Rust URL cases, 20 plan
cases, 625 name-encoding combinations, 10 saved-catalog cases, and 16 CLI
cases. The CLI replay includes a real PTY refusal, a positive control for
Nix-call detection, no Nix calls after refused consent, and preservation of
corrupt registry bytes. Source hashes stayed unchanged during the tests.
No new safety bypass or product regression was reproduced.

The independent final Linux checks passed: 192 Rust tests, 57 fetch tests,
51 plan checks, a forced rebuild of all 9 reader checks, formatting,
strict Clippy and Rustdoc, Python lint on new files, shell/Nix lint, and
OpenSpec validation. A real import of `goreleaser/tap` at
`77e442b6000587bf57e543081e95fd908b918bd7` produced 6 eligible records.
Saved `info` and tap removal passed with a state path containing spaces.
This final pass downloaded no application payload.

Limits: this attack pass ran on Linux. It adds no macOS, GUI, hardware,
live DNS-rebinding, or concurrent CLI-race proof. Some malformed inputs
produce Python exceptions instead of short errors. The source extractor
can turn a regular tar member with a trailing slash into an empty directory;
that shape is not produced by the tested GitHub archives. Neither case
escaped the staging directory. The raw attack scripts and logs are kept
outside the PR to avoid adding another generated evidence tree.
