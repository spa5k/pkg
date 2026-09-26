# Active implementation plan

Status: alpha product. Reviewed on 26 September 2026. The nine bug fixes are
committed in `538cdbe`; the six UX fixes are committed in `03c4e3a`.
Alpha.56 publication is in progress. The exact alpha.55 check found an old
macOS wrapper startup message. Alpha.55 remains unpublished; its signed assets
and deployed sequence 55 are retained unchanged. Alpha.56 also makes the Linux
profile MANPATH setup idempotent.
The filename is retained for existing links. The original four-PR migration
stack is complete. New work branches from current `main`.

## Current state

- The Determinate migration and the Linux and Apple Silicon macOS cutovers are
  complete. [Completed work](completed-alpha-work.md) records the evidence.
- [Alpha.54](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.54) is the
  current public release. Alpha.53 remains an unpublished candidate.
- PUBLIC-23 merged in [PR #61](https://github.com/spa5k/pkg/pull/61). Its bounded
  startup check is on `main`, but it is not in alpha.54.
- The owner resumed alpha release work on 26 September. Alpha.56 will include
  PUBLIC-23, the nine bug fixes, and the six UX fixes after the release gates.
- Production trust is not active. The alpha uses a test root and ad-hoc macOS
  signing. Tooling readiness does not mean production readiness.

The only active implementation plan is this file. OpenSpec files below describe
remaining work in this plan. Completed and superseded UX proposals have been
removed; their replacements are in the completed-work record and Git history.

## Remaining work

### QUALITY-01: onboarding and code-quality follow-up

- **Purpose:** test the public user path and keep one measured quality backlog.
- **Owns:** disposable-VM onboarding evidence, issue #4, lint measurements,
  and consistent plans. Runtime fixes need a separate implementation change.
- **Depends:** current `main` and the published alpha.54 assets.
- **Tests & gates:** fresh install; shell setup; help; cached and public-source
  packages; preview and approval; errors; pinning; upgrades; history; rollback;
  repair; cleanup; repeat setup; reboot; system removal; current quality gate;
  docs links and evidence review.
- **Rollback:** revert the documentation change. No release or user-state
  migration is included.

The [26 September VM audit](../tests/macos-clean-host/ONBOARDING-2026-09-26.md)
records measured onboarding and code-quality findings.
The [daily-use and logic audit](../tests/macos-clean-host/BUG-HUNT-2026-09-26.md)
adds 68 VM cases and nine confirmed defects. All nine now have committed fixes
and native macOS regression evidence. The original failures remain historical
evidence; they are no longer open implementation tasks.
[Issue #4](https://github.com/spa5k/pkg/issues/4) owns the quality backlog.
Keep observed user failures separate from lint debt. Record the tested release,
source revision, platform, and exact command. A passing happy path does not
prove every interruption or security boundary.

The [fix status and evidence](../tests/macos-clean-host/BUG-FIXES-2026-09-26.md)
link the nine separate implementation checkpoints and the final combined checks.
Independent E and A source reviews found no concrete defects. Native Linux
validation remains a merge gate. VERIFY-01 owns the remaining cross-platform
and wider interruption matrix; do not recreate the regressions already added.

The six onboarding UX findings are implemented in `03c4e3a`.
[UX verification](../tests/macos-clean-host/UX-FIXES-2026-09-26.md) records their
tests and VM checks. They are not additional open implementation tasks.

Remaining code-quality work:

- Reduce measured lint debt without increasing a per-file baseline.
- Implement the repository pattern counter described by ADR 0005.
- Resolve target-specific dependency diagnostics before enabling
  `unused_crate_dependencies` as a deny lint.
- Enable the strict touched-file rule only after the required debt is removed.
- Review complex acquisition, build, commit, and privileged filesystem paths
  with focused behavior tests. A graph score does not justify deleting code.

### VERIFY-01: repeatable assurance

- **Purpose:** make the existing proofs repeatable and close measured test gaps.
- **Owns:** repeat-proof completion, nightly execution, stable-state fixtures,
  mutation-boundary coverage, fuzzing, and lifecycle sequence tests.
- **Depends:** the existing hermetic test and static-audit gates.
- **Tests & gates:** two complete macOS repeat slots with real reboots; native
  Linux evidence; passing scheduled fault and real-Nix jobs; reviewed fixtures;
  reproducible failure artifacts. Preserve the planned two-week observation
  period before promoting new assurance checks to production prerequisites.
- **Rollback:** revert new test tooling. Do not weaken an existing passing gate.

The workflow exists, but the latest retained
[repeat-proof run](https://github.com/spa5k/pkg/actions/runs/33898972544) failed.
The latest inspected [nightly run](https://github.com/spa5k/pkg/actions/runs/36109155550)
was cancelled; its fault and real-Nix jobs were skipped. This is separate from
passing main CI and the historical DN-16 proof.

The [verification tasks](../openspec/changes/add-deterministic-verification-suite/tasks.md)
list the remaining work. Clock injection, hermetic checks, basic CLI process
tests, and the Linux clean-host workflow already exist. Do not recreate them.

### TRUST-01: production trust activation

- **Purpose:** replace alpha trust with reviewed production trust.
- **Owns:** offline root custody, provider-backed online signing, HTTPS hosting,
  Developer ID signing, notarization, final native evidence, and operations.
- **Depends:** verified operator inputs and accepted assurance results.
- **Tests & gates:** threshold refusal, root and online-key rotation, expiry and
  refresh, immutable publication, independent remote verification, notarization,
  clean-Mac Gatekeeper, native upgrade/reboot/removal, and retained audit records.
- **Rollback:** publish a higher channel sequence with previous approved content.
  Never replay old metadata or promote the alpha root to production.

The [production runbook](../docs/ops/production-release.md) is the current input
and operations checklist. Root thresholds, custody holders, provider, domain,
and credentials need verified operator decisions. Earlier proposals are not
proof that these inputs exist. Application binaries and installer packages need
separate Apple Application and Installer identities from the same team.

The [production tasks](../openspec/changes/execute-production-trust-ceremony/tasks.md)
contain only the remaining activation work. Signing helpers and the initial
runbook already exist.

### RELEASE-NEXT: alpha.56 publication

- **Purpose:** publish PUBLIC-23 and the confirmed bug and UX fixes.
- **Owns:** version metadata, signed alpha assets, channel sequence 56, public
  installation instructions, and exact-release lifecycle evidence.
- **Depends:** reviewed source changes and passing CI for the final source.
- **Tests & gates:** signed native builds; authenticated channel and artifact
  verification; native macOS alpha.54-to-alpha.55-to-alpha.56 upgrade on the same VM;
  reboot; package workflows; removal; fresh public Linux and macOS checks.
- **Rollback:** publish a higher channel sequence with prior approved content.
  Never replay older metadata. Keep the alpha test root separate from production.

## Supported scope and deferred features

| Area | Current scope | Remaining limit |
| --- | --- | --- |
| Linux | x86-64 with systemd | No Linux arm64 release; no broad distribution or real Linux reboot claim |
| macOS | Apple Silicon | No Intel support; alpha package is not notarized |
| Public flakes | Public, locked GitHub roots on macOS | Linux needs an equivalent per-process source read boundary |
| Package sources | Catalog and supported public flakes | Private credentials, development shells, and system modules are outside current scope |
| Base Nix | Pinned Determinate install and terminal uninstall | No product-owned Base Nix repair/update or vendor-residue cleanup |

These limits are not unfinished items in the completed migration stack.
New platform or source features need a scoped plan and platform proof.

The old DN-21 through DN-32 simplification proposals do not require new delivery
PRs. Do not claim them complete without evidence. Any remaining useful work must
start from a measured problem in QUALITY-01. Keep Broker and Root Helper duties
until a replacement has passed the relevant privilege, build, and lifecycle tests.

## Accepted ownership and invariants

- Determinate owns the machine-wide Base Nix lifecycle.
- `pkg` authenticates the pinned installer executable before privilege and
  starts it by absolute path with a fixed environment.
- One supervisor drains bounded diagnostics, waits, and reaps. Vendor output is
  not a completion protocol. There is no proved safe cancellation after start.
- Persisted `Started` state means Unknown Base Nix Outcome. It fails closed.
  Only exit 0 plus installed-state validation creates Accepted Base Nix Handoff.
- `pkg` owns selectors, builds, manifests, locks, generations, activation, roots,
  package garbage collection, Package Repair, and product assets.
- An ordinary product upgrade authenticates assets and installed service files,
  stops fixed product services, commits the offline file transaction, verifies
  the result, restarts services, and checks the broker. It preserves Base Nix
  and package state. PUBLIC-02 superseded the old manual-upgrade requirement.
- `--leave-services-offline` remains the operator-controlled mode. Product Asset
  Repair also remains offline and restores authenticated same-release bytes.
  An uncertain result keeps services offline until verified recovery.
- Live system uninstall requires plain terminal output. `pkg` verifies and
  removes product-owned state, revalidates the installed vendor executable and
  receipt, consumes Accepted state, then terminally executes the vendor.
- A synchronous exec failure restores exact Accepted state under the same lock.
  A crash after state consumption can leave unmarked Nix; this fails closed.
  After exec, Determinate owns signals, status, cleanup, and residue.
- Foreign or changed state is preserved and refused. No unauthenticated reset,
  adoption, receipt reconstruction, or vendor-residue cleanup is allowed.
- The Linux `/run/pkg-install-handoff.lock` and macOS
  `/private/var/db/pkg-install-handoff.lock` are root-owned mode-0600 coordination
  files. They are not package lifecycle state.
- Broker and Root Helper retain their proved package duties. Raw Nix visibility
  and local administrator access are not product security boundaries.
- Public sources cannot add caches, trust keys, Nix settings, or root evaluation.
- Keep one vendor lifecycle engine. Do not add an installer-provider framework,
  the upstream Rust installer crate, copied upstream code, or plan JSON APIs.
- Remove dependencies only after proving use on every supported target.

The pinned executable is Determinate Nix Installer 3.22.1, revision
`4132ad07a15ee7d88c096ac7172b7afb2672866b`, under LGPL-2.1.
Its installed executable and receipt are `/nix/nix-installer` and
`/nix/receipt.json`. The [DN-03 findings](../spikes/s6-determinate-installer/FINDINGS.md)
and [ADR 0004](../docs/adr/0004-determinate-base-nix-lifecycle.md) retain the
vendor evidence and ownership decision.

## Change and proof rules

Use `feat/` branches from current `main`. Give each PR one purpose and identify
its entry above. State purpose, ownership, dependencies, tests, evidence limits,
rollback, and remaining risk. If PRs form a new stack, name the immediate parent
and restack descendants after a squash merge.

Follow [CONTRIBUTING.md](../CONTRIBUTING.md) for the E/F/A review model and gates.
Code changes require the pinned Rust 1.96.1 checks and relevant platform tests.
Documentation changes require working links and no unsupported status claims.

A platform proof records the exact source or artifact hash, host image,
architecture, commands, logs, verdicts, and retained evidence. Native macOS proof
cannot be replaced by Docker or Linux. Linux emulation is not native Linux
proof. Do not run the destructive DN-16 harness directly; use its manual
workflow and exact disposable runners. User-path VM audits are separate
regression evidence and do not certify a production release.
