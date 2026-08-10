# 11 — PR Roadmap & DAG (Foundations → Technical Preview → v1)

**Owner:** Assurance track (plans 08–12). **Status:** Draft v1 (planning only).
**Depends on:** `00`..`10` (every prior plan). **Feeds into:** `12` (spike decisions referenced here as DRs).

> **Note on module paths.** The *authoritative* module/file layout is defined in `01`
> (architecture), `04`/`05` (core/state), and `07` (platform). The crate paths below are the
> **proposed workspace** this roadmap assumes; if `01`/`04`/`07` choose different names, PR
> ownership transfers with the rename but the DAG, dependencies, and gates are unchanged.

---

## 1. Principles

1. **One purpose per PR.** A reviewer can hold the whole change in their head.
2. **Spikes before irreversible architecture.** Every risky unknown (store-prefix, TUF
   fitness, macOS binary coverage, reeval cost, sandbox/caps) is resolved by a numbered
   **spike PR** that *produces a Decision Record in `12`* before the PR that depends on it.
3. **Foundations before features; tests before trust.** No PR touches channel/keys/state/
   helper/substitute/eval-purity without a security review and the relevant test lane green.
4. **Parallelism is explicit.** PRs with no edge between them in §3 may be developed in
   parallel; the matrix in §5 says which.
5. **Every PR is reversible.** Each entry lists a rollback strategy (revert + what state is
   left behind).
6. **Small enough for serious review.** Target: a PR lands in ≤ a few hundred lines of
   *logic* (fixtures/tests excluded); anything larger is split.
7. **Privilege split is reflected in the adapter seam.** The unprivileged `NixAdapter` (broker)
   exposes **no** mutating repair / GC-root-write method; the privileged `MaintenanceAdapter`/helper
   owns `repair_store_paths` and per-output root-set writes (D-19, doc 09 §4.1.2). No PR
   adds a generic `repair(paths)` to `NixAdapter`.

---

## 2. Reviewer Model

Three area owners (people TBD; roles fixed):
- **F** = Foundations & Trust owner (`00`–`03`): architecture, channel/TUF, Nixpkgs/index, Nix adapter contract.
- **E** = Execution & Platform owner (`04`–`07`): resolve/install/build, state/locks/gen/GC, CLI/UX, installers.
- **A** = Assurance owner (`08`–`12`, this track): security, tests, release/ops, roadmap, risks.

**Rules:** every PR has a **primary** reviewer (owner of the most-touched plan), ≥1
**cross-area** reviewer (a different owner), and a **mandatory security review by A** for any
PR in the **trust surface** (channel/keys, state integrity, helper/privilege, substitute,
eval purity, uninstall, release signing). Spikes additionally require sign-off by the owner
whose plan the spike informs.

---

## 3. The PR DAG (PR-0 … PR-38)

```mermaid
flowchart TD
    P0[PR-0 repo+CI+plans]
    P1[PR-1 workspace+lint+deny]
    P2[PR-2 pkg-core domain types]
    P3[PR-3 NixAdapter trait + FakeNix + Fast CI]
    P4[PR-4 SPIKE S1 store-prefix]
    P5[PR-5 SPIKE S2 TUF/tough]
    P6[PR-6 SPIKE S4 reeval cost]
    P7[PR-7 SPIKE S3 macOS bin+sign]
    P8[PR-8 SPIKE S5 managed-daemon sandbox/approval/resource-boundary]

    P9[PR-9 detect unmanaged Nix fail-closed]
    P10[PR-10 state schema v1 + journal]
    P11[PR-11 channel descriptor + tough]
    P12[PR-12 provision managed Nix]

    P13[PR-13 fetch+verify pinned Nixpkgs]
    P14[PR-14 index build disposable]
    P15[PR-15 index query API]
    P16[PR-16 resolver Selector->evaluated plan]

    P17[PR-17 substitute + sig/NAR verify]
    P18[PR-18 GC roots + activation + current]
    P19[PR-19 install pipeline + rollback]
    P20[PR-20 remove/upgrade + mixed-rev]

    P21[PR-21 generations/history/rollback/pin]
    P22[PR-22 GC + leases]
    P23[PR-23 CLI skeleton + UX]
    P24[PR-24 CLI wire commands]
    P25[PR-25 completion + PATH/doctor]

    P26[PR-26 shared local-build engine sandbox+approval]
    P27[PR-27 Linux installer + root helper]
    P28[PR-28 macOS installer+build+launchd+notary]
    P29[PR-29 uninstall boundaries]

    P30[PR-30 two-phase repair + corruption recovery]
    P31[PR-31 security test lane + chaos]
    P32[PR-32 perf bench + budget gate]
    P33[PR-33 release signing + publish]
    P34[PR-34 observability logs/telemetry]
    P35[PR-35 docs site + install scripts + doctor]

    P36[PR-36 Tech Preview hardening + Real-Nix CI]
    P37[PR-37 v1 RC + sign-off + revoke rehearsal]
    P38[PR-38 v1.0 release]
    P39[PR-39 broker/helper contract + capability — early BLOCKING design/contract milestone (M1.5)]

    P0-->P1-->P2-->P3
    P2-->P10
    P2-->P11
    P3-->P9
    P1-->P4-->P9
    P1-->P5-->P11
    P1-->P6-->P14
    P1-->P7-->P28
    P1-->P8-->P26
    P11-->P12
    P9-->P12
    P11-->P13
    P12-->P13
    P13-->P14
    P14-->P15-->P16
    P13-->P16
    P3-->P16
    P13-->P17
    P11-->P17
    P10-->P18
    P17-->P18
    P16-->P19
    P18-->P19
    P19-->P20
    P19-->P21
    P18-->P21
    P21-->P22
    P18-->P22
    P2-->P23
    P23-->P24
    P16-->P24
    P19-->P24
    P20-->P24
    P21-->P24
    P22-->P24
    P23-->P25
    P17-->P26
    P9-->P27
    P12-->P27
    P12-->P28
    P27-->P29
    P28-->P29
    P26-->P28
    P18-->P30
    P26-->P30
    P22-->P30
    P3-->P39
    P10-->P39
    P39-->P27
    P39-->P28
    P39-->P30
    P39-->P36
    P27-->P30
    P28-->P30
    P30-->P36
    P19-->P31
    P18-->P31
    P11-->P31
    P14-->P32
    P19-->P32
    P11-->P33
    P14-->P33
    P23-->P34
    P27-->P35
    P28-->P35
    P23-->P35
    P24-->P36
    P27-->P36
    P28-->P36
    P29-->P36
    P31-->P36
    P32-->P36
    P33-->P36
    P36-->P37-->P38
```

---

## 4. PR Entries

> Fields: **Purpose · Owns · Depends · Migration/compat · Tests & gates · Demo · Reviewers ·
> Rollback · Parallel · Milestone.** "Fast" = layers 1–4(Fake)+lint (`09`). "Full" =
> nightly/release lanes.

### Milestone M0 — Foundations

#### PR-0 — Repo bootstrap, CI skeleton, plans cross-links, threat-model baseline
- **Purpose:** establish the repo, the plans directory as the source of truth, and CI that
  enforces plan cross-references and the `08` threat model as a living baseline.
- **Owns:** `.gitignore`, `README.md`, `plans/` (links only — do **not** edit others' plans),
  `.github/workflows/docs-linkcheck.yml`, `CONTRIBUTING.md` (PR rules from §1–§2).
- **Depends:** — (first PR).
- **Migration/compat:** n/a.
- **Tests & gates:** docs linkcheck green; CONTRIBUTING references the reviewer model.
- **Demo:** `act -W .github/workflows/docs-linkcheck.yml` (or CI run).
- **Reviewers:** primary A; cross F,E.
- **Rollback:** revert; repo is empty so no state impact.
- **Parallel:** no (blocks all).
- **Milestone:** M0.

#### PR-1 — Cargo workspace, toolchain, lint/deny/audit + `pkg-core` scaffold
- **Purpose:** stand up the permanent `pkg-core` crate scaffold (manifest + empty `lib.rs`)
  and the workspace/toolchain/lint/security gates that every later PR inherits. A memberless
  virtual workspace makes `cargo build`/`check`/`clippy`/`doc` fail 101 and `fmt` fail 1, so
  PR-1 must lay down a real member crate now (not an empty workspace); PR-2 still owns all
  domain types/logic/tests.
- **Owns:** `Cargo.toml` (workspace), `Cargo.lock`, `rust-toolchain.toml`, `deny.toml`,
  `rustfmt.toml`, `clippy.toml`, `.github/workflows/ci-fast.yml` (G-LINT job: `fmt`,
  `clippy -D warnings`, `doc`, `build`, `cargo deny check`, `cargo audit`), and the
  `crates/pkg-core/` scaffold (manifest + empty `lib.rs` only — **no** domain logic). The
  project license is **deferred** per DR-015: no `license` field, no `SPDX-License-Identifier`
  headers anywhere.
- **Depends:** PR-0.
- **Migration/compat:** pins MSRV (`1.96`) and the exact repo toolchain (`1.96.1`);
  documented in `CONTRIBUTING`.
- **Tests & gates:** G-LINT green on the `pkg-core` scaffold.
- **Demo:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features
  --locked -- -D warnings && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features
  --no-deps --locked && cargo build --workspace --all-targets --all-features --locked &&
  cargo deny --locked check && cargo audit`.
- **Reviewers:** primary F; cross A; (E informed).
- **Rollback:** revert.
- **Parallel:** no.
- **Milestone:** M0.

#### PR-2 — `pkg-core` domain types
- **Purpose:** the vocabulary everything else uses: user intent vs exact realization, identity,
  system triples, channel/policyVersion, version comparison.
- **Owns:** `crates/pkg-core/src/{identity.rs,selector.rs,realization.rs,channel.rs,version.rs,system.rs}`, the domain content of `crates/pkg-core/src/lib.rs` (module re-export
  tree) plus any manifest additions the modules need, and unit tests. The scaffold itself
  (manifest + empty `lib.rs`) is already landed in PR-1; PR-2 **extends** it, it does not
  re-create it.
- **Depends:** PR-1.
- **Migration/compat:** defines the (display-only) `pname@version` vs unique-identity
  distinction (`00`,`05`). No persistence yet.
- **Tests & gates:** G-UNIT; property tests for version compare + identity equality.
- **Demo:** `cargo test -p pkg-core`.
- **Reviewers:** primary F; cross E; (A informed).
- **Rollback:** revert (no consumers yet).
- **Parallel:** with PR-3's *planning* but not merge (PR-3 needs these types).
- **Milestone:** M0.

#### PR-3 — `NixAdapter` trait + JSON contract + `FakeNix` skeleton + Fast CI
- **Purpose:** the seam that lets us build/test the entire core without Real Nix.
- **Owns:** `crates/pkg-nix/src/{adapter.rs,contract.rs,error.rs}`, `crates/pkg-testkit/src/{fake_nix.rs,lib.rs}`, Fast-CI test job wiring (unit+contract+integration-stub).
- **Depends:** PR-1, PR-2.
- **Migration/compat:** defines the **JSON-only** contract (`04`) — no human-output scraping.
- **Tests & gates:** G-UNIT + G-CONTRACT (serde round-trips) on FakeNix; **negative**: the
  unprivileged `NixAdapter` exposes **no** mutating repair / GC-root-write method (those are the
  privileged `MaintenanceAdapter`/helper — doc 09 §4.1.2; AC-S21); no generic `repair(paths)`.
- **Demo:** `cargo test -p pkg-nix -p pkg-testkit`.
- **Reviewers:** primary F; cross E; **security A** (contract is a trust boundary, `08` T-DAEMON-2).
- **Rollback:** revert.
- **Parallel:** no (foundation of all Nix-touching PRs).
- **Milestone:** M0.

### Milestone M0.5 — Spikes (gated, parallel-able)

> Each spike PR contains **investigation scripts/notes + a Decision Record stub in `12`**
> (no production code merged except tiny repro helpers). Acceptance = the DR is filled with a
> concrete go/no-go and the blocking PR unblocks. Spikes are the **only** thing allowed to
> land large exploratory diffs; they are reviewed for *soundness of the conclusion*, not code.

#### PR-4 — SPIKE S1: Nix store-prefix & coexistence
- **Purpose:** determine whether a non-`/nix/store` prefix is viable and how the managed
  daemon/store can coexist with or exclude an existing unmanaged Nix. **Gates the installer
  layout (PR-27/28) and the managed-Nix provision model (PR-12).**
- **Owns:** `spikes/s1-store-prefix/` (scripts, findings.md), DR in `12` (DR-001).
- **Depends:** PR-1.
- **Migration/compat:** expected conclusion confirms the v1 "exclusive managed `/nix/store`"
  invariant (`00`,`07`) OR mandates a different layout before PR-12.
- **Tests & gates:** DR accepted by F, E, and A.
- **Demo:** `cat spikes/s1-store-prefix/findings.md`.
- **Reviewers:** primary F; cross E; **A** (security: T-INST-4).
- **Rollback:** archive spike dir.
- **Parallel:** with PR-5,6,7,8.
- **Milestone:** M0.5.

#### PR-5 — SPIKE S2: TUF/`tough` fitness
- **Purpose:** confirm real TUF via `tough` expresses our small target set (rev/narHash,
  managed-Nix version, index hashes, substituters/keys, systems, policyVersion, sequence,
  expiry) and supports threshold + revocation. **Gates PR-11.**
- **Owns:** `spikes/s2-tough/`, DR-002 in `12`.
- **Depends:** PR-1.
- **Migration/compat:** locks the channel-signing choice (`02`,`08` §7).
- **Tests & gates:** DR accepted by F and **A** (security).
- **Demo:** a tiny signed metadata set that the spike client verifies end-to-end.
- **Reviewers:** primary F; cross A (security owner); E informed.
- **Rollback:** archive spike.
- **Parallel:** with PR-4,6,7,8.
- **Milestone:** M0.5.

#### PR-6 — SPIKE S4: single-attribute reevaluation cost
- **Status:** Complete; DR-004 accepted 2026-08-09 from Complete Real Nix 2.34.8
  native macOS arm64 and Linux arm64 evidence. PR-14/16/32 S4 gates are clear;
  native x86_64 baseline expansion remains PR-32 work before GA.
- **Purpose:** measure time/memory of realizing one attribute at a pinned rev (Fake vs Real)
  to size resolve UX and the index. **Gates PR-14/16 budgets.**
- **Owns:** `spikes/s4-reeval-cost/`, DR-004 in `12`.
- **Depends:** PR-1.
- **Migration/compat:** informs `09` perf budgets and `04` resolve design.
- **Tests & gates:** DR with measured numbers accepted by E and A.
- **Demo:** `spikes/s4-reeval-cost/run.sh` output table.
- **Reviewers:** primary E; cross F; A informed.
- **Rollback:** archive spike.
- **Parallel:** with PR-4,5,7,8.
- **Milestone:** M0.5.

#### PR-7 — SPIKE S3: macOS binary coverage + Apple signing/notarization
- **Status:** Partial real evidence. S5 records the native sandboxed build; a
  reviewed 2026-08-10 S3 Detect is Complete. Broker-run cache coverage and real
  Developer-ID signing/notarization remain Pending, so DR-003 stays Proposed.
- **Purpose:** confirm Darwin binary coverage on `cache.nixos.org` for `x86_64-darwin`/`aarch64-darwin`, **and** that real macOS local builds are viable (native sandboxed build, `nixbld` build-user group / `_nixbld*` build users, Xcode/CLT toolchain availability, the honest resource boundary with **no** per-build memory/CPU/IO cap in stock Nix 2.34.8, `sandbox-fallback=false` fail-closed), and that a notarized signed installer/runtime is achievable. **Gates PR-28 and the Real-Nix macOS lane (`09`).**
- **Owns:** `spikes/s3-macos/`, DR-003 in `12`.
- **Depends:** PR-1.
- **Migration/compat:** confirms or revises the macOS build-security policy (cache coverage **and** native local-build readiness) (`07`, DR-003).
- **Tests & gates:** DR accepted by E and A.
- **Demo:** availability matrix for a fixture attr set on `aarch64-darwin`.
- **Reviewers:** primary E; cross F; **A** (security: signing).
- **Rollback:** archive spike.
- **Parallel:** with PR-4,5,6,8.
- **Milestone:** M0.5.

#### PR-8 — SPIKE S5: managed-daemon sandbox + approval + resource-boundary
- **Status:** Complete; DR-005 accepted 2026-08-09 with resource exhaustion and
  service-manager ceilings retained as disclosed residuals rather than hard-cap claims.
- **Purpose:** confirm `sandbox=true`/`sandbox-fallback=false` works with the managed daemon on **both Linux and macOS** (including Nix's macOS sandbox primitives and the `nixbld` build group / `nixbld*`/`_nixbld*` users), that we can intercept a build for preview/approval, **what resource boundary actually holds** (`max-jobs=1` bounds concurrent derivations per client/connection (so `pkg` adds a machine-global local-build admission lease across users); `timeout`/`max-silent-time`/`max-build-log-size` are daemon bounds; disk/free-space/load preflight; Nix `use-cgroups` is Linux-only process grouping/cleanup/statistics, **not** caps; service-manager ceilings have **distinct** systemd-vs-launchd semantics and are **Pending** defense-in-depth, not accepted enforcement), and that `pkg` fails closed if sandbox/build-user readiness cannot be verified. The spike must record **real managed-host behavioral evidence** (or stay `Proposed`); until then PR-26 stays gated. **Gates PR-26.**
- **Owns:** `spikes/s5-sandbox/`, DR-005 in `12`.
- **Depends:** PR-1.
- **Migration/compat:** informs `04`/`07` build/sandbox design and `08` T-BUILD-* controls.
- **Tests & gates:** DR accepted by E and **A**.
- **Demo:** a **regular** derivation build that is filesystem-sandboxed and network-denied as predicted under `sandbox=true`; a fixed-output derivation that is intentionally network-enabled (hash boundary); readiness fail-closed when sandbox/build users are unready. On Linux the spike also validates **cgroup v2 + service readiness** for the per-build grouping/cleanup/accounting provided by `use-cgroups` (confirming it writes no `memory.max`/`cpu.max`/`pids.max`/IO knobs) and that the generated `nix.conf` renders per-platform exactly (Linux: `cgroups` feature + `use-cgroups=true`; macOS: both omitted).
- **Reviewers:** primary E; cross F; **A** (security: T-BUILD-1/3).
- **Rollback:** archive spike.
- **Parallel:** with PR-4,5,6,7.
- **Milestone:** M0.5.

### Milestone M1 — Managed Nix lifecycle & state

#### PR-9 — Managed-Nix detection (fail-closed on unmanaged Nix)
- **Status:** **Completed 2026-08-09** after DR-001 acceptance. The production
  read-only detector, `pkg doctor` refusal integration, and authenticated ownership-receipt
  verifier are implemented; a real privileged macOS scan was captured and correctly remained
  `nix_ownership_unknown` because the S5 spike install has no production receipt. PR-12 now
  creates the signed asset expectation and installs the atomic receipt, binding portable signed
  group roles to host-local numeric IDs without weakening or reopening PR-9's verification
  contract.
- **Purpose:** detect any existing **unmanaged** Nix and refuse with remediation text; never
  delete it (`08` T-INST-4, G6).
- **Owns:** `crates/pkg-nix/src/managed/{detect.rs,ownership.rs}` + tests. The ownership verifier
  accepts only an independently authenticated expectation, a root-only receipt at the fixed
  platform path, and a complete static privileged-install artifact metadata/content match; a
  receipt or marker alone never establishes ownership. Dynamic store objects remain Nix/pkg state
  behind the exclusive daemon boundary rather than a frozen release-receipt inventory. The static
  artifact list is canonically encoded and SHA-256-bound to the authenticated manifest digest, so
  truncating or altering the caller-supplied list fails before host verification.
- **Depends:** PR-3, PR-4 (S1).
- **Migration/compat:** first user-facing gate; documented in `07`.
- **Tests & gates:** G-UNIT + G-INTEGRATION (Fake host with "unmanaged Nix" fixture → refuse).
- **Demo:** `pkg doctor` on a host with a stray `/nix` → refusal + remediation.
- **Reviewers:** primary E; cross F; **A** (security).
- **Rollback:** revert; detection is non-destructive.
- **Parallel:** with PR-10, PR-11.
- **Milestone:** M1.

#### PR-10 — `pkg-core` state schema v1 + migrations + journal + integrity
- **Purpose:** the authoritative state: schema-versioned, forward-only migrations, operation
  journal, tamper-evident integrity anchor to the channel (`08` §7.3, T-STATE-1).
- **Owns:** `crates/pkg-core/src/state/{mod.rs,schema.rs,migrate.rs,journal.rs,integrity.rs}` + tests + `fixtures/state-v1/`.
- **Depends:** PR-2.
- **Migration/compat:** establishes `stateVersion=1`; all later schema PRs add migrations.
- **Tests & gates:** G-UNIT + G-CONTRACT (migration idempotence, fail-closed on tamper).
- **Demo:** `cargo test -p pkg-core state::`.
- **Reviewers:** primary E; cross F; **A** (security: state integrity).
- **Rollback:** revert (no writers yet beyond tests).
- **Parallel:** with PR-9, PR-11.
- **Milestone:** M1.

#### PR-11 — `pkg-channel` descriptor + `tough` client + verify
- **Status:** **Completed 2026-08-09** after DR-002 acceptance. Production `pkg-channel`
  now loads from canonical HTTPS repository URLs with the embedded root as its only bootstrap,
  fixed `ExpirationEnforcement::Safe`, conservative metadata limits, fully drained target reads,
  and a private persistent datastore protected by a lifetime-held cross-process writer lease.
  Its public surface returns only semantically validated V1 policy: exact schema/policy support,
  descriptor expiry, sequence/policy rollback and sequence-reuse refusal, all-four-system map and
  TUF-target hash binding, native-build `allow-with-gates` or emergency `deny`, the sole V1 cache
  URL with signed cache-key rotation, and canonical Nix/Nixpkgs/index identities. The committed
  `fixtures/channel-v1/` repository and isolated S2 adversarial suite cover the crypto/TUF path;
  production tests cover semantic tampering and datastore contention. PR-12 is now unblocked.
- **Purpose:** load/verify signed channel metadata; enforce version monotonicity, expiry,
  allowed substituters/keys/systems/policyVersion (`02`,`08` §6.5, §7).
- **Owns:** `crates/pkg-channel/src/{descriptor.rs,tuf.rs,keys.rs,policy.rs}` + tests + fixture `fixtures/channel-v1/`.
- **Depends:** PR-2, PR-5 (S2).
- **Migration/compat:** defines `policyVersion=1`; CLI min-policyVersion enforced later (PR-37).
- **Tests & gates:** G-UNIT + G-CONTRACT + security-scoped tests for rollback/freeze/mix-match.
- **Demo:** `cargo test -p pkg-channel`.
- **Reviewers:** primary F; cross A; **A mandatory security** (channel = trust root).
- **Rollback:** revert; no runtime consumers yet.
- **Parallel:** with PR-9, PR-10.
- **Milestone:** M1.

#### PR-12 — Managed-Nix provisioning (fetch/verify/install)
- **Status:** **Completed 2026-08-09.** The production provisioner consumes the independently
  authenticated per-system asset-manifest target, performs a second clean-host scan immediately
  before publication, downloads with hard size limits, verifies SHA-256 before installation, and
  extracts the XZ/tar stream against the exact signed allowlist. Signed stable group roles are
  bound to host-local numeric IDs only in the authenticated receipt. Daemon readiness gates the
  receipt-last commit point; any earlier failure removes only assets created by that attempt.
  Temp-root Fake CDN/FakeNix tests cover success, archive tampering, unmanaged-Nix refusal, and
  readiness rollback. Real-Nix smoke remains PR-36; systemd/launchd transports remain PR-27/28.
- **Purpose:** download the pinned managed-Nix tarball, verify hash from signed `targets`,
  lay it down, launch the daemon; refuse if detection (PR-9) failed.
- **Owns:** `crates/pkg-nix/src/managed/{provision.rs,daemon.rs,ownership.rs}` provisioning
  extensions, authenticated runtime-manifest fields in `pkg-channel`, signed fixtures, and tests.
- **Depends:** PR-9, PR-11.
- **Migration/compat:** records the managed-Nix asset manifest (consumed by uninstall PR-29) and,
  only after successful installation, atomically writes the PR-9 ownership receipt bound to that
  signed manifest. The provisioner must not derive its trusted expectation from the local receipt.
- **Tests & gates:** G-UNIT + G-INTEGRATION (Fake CDN + FakeNix); Real-Nix smoke deferred to PR-36.
- **Demo:** provision against a fixture tarball in a temp root.
- **Reviewers:** primary E; cross F; **A** (security: T-REL-3, T-INST-1).
- **Rollback:** remove laid-down files via manifest; documented.
- **Parallel:** no (consumes 9+11).
- **Milestone:** M1.

### Milestone M1.5 — Broker/helper contract & capability (BLOCKING design/contract milestone)

> **Blocking gate.** No broker/helper/platform integration (PR-27/28), no two-phase repair
> (PR-30), and no Real-Nix execution (PR-36) merge before this milestone's accepted design lands
> (`12` DR-017). It deliberately does **not** renumber PR-0..38: PR-39 is timeline-positioned
> **early** (right after the minimum core-types/state prerequisites PR-3/PR-10, before M2) and is
> ordered by DAG edges, not by its number. It lands as a **design/contract** milestone — the ADR
> + the typed `MaintenanceAdapter` trait + the framed channels validated on FakeNix/in-process —
> so it does **not** depend on completed installers; the real OS transports (peer-auth via
> `SO_PEERCRED`/launchd-XPC, systemd/launchd units) are implemented later in PR-27/28, which is
> exactly why PR-39 must precede them rather than follow them.
>
> **Repair scope (decided — accepted).** Full privileged two-phase mutating `pkg repair` is an
> **unconditional V1 milestone** (accepted product decision; `00` D-19/INV-12, `12` DR-017): PR-30
> (Phase 0 read-only verify via broker → Phase A cache-only repair via helper → Phase B approved
> local rebuild via helper) ships in V1 and is gated by this milestone. PR-39 therefore
> **unconditionally** gates broker/helper integration (PR-27/28), two-phase repair (PR-30), and
> Real-Nix execution (PR-36); the PR-39→PR-30 and PR-36→PR-30 edges are permanent, not conditional.
> The verified **non-atomic** residual is retained as **RISK-22** (`12`).

#### PR-39 — Broker/helper framed-RPC, peer-auth, operation-lifecycle, child-containment, capability & restart-handshake design (early contract milestone)
- **Status:** **Completed 2026-08-09.** The accepted contract and reference implementation are
  recorded in `13-broker-helper-contract.md`; Linux/macOS transport bindings remain PR-27/28.
- **Purpose:** land the **detailed framed RPC + wire/capability design** that sibling docs flag as
  "the next milestone" (`01` ARCH-INV-01/05/06/07; `07` I7; `04` §5.3.1; `12` DR-017) so the
  accepted broker/helper boundary is **not left vague** before any broker/helper/platform
  integration or Real-Nix execution. Defines and specifies (with a FakeNix/in-process reference
  implementation): (a) the **CLI↔broker** closed product-framed RPC and the **broker↔helper**
  closed validated execute channel; (b) **peer authentication** (caller uid on CLI→broker;
  broker-only authenticated capability on broker→helper); (c) **operation lifecycle** (opaque
  operation handles, handle expiry, CLI-disconnect/cancel semantics); (d) **child containment**
  for the bundled `nix` CLI subprocesses the broker spawns (scrubbed env, no
  expression/substituter passthrough, ARCH-INV-02); (e) **opaque expiring single-use maintenance
  capability storage/expiry** server-side in helper/broker state (bound to uid / existing
  pkg-owned rooted generation/closure / typed `StorePath` set / `RepairBuildPlan` digest /
  policyVersion / mode; stale/replayed/mismatched/cross-UID **fail closed**; invalidated on
  helper/broker restart); (f) the **restart handshake** (broker/helper restart re-establishes
  peer auth and empties in-flight handles/admission; capabilities do not survive restart). The
  real OS credential/transport APIs (Linux `SO_PEERCRED` + systemd units; macOS launchd/XPC
  authorized-client + launchd plists) are implemented **later** in PR-27/28, not here.
- **Owns:** the framed-RPC + capability + restart-handshake design doc/ADR (closes DR-017's open
  wire item), the typed closed-grammar `MaintenanceAdapter` trait surface (`publish_root_set` /
  `remove_root_set` / `repair_store_paths`; doc 09 §4.1.2), and the broker↔CLI /
  broker↔helper framed-channel **reference implementation + tests on FakeNix / in-process** (no
  Real-Nix transport — that binding is PR-27/28). The unprivileged `NixAdapter` (PR-3) is
  confirmed to expose **no** repair / GC-root-write method; repair/root writes live only on
  `MaintenanceAdapter`/helper. It also owns the **contract + FakeNix/in-process reference
  implementation of the broker-internal, host-global admission gates** — the machine-wide **build
  admission lease** (AC-S19) and the **GC admission gate** that mints the shared **GC-inhibit
  permit** (AC-S23) — as in-memory broker state (no backing-file `flock`): the gc command (PR-22,
  reclamation logic only) and the build engine (PR-26) **integrate** these gates at runtime, repair
  (PR-30) holds its GC-inhibit permit from here, and the real broker hosts them once its transports
  land (PR-27/28). The fixed frame and token grammar is recorded in doc 13 and DR-017.
- **Depends:** PR-3 (the `NixAdapter` contract + `StorePath`/JSON surface whose privileged
  counterpart this defines — PR-3 itself depends on PR-2, so the pkg-core domain types are
  satisfied transitively), PR-10 (state schema: the generation/closure types the capability
  binds to). The broker boundary itself is accepted (CP-01.6, D-19/DR-016) — this PR formalizes
  the wire, not the boundary. It deliberately does **not** depend on PR-27/PR-28; on the contrary
  they depend on it (a contract must precede the transports that implement it — depending on the
  installers would be circular with the PR-39→PR-27/PR-28 edges).
- **Tests & gates:** G-UNIT + G-CONTRACT + G-SECURITY **on FakeNix / in-process** (Real Nix is
  not yet wired — the installers that provide the OS transports land in PR-27/28): peer-auth
  rejection of unauthenticated/impersonated callers against the in-process/framed fake;
  operation-handle lifecycle + disconnect/cancel releases admission; capability
  replay/stale/mismatch/expiry/cross-UID all **fail closed** (AC-S22); helper grammar **cannot
  widen** to raw
  path/installable/derivation/expression/flake/argv/option/substituter-key/env-override/output-selection/verb
  (AC-S22 fuzz); restart handshake empties in-flight handles + admission and refuses stale
  capabilities; **broker-internal build/GC admission gates** are in-memory only — no backing-file
  `flock` (AC-S19/S23); ADR accepted by E, F, and **A**. The matching Real-Nix peer-auth /
  transport / capability validation lands in PR-27 (Linux), PR-28 (macOS), and PR-30 (repair).
- **Demo:** against the in-process/framed fake: a closed broker→helper repair/root request
  round-trips with a fresh capability; a replayed/expired/cross-UID capability is refused;
  kill+restart the broker → in-flight ops fail, admission empties, stale capabilities rejected, a
  new op re-authenticates cleanly.
- **Reviewers:** primary E; cross F; **A mandatory security** (peer auth, capability, child
  containment — T-DAEMON-*, T-INST-7).
- **Rollback:** revert the framed-channel reference implementation; the broker/helper boundary
  stays accepted and the contract/ADR remains the spec PR-27/28 implement against.
- **Parallel:** no (it is the early gate). It may be *developed* alongside M2/M3/M4 work that
  does not depend on it, but it must land before PR-27/28/30/36.
- **Milestone:** M1.5.

### Milestone M2 — Catalog & resolve

#### PR-13 — Fetch + verify pinned Nixpkgs (rev/narHash)
- **Status:** **Completed 2026-08-09.** The closed fetch-spec/metadata-runner contract,
  top-level identity promotion, adversarial tests, and FakeNix hook are implemented in
  `pkg-nix::nixpkgs` / `pkg-testkit::FakeNixpkgsRunner`. Real contained subprocess execution
  remains PR-36.
- **Purpose:** materialize the pinned Nixpkgs rev and verify `narHash` from signed channel.
- **Owns:** `crates/pkg-nix/src/nixpkgs.rs` + tests + FakeNix hooks.
- **Depends:** PR-11, PR-12.
- **Migration/compat:** pinned rev is the catalog source of truth (`00`,`03`).
- **Tests & gates:** G-UNIT + G-INTEGRATION (narHash mismatch → refuse).
- **Demo:** realize a fixture attr's derivation path from the pinned slice.
- **Reviewers:** primary F; cross E; **A** (security: T-EVAL-1).
- **Rollback:** revert; cache is disposable.
- **Parallel:** no.
- **Milestone:** M2.

#### PR-14 — `pkg-index` disposable, deterministic build
- **Status:** **Completed 2026-08-09.** `pkg-index` now validates the bounded
  closed-schema Nix projection, normalizes and RFC 8785-canonicalizes schema-v1
  records, emits the channel-compatible exact-byte SHA-256, and rejects store-path
  leakage. The maintained `tryEval` projection lives in
  `crates/pkg-index/nix/index-meta.nix`; the tiny-slice golden digest is reproduced by
  native macOS and Linux CI jobs.
- **Purpose:** derive the search/list/info index from the pinned Nixpkgs; assert
  cross-host determinism (`08` T-IDX-3, `03`).
- **Owns:** `crates/pkg-index/src/build.rs` + tests + determinism job in CI.
- **Depends:** PR-13, PR-6 (S4 budgets).
- **Migration/compat:** index hash recorded into channel `targets` later (PR-33 publish).
- **Tests & gates:** G-UNIT + G-INTEGRATION + **determinism** (two-host identical bytes).
- **Demo:** build index for `fixtures/nixpkgs-slice-tiny`, hash stable.
- **Reviewers:** primary F; cross A; (E informed).
- **Rollback:** rebuild (disposable).
- **Parallel:** with PR-15 planning (PR-15 consumes its output).
- **Milestone:** M2.

#### PR-15 — `pkg-index` query API (search/list/info)
- **Status:** **Completed 2026-08-09.** The pure offline `IndexQuery` API provides
  bounded host-filtered ranked search, paged derived-catalog enumeration, and
  exact/alias/display-name info lookup that preserves ambiguity. Stable JSON
  goldens expose canonical copy/paste package ids, friendly platform ids, stale
  state and catalog provenance without store/derivation identities; unavailable
  advisory/size data is reported honestly rather than invented.
- **Purpose:** read-side API used by the CLI; pure over the index; no network.
- **Owns:** `crates/pkg-index/src/{query.rs,search.rs,list.rs,info.rs}` + tests.
- **Depends:** PR-14.
- **Migration/compat:** exposes `--json` shapes the CLI pins (PR-24).
- **Tests & gates:** G-UNIT + golden query outputs.
- **Demo:** `cargo test -p pkg-index`.
- **Reviewers:** primary F; cross E; A informed.
- **Rollback:** revert.
- **Parallel:** no.
- **Milestone:** M2.

#### PR-16 — `pkg-resolver` (Selector → evaluated derivation plan)
- **Status:** **Completed 2026-08-09.** A dependency-safe orchestration crate now
  resolves canonical attributes using the disposable index when its
  channel/revision/system identity matches, falls back to direct conservative
  attribute syntax when the index is missing or incomplete, and performs one
  evaluate-only adapter call against the verified source. The normalized JSON
  v4 derivation closure contains expected output paths but cannot claim they are
  realized; evaluated pname/version are authoritative and user version intent
  is enforced after evaluation.
- **Purpose:** map user intent to a deterministic evaluate-only derivation plan
  via pinned Nixpkgs under pure evaluation (`04`, `08` T-EVAL).
- **Owns:** `crates/pkg-resolver`; the evaluate-only request/report boundary in
  `crates/pkg-nix`; corresponding `pkg-testkit` transcript support.
- **Depends:** PR-3 (adapter), PR-13, PR-15.
- **Migration/compat:** this intentionally corrects the original `pkg-core`
  placement, which would create a dependency cycle (`pkg-index` and `pkg-nix`
  already depend on `pkg-core`), and preserves the intent-vs-realization
  contract by deferring `Realization` until acquisition (`00`,`05`).
- **Tests & gates:** G-UNIT + G-INTEGRATION (exact FakeNix evaluate-only call,
  missing-index direct attr, alias lookup, ambiguity, source/version mismatch,
  adapter/impure-eval failure closed and redacted).
- **Demo:** resolve `ripgrep` → exact evaluated derivation closure; no store mutation.
- **Reviewers:** primary E; cross F; **A** (security: T-EVAL-1/2).
- **Rollback:** revert.
- **Parallel:** no.
- **Milestone:** M2.

### Milestone M3 — Install / activate

#### PR-17 — Substitute from `cache.nixos.org` + signature/NAR verify
- **Status:** **Completed 2026-08-09.** Authenticated channel promotion now
  retains the fixed cache URL and public-key identity instead of discarding
  them. Fetched adapter reports require a bounded substitution-time receipt
  (source URL, NAR hash, observed signatures); normal misses carry no receipt.
  `acquire_substitute` binds URL and signature key name to channel policy,
  requires recursive read-only integrity/trust verification, and matches
  post-copy path metadata to the receipt before returning a redacted
  `VerifiedSubstitute`. Absence/no-binary remains a normal cache miss; adapter,
  trust, integrity and metadata failures fail closed.
- **Purpose:** acquire store paths only from channel-approved substituters/keys; verify
  Ed25519 sig + NAR hash (`08` T-CACHE-1/3).
- **Owns:** `crates/pkg-nix/src/substitute.rs` + tests + FakeNix fake-cache hooks.
- **Depends:** PR-13, PR-11 (keys).
- **Migration/compat:** only `cache.nixos.org` + product keys admitted (`02`).
- **Tests & gates:** G-UNIT + G-INTEGRATION + security (sig mismatch not substituted).
- **Demo:** substitute a fixture path; show verify report.
- **Reviewers:** primary E; cross F; **A mandatory security** (substitution = trust boundary).
- **Rollback:** revert; substituted paths remain in disposable store.
- **Parallel:** with PR-18 planning (PR-18 needs its output).
- **Milestone:** M3.

#### PR-18 — `pkg-store` GC roots + activation + atomic `current`
- **Status:** **Completed 2026-08-09.** `pkg-store` now derives deterministic,
  non-user-controlled per-output root names and publishes the complete set only
  through `MaintenanceAdapter`; stages a sorted Rust-only symlink forest without
  following source links; enforces abort/keep-first/keep-last collisions and
  hard file/directory conflicts; binds the forest to a recomputable digest; and
  performs the required durable ordering `rooted → retain forest → atomic
  relative current`. State paths are revalidated for ownership, writable bits,
  and symlink components before mutation. The four reachable transaction tails
  are represented by a fail-closed recovery classifier; PR-19 consumes those
  actions to write/restore snapshots and journal rows in the full install state
  machine.
- **Purpose:** product-owned GC roots (per-user, uid-scoped under
  `/nix/var/nix/gcroots/pkg/users/<uid>/`, created by the authenticated root-helper —
  D-17/ARCH-INV-06; `05` §8.3), store-relative activation symlinks, atomic current
  generation pointer, and the generation-transaction ordering in which the GC root
  is created **before** the `current` swap and the `committed` journal row is
  appended **after** (`05` §8.4; `08` T-PATH-1/2/4, T-STATE-3).
- **Owns:** `crates/pkg-store/src/{roots.rs,activate.rs,current.rs}` + tests.
- **Depends:** PR-10, PR-17.
- **Migration/compat:** Nix profile state is **not** authoritative (`05`).
- **Tests & gates:** G-UNIT + G-INTEGRATION + security (symlink/world-writable rejection) + fault: kill at each transaction state (prepared/rooted/activated/committed, `05` §8.4) recovers correctly, the GC root always precedes the `current` swap, and no crash leaves `current` unrooted.
- **Demo:** activate a fixture closure into a temp HOME; verify the GC root exists before `current` switches and the swap is atomic.
- **Reviewers:** primary E; cross F; **A mandatory security** (path/symlink hardening).
- **Rollback:** revert to previous activation via `current`.
- **Parallel:** no.
- **Milestone:** M3.

#### PR-19 — Install pipeline state machine + rollback-on-failure
- **Status:** **Completed 2026-08-09.** The dependency-safe `pkg-pipeline`
  orchestration crate now composes `pkg-resolver`, authenticated cache-only
  acquisition, verified-output staging, `pkg-store`, and the persisted-state
  contracts without making `pkg-core` depend upward on its consumers. Its
  seven-phase driver has one legal order and cleans pre-swap failures while
  distinguishing post-swap failures that must recover forward. The concrete
  generation executor validates cross-file/body/generation hashes, writes
  snapshots + sidecars before the immutable record, appends hash-chained
  prepared/rooted/activated/committed rows at their durability landmarks,
  roots before the atomic relative `current` swap, restores current views from
  snapshots, rejects symlinked journal/state files, and idempotently reconciles
  prepared/rooted/activated/committed crash tails. Cache misses return an
  explicit `BuildRequired` boundary for PR-26's approved local-build path.
- **Purpose:** resolve→preflight→acquire→verify→stage→activate→commit with the invariant
  that **failure leaves the previous generation active** (`04`,`08` T-STATE-3).
- **Owns:** `crates/pkg-pipeline/src/{lib.rs,resolve.rs,preflight.rs,acquire.rs,verify.rs,stage.rs,activate.rs,commit.rs}` + narrow read-only state accessors in `pkg-core`. The originally planned `pkg-core/src/pipeline` placement is dependency-invalid: orchestration consumes `pkg-resolver`, `pkg-nix`, and `pkg-store`, all of which already depend on `pkg-core`.
- **Depends:** PR-16, PR-18.
- **Migration/compat:** writes generations via PR-10 state.
- **Tests & gates:** G-UNIT + G-INTEGRATION + fault (kill at each transaction state — prepared/rooted/activated/committed, `05` §8.4 — → correct recovery; the previous generation stays active on any pre-swap failure).
- **Demo:** install a fixture package; kill after the `current` swap but before the `committed` row; rerun → recovery finalizes and the rooted generation N+1 is active.
- **Reviewers:** primary E; cross F; **A** (security/reliability).
- **Rollback:** revert; pipeline is idempotent + resumable.
- **Parallel:** no (core of the product).
- **Milestone:** M3.

#### PR-20 — Remove / upgrade-one / upgrade-all + mixed-revision handling
- **Status:** **Completed 2026-08-09.** `LifecycleState` now proves manifest and
  lock ownership, channel, selector-id, attribute, pin, system, and exact-rev
  coherence before any edit or generation commit. Remove validates every
  target first, then deletes manifest entries, lock entries, and the pin index
  atomically. Upgrade-one/all emits only eligible resolver requests at the
  authenticated current revision, preserves every untouched exact lock entry,
  skips pins unless explicitly bumped, binds replacements to the planned
  attribute/system/revision, and models removed-upstream refusal versus
  explicit skip without partial mutation. Candidate commits additionally bind
  generation outputs and activation roots back to the coherent lifecycle
  state. Removing the final selector activates and recovers an empty forest
  without widening the helper's nonempty root-set grammar.
- **Purpose:** lifecycle ops that edit the desired-state set; handle mixed Nixpkgs revisions
  during selective upgrades (`04`,`05` §7). Per INV-04/`00`, a generation may legitimately
  contain outputs realized from multiple Nixpkgs revisions (selectors may pin
  `channel:current` | `channel:pinned:<id>` | `rev:<gitsha>`), and **every** such rev is
  exact/pinned — never floating; `upgrade` re-resolves only the named/non-pinned selectors at
  the current `channelSeq`.
- **Owns:** `crates/pkg-core/src/{lifecycle.rs,remove.rs,upgrade.rs}` + narrow
  schema accessors, generation-binding checks in `pkg-pipeline`, empty-state
  activation support in `pkg-store`, and tests.
- **Depends:** PR-19.
- **Migration/compat:** defines the upgrade semantics users see (`06`).
- **Tests & gates:** G-UNIT + G-INTEGRATION + golden desired-state diffs.
- **Demo:** upgrade one package while others stay pinned.
- **Reviewers:** primary E; cross F; A informed.
- **Rollback:** revert; ops are generation-based.
- **Parallel:** with PR-21 (both consume PR-19).
- **Milestone:** M3.

### Milestone M4 — Generations & UX

#### PR-21 — Generations / history / rollback / pin-unpin
- **Status:** **Completed 2026-08-09.** Immutable `GenerationSnapshot` values now
  prove every generation/manifest/lock binding; history is numeric, sanitized,
  and diffable; pin/unpin edits desired state atomically. Rollback selection
  verifies retained ownership, platform, and channel-derived runtime
  compatibility. Pipeline preparation rejects stale plans and reused generation
  ids, re-stages the target outputs into a fresh forest, creates fresh snapshots,
  record, and root set, then uses the PR-19 crash-safe activation transaction.
- **Purpose:** surface and roll back generations; pinning expresses user intent (`05`,`06`).
- **Owns:** `crates/pkg-core/src/{generation.rs,history.rs,pin.rs}` and
  `crates/pkg-pipeline/src/rollback.rs` + tests.
- **Depends:** PR-10, PR-19.
- **Migration/compat:** rollback stays within managed-Nix compat window (`10` §6).
- **Tests & gates:** G-UNIT + G-INTEGRATION (rollback restores prior activation).
- **Demo:** install → install → `history` → `rollback`.
- **Reviewers:** primary E; cross F; A informed.
- **Rollback:** revert.
- **Parallel:** with PR-20, PR-22 planning.
- **Milestone:** M4.

#### PR-22 — GC + leases
- **Status:** **Completed 2026-08-09.** `StateLease` uses kernel-backed
  nonblocking shared/exclusive file locks with fail-closed owner/mode/symlink
  checks; generation transactions now own an exclusive lease through commit.
  Retention planning protects active, newest-count, and age-window generations;
  pruning journals intent, durably deletes only validated user metadata,
  removes privileged roots last, and is restart-idempotent. FakeNix integration
  verifies the global collector runs once after pruning, while the broker's
  already-landed PR-39 admission gate remains the runtime wrapper boundary.
- **Purpose:** collect unreferenced store paths using product-owned roots + generation
  manifest; the per-user **state-mutation lease** (a filesystem `flock`, `05` §12) that serializes
  a user's state writes and `gc` for that user (`05`,`08` T-STATE-4, T-CONC-1).
- **Owns:** `crates/pkg-store/src/{gc.rs,leases.rs,journal.rs}` plus the
  `pkg-pipeline` lease integration + tests. `gc.rs` = reclamation logic that
  consults generations/roots (via PR-18/PR-21); `leases.rs` = the per-user state-mutation lease
  (a filesystem `flock`). This PR does **not** own the **broker-internal, host-global GC admission
  gate** — that is an in-memory broker-hosted gate whose **contract + FakeNix/in-process reference
  implementation is owned by PR-39** (M1.5; AC-S19/S23), hosted at runtime by the broker (PR-27/28
  transports). PR-22 is **M4 and runs in-process against FakeNix (no real broker yet)**, so it
  acquires no broker gate here — the broker wraps `pkg gc` with that gate once the broker exists;
  an M4 PR cannot supply a broker-hosted gate.
- **Depends:** PR-18, PR-21.
- **Migration/compat:** GC must consult generations, not only roots.
- **Tests & gates:** G-UNIT + G-INTEGRATION (GC never breaks an active generation) on FakeNix
  in-process.
- **Demo:** `gc` reclaims space; active rollback still works.
- **Reviewers:** primary E; cross F; **A** (security/reliability).
- **Rollback:** revert.
- **Parallel:** with PR-21.
- **Milestone:** M4.

#### PR-23 — `pkg-cli` clap skeleton + UX/progress/TTY/exit-codes
- **Status:** **Completed 2026-08-09.** The complete derive-based V1 grammar,
  stable exit codes, human/JSON/JSONL error envelopes, public progress schema,
  TTY policy, and parser/contract tests landed in `pkg-cli`.
- **Purpose:** the command shell: clap structure, progress events, TTY detection, stable exit
  codes, `--json` harness (`06`).
- **Owns:** `crates/pkg-cli/src/{main.rs,cli.rs,ux.rs,progress.rs,exit.rs}` + tests.
- **Depends:** PR-2.
- **Migration/compat:** defines exit-code & `--json` contracts other PRs fill.
- **Tests & gates:** G-UNIT + G-CONTRACT (JSON schema presence).
- **Demo:** `pkg --help` with all command stubs.
- **Reviewers:** primary E; cross F; A informed.
- **Rollback:** revert.
- **Parallel:** yes (only depends on PR-2) — can start in M1/M2.
- **Milestone:** M4.

#### PR-24 — Wire all CLI commands to core
- **Status:** **Completed 2026-08-09.** Every non-bootstrap verb now crosses a
  typed `CommandRequest` → `CoreOperations` boundary with global confirmation
  and dry-run policy preserved after parsing. Success output is bounded and
  recursively rejects reserved fields, raw store/derivation/flake identities,
  Nix system triples, and trust-control names before human/JSON/JSONL rendering.
  Offline verified-index `search`/default `info`, active-state `list`, atomic
  remove/pin/unpin edits, and history views use the real core APIs. The missing
  atomic install-state editor and resolved+verified pipeline binder were added
  so the install path no longer needs ad-hoc JSON assembly. The E2E-Fake suite
  routes every verb, asserts all seven install phases, exact FakeNix calls,
  output redaction, and policy propagation. The shipped binary deliberately
  uses `UnavailableEngine` until PR-36 supplies the authenticated private
  broker connector; PR-24 does not bypass that boundary or claim PR-30 repair
  execution exists early.
- **Purpose:** connect `doctor, search, info, install, remove, list, outdated, update,
  upgrade, pin/unpin, history, rollback, gc, repair, completion` to PR-15..22.
- **Owns:** `crates/pkg-cli/src/commands/*.rs` + e2e-Fake suite, plus the narrow
  missing `pkg-core::install` lifecycle editor and
  `pkg-pipeline::assemble_install_state` binding required by real install
  orchestration.
- **Depends:** PR-16, PR-19, PR-20, PR-21, PR-22, PR-23.
- **Migration/compat:** first user-complete surface; `06` copy/honesty applied.
- **Tests & gates:** G-UNIT + G-INTEGRATION + **G-E2E-FAKE** (every command).
- **Demo:** full `install`→`list`→`rollback`→`gc` happy path on Fake Nix.
- **Reviewers:** primary E; cross F; A (honesty copy, T-RUN-1 disclosure).
- **Rollback:** revert (core intact).
- **Parallel:** no (integration apex).
- **Milestone:** M4.

#### PR-25 — Shell completion + PATH integration + `doctor`
- **Status:** **Completed 2026-08-09.** Static completion generation for all
  supported shells, idempotent managed PATH snippets, shadowing detection, and
  a versioned fail-closed doctor report are implemented and tested.
- **Purpose:** completions for supported shells; PATH snippet detection; `doctor` health
  checks (`06`,`07`).
- **Owns:** `crates/pkg-cli/src/completion.rs`, `crates/pkg-cli/src/commands/doctor.rs` + tests.
- **Depends:** PR-23.
- **Migration/compat:** PATH edits via managed snippet file (`08` T-UNINST-3).
- **Tests & gates:** G-UNIT + G-INTEGRATION (doctor detects shadowing/permissions).
- **Demo:** `pkg completion bash` + `pkg doctor`.
- **Reviewers:** primary E; cross F; A informed.
- **Rollback:** revert.
- **Parallel:** with PR-26..29.
- **Milestone:** M4.

### Milestone M5 — Local builds & platform installers

#### PR-26 — Shared local-build engine & policy (managed-daemon sandbox + approval + resource-boundary)
- **Status:** **Completed 2026-08-10.** The shared native Linux/macOS engine now
  provides a private canonical approval plan, sanitized volatile preview,
  single-use journaled approval, fair machine-global admission, admission-time
  replan/resource checks, typed derived-output targets, actual per-output
  provenance, and exact fail-closed managed Nix settings.
- **Purpose:** the **explicit**, non-default, cross-platform local-build path: deterministic closure/derivation preview (with target system + sandbox status + fixed-output label), managed-daemon sandbox (`sandbox=true`/`sandbox-fallback=false`; Nix's own Linux namespace/chroot sandbox, not bubblewrap; regular-derivation network denial; fixed-output network-enabled with hash boundary), the **honest resource boundary** (`max-jobs=1` bounds concurrency per client/connection, supplemented by a **machine-global local-build admission lease** across users — a second op waits/cancels, then revalidates approval/readiness once it acquires the lease; immediately before local-build execution `pkg` acquires the lease, recomputes the exact derivation/readiness `BuildPlan` and compares its digest to the approved one, then re-measures disk/free-space/load outside the digest; `timeout`/`max-silent-time`/`max-build-log-size` daemon bounds; disk/free-space/load preflight; Nix `use-cgroups` Linux process grouping/cleanup/accounting; service-manager ceilings Pending defense-in-depth — **no** stock per-build memory/CPU/IO cap, so resource exhaustion stays a disclosed residual RISK-07), **evaluation/planning that never realizes outputs** (`nix derivation show --recursive` of the exact pinned installable with import-from-derivation disabled; `nix build` begins only at acquire — pure substitution first with `max-jobs=0`, then an approved local build), build-user readiness verification with fail-closed, **single-operation** approval journaling (bound to the canonical `BuildPlan` digest + policy version; `--yes` pre-approves that one op; no `PKG_YES_TO_BUILDS`/session skip), and `ACQUIRE_NO_BINARY` for impossible/disallowed builds (`08` T-BUILD-*, `04`). **The S5/DR-005 managed-host evidence gate is accepted**; remaining launchd/systemd integration evidence belongs to PR-27/28. The engine is shared by Linux and macOS; macOS-specific wiring/validation lands in PR-28.
- **Owns:** `crates/pkg-nix/src/build.rs` + tests.
- **Depends:** PR-17, PR-8 (S5).
- **Migration/compat:** implements the cross-platform D-11 (`00`,`07`); macOS validation/integration completes in PR-28.
- **Tests & gates:** G-UNIT + G-INTEGRATION + security (approval is non-default and single-operation; sandbox on; `sandbox-fallback=false` fail-closed; disallowed build → `ACQUIRE_NO_BINARY`). Cross-referenced AC-S14–S17 (`09` §6.6): exact per-platform `nix.conf` rendering (Linux cgroups feature+setting present; macOS omits both) and finite defaults; canonical `BuildPlan` determinism + mutation-invalidation; approval journal `source` + no persistence beyond one op; launchd-vs-systemd semantics documented as Pending.
- **Demo:** cache-miss → preview (target system + sandbox + fixed-output label) → explicit single-operation approval → sandboxed native build.
- **Reviewers:** primary E; cross F; **A mandatory security** (T-BUILD-1/3).
- **Rollback:** revert; users fall back to substitution.
- **Parallel:** with PR-27/28/29.
- **Milestone:** M5.

#### PR-27 — Linux installer + root helper (polkit/socket-cred; implements the PR-39 contract)
- **Status:** **Completed 2026-08-10.** The Linux integration crate now provides
  a privileged production-host preflight contract, exact authenticated install
  assets with failure-atomic rollback ordering, validated systemd/tmpfiles
  definitions, kernel-derived `SO_PEERCRED` authentication on both RPC hops,
  bounded strict frames, serialized PR-39 capability/root transactions, and
  crash-durable atomic per-user GC-root sets. Linux container validation covers
  peer auth, concurrency/restart behavior, systemd unit verification, and `/run`
  ownership recreation; release packaging remains PR-33/35.
- **Purpose:** install the product + managed Nix with a least-privilege privileged helper that
  implements the broker↔helper contract (PR-39) on Linux — the real framed-RPC **transport** (Unix
  socket + length-prefixed frames), the real **peer-auth** (caller uid via socket credentials /
  `SO_PEERCRED`), the real **capability transport**, and the **systemd** service definitions for
  the broker and helper (`07`,`08` T-INST-1/2/3/5). The OS credential/transport APIs are implemented
  **here**, not in PR-39.
- **Owns:** `crates/pkg-installer/src/{installer.rs,helper.rs,assets.rs,platform/linux.rs}`
  (including the Linux transport binding of PR-39's `MaintenanceAdapter` + peer-auth + capability
  transport and the generated systemd units) + privileged-VM tests.
- **Depends:** PR-9, PR-12, PR-4 (S1), **PR-39 (the contract/core/fake this transport implements)**.
- **Migration/compat:** records the install asset manifest (PR-29 consumes).
- **Tests & gates:** G-UNIT + privileged-VM integration (helper auth, allowlist, fail-closed).
- **Demo:** install in an ephemeral VM → `pkg doctor` clean.
- **Reviewers:** primary E; cross F; **A mandatory security** (privilege).
- **Rollback:** uninstall path (PR-29).
- **Parallel:** with PR-28 (different OS), PR-26, PR-25.
- **Milestone:** M5.

#### PR-28 — macOS installer + Darwin build integration + launchd + signing/notarization (implements the PR-39 contract)
- **Status:** **Implementation complete 2026-08-10; external evidence gate remains open.**
  Rust/macOS tests cover `getpeereid`, strict framed transports, durable helper
  restart behavior, failure-atomic encrypted-APFS/store installation contracts,
  exact 32-user/readiness gates, valid launchd plists, and a closed product-only
  signing/notarization plan. S5 already supplies native sandbox/build evidence;
  the refreshed S3 Detect is Complete but found zero Developer ID identities.
  Therefore PR-28 is **not marked merge-complete** until a Complete broker-run
  cache Preflight and real Developer-ID/notarization validation are recorded.
- **Purpose:** macOS launchd-based privileged setup + authorized-client auth + notarized/signed installer/runtime, **and** integration/validation of the shared local-build engine (PR-26) on Darwin: `nixbld` build-user group / `_nixbld*` build users, Nix macOS sandbox under `sandbox=true`/`sandbox-fallback=false` with fail-closed readiness checks, native toolchain (Xcode/CLT) verification, and the honest resource boundary (**no** per-build memory/CPU/IO cap in stock Nix 2.34.8) (`07`, DR-003 from S3). It implements the broker↔helper contract (PR-39) on macOS — the real framed-RPC **transport**, **peer-auth** (caller uid via `getpeereid` on launchd-managed Unix sockets), **capability transport**, and the **launchd** service definitions for broker and helper. The OS credential/transport APIs are implemented **here**, not in PR-39. Installer/runtime codesigning & notarization remain **separate** from building Nix packages — local Nix outputs are not individually Apple-notarized.
- **Owns:** `crates/pkg-installer/src/platform/macos.rs` (including the macOS transport binding of PR-39's `MaintenanceAdapter` + peer-auth + capability transport and the generated launchd plists), `_nixbld` build-user provisioning, notarization tooling, packaging.
- **Depends:** PR-12, PR-7 (S3), PR-26 (shared engine), **PR-39 (the contract/core/fake this transport implements)**.
- **Migration/compat:** macOS supports approved native sandboxed local builds (D-11); not binary-only.
- **Tests & gates:** G-UNIT + macOS runner integration: cache hit, cache-miss preview/cancel/approval, successful native sandboxed build, sandbox-unavailable fail-closed, unsupported-package `ACQUIRE_NO_BINARY`, receipt/rollback.
- **Demo:** install on macOS arm64 → `pkg doctor` clean; cache-miss → approved native sandboxed build → `pkg history`.
- **Reviewers:** primary E; cross F; **A mandatory security** (signing/notarization).
- **Rollback:** uninstall path.
- **Parallel:** with PR-27 (different OS).
- **Milestone:** M5.

#### PR-29 — Uninstall boundaries (asset manifest; never touch unmanaged Nix)
- **Status (2026-08-10):** implementation complete locally; signed commit and external
  privileged install→uninstall evidence gate pending.
- **Purpose:** remove only recorded product assets; dry-run preview; refuse to touch
  unmanaged Nix; verify zero privileged residue (`08` T-UNINST-1/2/3).
- **Owns:** `crates/pkg-installer/src/uninstall.rs` + tests.
- **Depends:** PR-27, PR-28.
- **Migration/compat:** authoritative asset manifests from PR-12/27/28.
- **Tests & gates:** G-UNIT + e2e (install→uninstall→`doctor --post-uninstall`).
- **Demo:** `pkg uninstall --dry-run` then real uninstall on a VM.
- **Reviewers:** primary E; cross F; **A mandatory security** (T-UNINST-*).
- **Rollback:** uninstall is itself the rollback; reinstall restores.
- **Parallel:** no.
- **Milestone:** M5.

### Milestone M6 — Hardening & operations

#### PR-30 — Two-phase `pkg repair` + corruption recovery (privilege-split; non-atomic)
- **Status (2026-08-10):** Phase-0 exact-closure verification, missing-path contract,
  capability-driven Phase A/B coordinator, broker admission integration, final-verify gate,
  scope-bound single-use build approval, checked admission cleanup, validated per-path journal
  state machine, clean-after-crash reconciliation, and cache-only/fresh-approval restart policy are
  implemented and locally evidence-gated. Workspace tests/lints/docs, docs link checks,
  dependency policy/audit, Linux container tests, and independent P1 review pass. Real-Nix 2.34.8
  Linux/macOS fault evidence and the PR-36 production broker connector remain external merge
  gates; no Real-Nix completion claim is made here.
- **Purpose:** `pkg repair` is **explicitly user-initiated** and **verified non-atomic** (`00`
  D-19/INV-12; `05` §10; `08` T-CACHE-3/T-INST-7). It re-verifies integrity and restores it across
  the broker/helper privilege split: (Phase 0) **read-only** `nix store verify --recursive` (**no**
  `--repair`) via the **unprivileged broker** `NixAdapter` computes the damage set from broker-held
  generation state; (Phase A) **cache-only** `nix store repair` via the **privileged helper**
  `MaintenanceAdapter` with managed pinned substituters/keys, `max-jobs=0`, `builders` empty — auto
  on a signed cache hit and **must stop before any build** on a cache miss; (Phase B) an **approved
  local rebuild** via the helper, bounded nonzero `max-jobs`, `builders` empty, serialized by the
  broker's machine-wide build mutex and holding a shared GC-inhibit permit, using the ordinary
  public build preview / explicit single-operation approval whose internal
  `RepairBuildPlan`/digest covers every output Nix may rebuild. A path is marked repaired **only
  after a fresh read-only verify**. `pkg repair` warns affected commands may be temporarily
  unavailable, journals per path, auto-resumes **only** cache repair after a crash, and requires
  **fresh approval** before repeating a local repair build (the `mode=build` capability is
  single-use and invalidated on restart).
- **Owns (adapter split — no generic `repair(paths)` on `NixAdapter`):** broker-side read-only
  `crates/pkg-nix/src/verify.rs` (NixAdapter: verify **only**; **no** repair method) + broker-side
  capability issuance (opaque expiring single-use, server-side bound to uid / rooted generation /
  typed `StorePath` set / `RepairBuildPlan` digest / policyVersion / mode); the **privileged**
  repair execution on `MaintenanceAdapter`/helper — `repair_store_paths` (nonempty sorted
  validated typed `StorePath` set) — lives with the helper in `crates/pkg-installer/` (PR-39
  contract trait; PR-27/PR-28 platform transports); and `crates/pkg-cli/src/commands/repair.rs`
  + tests. Raw Nix logs stay
  **service-private**; only sanitized per-path outcome/versioned events reach the CLI/public logs.
- **Depends:** PR-18 (per-output roots + re-root), PR-22 (GC reclamation logic + the per-user
  state-mutation lease that repair coordinates with — **not** the broker gate), PR-26 (the shared
  build engine: build preview/approval + the Phase-B repair-build path that reuses it; transitively
  S5/PR-8), **PR-39** (the early M1.5 contract/core/fake: capability issuance + framed channel +
  restart handshake + the **broker-internal in-memory admission gates** — the machine-wide **build
  admission lease** (AC-S19) the Phase-B repair build waits on, and the **GC admission gate** that
  mints the shared **GC-inhibit permit** (AC-S23) repair holds so a concurrent `gc` cannot reap
  mid-repair; these are broker-hosted and owned here, **not** by PR-22/PR-26/PR-18), **PR-27** (the
  Linux transport/helper that implements the PR-39 contract), **PR-28** (the macOS transport/helper
  that implements the PR-39 contract). *(PR-39 now precedes the helpers (M1.5 ≪ M5), so both platform
  helpers are listed explicitly; repair must run on Real Nix on both Linux and macOS. Full
  two-phase mutating repair is an unconditional V1 milestone (M1.5 repair-scope note), so the
  PR-39→PR-30 edge and these helper deps are permanent; PR-39's ownership of the broker-internal
  admission gates is unchanged.)*
- **Tests & gates:** G-UNIT + G-INTEGRATION + G-SECURITY + G-FAULT on **Real Nix 2.34.8 on Linux
  x86_64 and macOS arm64** (the repair privilege split is a Nix-2.34.8 trusted-user fact — it
  cannot be asserted on Fake Nix alone). Required real-Nix cases (cross-ref `09` AC-S*):
  (a) **socket access** — an ordinary non-broker uid cannot `connect()` the daemon socket or exec
  the bundled `nix`/helper (AC-S24);
  (b) **cache repair** — corrupt a fixture output → Phase-0 verify detects → Phase-A cache-only
  repair restores from `cache.nixos.org` with `max-jobs=0`/`builders` empty (AC-S3);
  (c) **cache-miss stop-before-build** — a damaged path with no substitute and a valid deriver
  **stops** at Phase A and does not build (AC-S14);
  (d) **approval fallback** — Phase-B build preview shows the full `RepairBuildPlan` (every
  rebuildable output) → explicit single-operation approval → bounded-nonzero-`max-jobs` local
  rebuild (AC-S15/S16);
  (e) **replay / cross-UID rejection** — a stale/replayed/mismatched/cross-UID capability is
  refused (SECURITY logged) and the helper grammar cannot widen to raw
  path/installable/derivation/expression/flake/argv/option/substituter-key/verb (AC-S22);
  (f) **crash between deletion/replacement** — kill mid cache-repair (path deleted, not yet
  restored) and mid local-repair (old moved aside, not yet replaced) → recovery re-runs read-only
  verify, **idempotently resumes cache-only repair per path**, and **does not silently resume an
  approved build** (marks it needing re-approval) (AC-S5, `05` §10.8);
  (g) **final verify** — no path is marked `repaired` until a fresh read-only `nix store verify`
  confirms it clean (AC-S14);
  (h) **no raw-log leak** — the raw broker/Nix subprocess log never appears in
  `--json`/`--jsonl`/public `<user-state>/logs/*.ndjson` (AC-S25).
  Negative parity (AC-S21): the broker `NixAdapter` issuing any `--repair`-shaped command is
  **denied** by Nix 2.34.8.
- **Demo:** on Real Nix (Linux + macOS): corrupt one output → `pkg repair` → Phase-0 verify →
  Phase-A cache restore → final verify clean; then force a cache-miss → stop-before-build →
  approve → Phase-B rebuild → final verify; replay a used capability → refused; kill between
  delete/restore → recovery resumes cache repair only; grep public logs → no raw Nix log.
- **Reviewers:** primary E; cross F; **A mandatory security** (T-CACHE-3/T-INST-7 privilege split +
  capability + non-atomic residual RISK-22).
- **Rollback:** revert; repair never deletes user state (worst case it reports paths it cannot
  re-acquire and exits non-zero).
- **Parallel:** within the M6 batch with PR-31..35 **but cannot start until PR-39 (M1.5) and both
  platform helpers (PR-27/PR-28) land**.
- **Milestone:** M6.

#### PR-31 — Security test lane + fault-injection harness
- **Status (2026-08-10):** the closed AC-S1..S10 manifest/runner, offline/no-shell enforcement,
  dependency deny/audit job, non-loopback egress barrier, deterministic process-checkpoint chaos
  harness, loopback exact-transcript HTTP drop/truncation fixture, and Linux/macOS nightly harness
  jobs are implemented and locally evidence-gated. The exact AC-S1..S10 lane, full workspace
  tests/lints/docs, dependency audit, Linux `--network none` testkit run, workflow contract tests,
  and independent P1 review pass. The workflow intentionally does not claim the PR-36-owned
  authenticated Real-Nix connector/platform lane.
- **Purpose:** the `09` security lane (AC-S1..S10) + `pkg-testkit::chaos` + nightly Full job.
- **Owns:** `crates/pkg-testkit/src/{chaos.rs,http.rs}`, `tests/security/`, CI `security.yml` + `nightly.yml`.
- **Depends:** PR-11, PR-18, PR-19.
- **Tests & gates:** the lane **is** the gate (G-SECURITY + G-FAULT).
- **Demo:** run the security lane in CI on a fixture channel/cache.
- **Reviewers:** primary A; cross F,E (this is the Assurance owner's lane; F/E consult on fixtures).
- **Rollback:** revert.
- **Parallel:** with PR-30,32,33,34,35.
- **Milestone:** M6.

#### PR-32 — Performance bench lane + budget gate
- **Status (2026-08-10):** complete for the fixed native arm64 reference host. Criterion
  measures tiny-index build, fixture search, and Fake-style info; a closed-schema Python
  gate rejects incomplete/foreign Criterion output, non-native or mismatched-runner
  provenance, any absolute ceiling failure, and any regression above 25%. Native Darwin
  and native-container Linux baselines are pinned to the named Apple M4 self-hosted runner,
  and the post-merge/manual workflow pins checkout, Rust, and the Linux container digest;
  pull-request revisions cannot execute on that persistent host or its Docker daemon.
  Real-Nix budgets remain honestly `pending-pr36`; native x86_64 baseline expansion remains
  required before GA, and QEMU is not accepted as release evidence.
- **Purpose:** `criterion` benches + budget regression gate (`09` §6.7, DR-004 budgets).
- **Owns:** `benches/`, CI perf job, baseline pinning.
- **Depends:** PR-14, PR-19.
- **Tests & gates:** G-PERF.
- **Demo:** `cargo bench` vs. pinned baseline.
- **Reviewers:** primary A; cross E (budgets from `04`).
- **Rollback:** relax/re-baseline only with sign-off (not silently).
- **Parallel:** with PR-30,31,33,34,35.
- **Milestone:** M6.

#### PR-33 — Release signing (offline root + threshold) + channel/index publish
- **Status (2026-08-10):** provider-neutral release trust boundary implemented. A closed manifest
  requires the exact V1 TUF target set, three separate non-TUF CLI/Sigstore artifacts, exact
  bytes, and distinct authenticated release/security approvals. A provider authority lease
  reserves the authoritative next sequence and supplies the authenticated signing identity;
  `pkg-release` consumes signed offline-root metadata, signs only online roles through
  `tough::KeySource`, binds hashes to the reviewed manifest, rehashes before signing, writes
  immutable consistent-snapshot output, verifies it with the real client, seals publication bytes
  into anonymous read-only files, records a mandatory create-only audit event, and coordinates
  resumable ensure/remote-verify across both destinations, then activates GitHub source-of-truth
  before the CDN mirror. Root, channel, and timestamp versions are independent; a separately
  authorized timestamp-only refresh
  atomically advances the stable route before its 48-hour expiry. The CI dry
  run generates memory-only keys, proves a 2-of-3 root and independent online roles, and loads and
  drains the result with the real `tough` client. No production local-key adapter or cloud secret
  is present. KMS/HSM provider, release-authority and destination adapters, protected environments,
  and the real custodian roster remain deployment configuration and require key-custodian sign-off;
  the repository does not claim that a live channel was published.
  Remote publication accepts only an atomically persisted transaction containing exact blobs,
  hashes, and opaque lease id; the external authority binds the transaction digest, and restart
  recovery rehashes the blobs and reacquires only that exact lease/digest pair.
  Root expiry must cover every issued child-metadata expiry.
  The reviewed manifest and timestamp authority bind the exact current trusted-root digest;
  arbitrary self-signed roots are refused, and a future rotation must provide the full update chain.
  Sealed metadata is freshness-checked before upload and activation with a one-hour safety margin;
  expired recovery transactions are refused rather than made discoverable.
  Exact authoritative activation is remotely queryable, so an aged retry can finish only the
  already-active digest's mirror and authority reconciliation without activating a stale newcomer.
- **Purpose:** the release-service that signs TUF metadata and publishes the channel (TUF),
  index, and managed-Nix runtime to GitHub Releases + CDN; the CLI is published alongside with
  Sigstore attestation + pinned checksum (it is **not** a TUF channel target — `10` §2,
  doc 02 §6.4). (`10` §2–§4.)
- **Owns:** `tools/release/`, signing workflow `release.yml`, publish scripts, audit logging.
- **Depends:** PR-11, PR-14.
- **Tests & gates:** dry-run sign on test keys; 2-person approval enforced; audit log present.
- **Demo:** sign a fixture channel with test keys; client verifies.
- **Reviewers:** primary A; cross F; **E informed**; **mandatory security review + key-custodian sign-off**.
- **Rollback:** a published channel can be rolled back via higher-sequence republish (`10` §5.3).
- **Parallel:** with PR-30,31,32,34,35.
- **Milestone:** M6.

#### PR-34 — Observability: structured logs, redactor, opt-in telemetry, crash record
- **Status (2026-08-10):** complete in signed commit `57ec5b4` and revalidated at current head.
  Logs are private, bounded, rotating, allowlisted JSON; arbitrary detail crosses a bounded
  denylist/control-character redactor. Local aggregate telemetry is disabled by default and its
  typed schema cannot carry package names, paths, or arguments. Panic handling emits only the
  allowlisted CLI version, coarse phase, channel sequence, and validated opaque operation id—no
  payload, environment, backtrace, or memory dump. Main-command logging records only the static
  command name and exit status. Unit/integration tests, AC-S9, secret scan, and independent P1
  review are clean.
- **Purpose:** the `08` T-LOG-* controls and `10` §7 observability.
- **Owns:** `crates/pkg-cli/src/{log.rs,telemetry.rs,crash.rs}`, redactor unit tests + golden.
- **Depends:** PR-23.
- **Tests & gates:** G-UNIT + G-SECURITY (redactor golden, no env/args).
- **Demo:** run a command; inspect redacted log + telemetry toggle.
- **Reviewers:** primary A; cross E; **A security** (T-LOG-1/2/3).
- **Rollback:** revert.
- **Parallel:** with PR-30,31,32,33,35.
- **Milestone:** M6.

#### PR-35 — Docs site + install scripts + `doctor` support export
- **Status (2026-08-10):** implementation complete locally. The docs site covers safe installation,
  daily commands, support, privacy, and the unpublished-release boundary. The POSIX installer
  template accepts no caller URL/checksum/target, requires HTTPS redirects, verifies an embedded
  per-platform SHA-256 before privilege, and refuses before network access while any release token
  is unresolved. `doctor --support` is a preview-only typed JSON projection that succeeds on an
  unhealthy host, reads bounded private logs into phase/outcome only, reports aggregate state
  health, and excludes args/env/package names/paths/contents/raw details/Nix identities. Authenticated
  channel/runtime/index observations are explicitly null/deferred until wired. Docs/link/script,
  CLI unit/integration, redaction, and output-mode tests pass. PR-36 must render the release template
  from its final signed installer artifacts; no unpublished installer or checksum is fabricated.
- **Purpose:** user-facing docs, install instructions (pinned checksums), and `doctor
  --support` previewable export (`06`,`10` §9).
- **Owns:** `docs/`, `docs/install.sh` (pinned), support-export code in `pkg-cli`.
- **Depends:** PR-23, PR-27, PR-28.
- **Tests & gates:** docs build + support-export redaction tests.
- **Demo:** `pkg doctor --support` preview.
- **Reviewers:** primary A; cross E; A privacy review.
- **Rollback:** revert docs; support export opt-in.
- **Parallel:** with PR-30..34.
- **Milestone:** M6.

### Milestone M7 — Technical Preview

#### PR-36 — Technical-preview hardening + Real-Nix nightly CI + e2e parity
- **Status (2026-08-10):** in progress. The first production Real-Nix connector slice is landed:
  the pinned Nix 2.34.8 adapter executes version/eval/substitute/path-info/verify/build/GC through
  the managed daemon with bounded process-group cancellation, strict JSON parsing, and provenance
  checks, and its isolated Linux daemon smoke covers both substitution and local build paths. The
  follow-up runtime-alignment slice makes Linux systemd assets, macOS launchd assets, installer
  paths, and the broker child environment agree on the canonical managed configuration, daemon
  socket, state directory, and private HOME/TMPDIR. Exact systemd, tmpfiles, and plist validation,
  affected tests, workspace diagnostics/Clippy, and independent P1 review pass. The helper's
  capability layer now delegates repair only through a typed verified-scope executor, reconstructs
  results from trusted paths, rejects cardinality drift, and consumes failed capabilities; the
  former in-memory success synthesis is isolated as an explicit reference backend rather than an
  implicit production behavior. The next slice adds the real root repair executor and an isolated
  Nix 2.34.8 regression: daemon repair is unsupported even for root, so the helper fixes
  `--store local`; cache-only repair of an intentionally uncached corrupt derivation returns
  `CacheMiss` without building, and a separately approved build-mode capability restores it.
  Exact argv/unit tests prove fixed store selection, bounded jobs, empty remote builders,
  post-repair verification, and fail-closed command/store errors. Linux systemd and macOS launchd
  assets now also provision a distinct root-owned helper HOME/TMPDIR (and root-only helper log
  directory), so privileged Nix never writes into the unprivileged broker's private home. The
  Linux `pkg-root-helper` executable now consumes exactly one systemd-activated listener, validates
  root/broker identities and the exact endpoint, safely initializes only the pkg GC-root subtree,
  and binds authenticated requests to the real fixed local-store executor; Linux container tests
  compile and exercise its platform-only contracts. The Linux `pkg-nix-broker` executable now
  validates its dedicated non-root identity and exact systemd-activated public socket, authenticates
  callers from peer credentials, and bounds concurrent sessions plus idle/write time. It deliberately
  serves the existing operation lifecycle plus six typed adapter calls: a caller-owned live handle
  of an authorized class gates each exposed fixed `RealNixAdapter` method, and GC acquires its
  machine-wide admission before execution. Build remains deliberately absent because a
  caller-constructed `BuildApprovalReceipt` cannot be treated as authority; its wire method stays
  unassigned. The in-process broker now supplies the underlying authenticated capability state: one
  private `BuildPlan` is retained behind the caller/epoch-bound build handle, only its sanitized
  preview/digest is public, exact approval is one-shot, and wrong-UID/digest, replay, cancellation,
  disconnect, expiry, or restart invalidate it. Approval journaling is now broker-owned: a private
  operation identity derived from the authenticated opaque handle is journaled before the broker
  retains the receipt, and every terminal lifecycle path revokes the corresponding engine grant.
  The broker's build/repair admission is now an in-memory FIFO: a contending operation waits with
  cooperative cancellation, cannot be bypassed by a nonblocking probe, and is removed on lifecycle
  cancellation/restart without a validation-to-enqueue race. Exclusive GC also refuses while a build
  holder or queued reservation exists, including the handoff gap between them. Wire exposure still
  waits for dispatcher integration, but in-process broker execution now consumes the private
  receipt, replans and rechecks volatile resources under FIFO admission, acquires GC inhibition,
  and invokes the typed adapter without accepting raw targets or receipts. Success retains admission
  for authoritative rooting; failure consumes approval and releases it. Lifecycle cancellation
  during a synchronous adapter call defers permit release until the call returns, so GC cannot race
  in-flight outputs. Every operation carries a broker-private
  cooperative-cancellation token signalled by completion, cancellation, disconnect, expiry, and
  restart. FIFO admission and execution consume that token; no cancellation authority crosses IPC.
  The production resource probe uses safe `statvfs` against the fixed `/nix` mount and samples the
  one-minute host load from `/proc/loadavg` on Linux or the fixed `/usr/sbin/sysctl vm.loadavg`
  query on macOS; malformed, non-finite, negative, unavailable, or overflowing measurements fail
  the dynamic preflight closed and remain outside the approval digest. A hardened installer-layer
  primitive is ready for the broker's durable authority-side approval audit under its existing
  private log directory: rows bind the
  authenticated uid, private operation id, plan digest, policy, source, and timestamp in a
  hash-chained replay-refusing `0700`/`0600` file. This is separate from the CLI-written per-user
  operation journal, so the unprivileged service gains no access to user state and a CLI-only row is
  never authority. It is deliberately not opened by the production service until the closed
  approval dispatcher can pass its kernel-authenticated caller journal into `approve_build`; startup
  validation alone would falsely imply that grants are being recorded. The shared local-build approval engine now
  reserves an operation before journal I/O without holding its authority mutex across that I/O;
  cancellation can revoke an in-flight recording, and a monotonic reservation identity prevents an
  operation-id retry from reviving the earlier journal call. The durable row may remain as audit
  evidence, but no receipt is issued for a revoked reservation. Strict nested request bytes remain intact through
  framing so the domain codecs still reject duplicates and unknown fields. The
  CLI crate now has the matching fixed-endpoint client: connect and I/O waits have finite
  deadlines, request ids are correlated, frames and allocations are bounded, and any mismatch permanently
  fails that connection. An end-to-end Unix-pair test exercises the actual broker server, FakeNix
  six-method FakeNix dispatch, admission cleanup, and a redacted adapter failure followed by
  successful reuse of the same connection. Adapter errors cross only a closed stable code; malformed
  or method-mismatched error frames still poison the connection. A CLI-side `BrokerNixAdapter` now
  presents those six methods through the existing typed `NixAdapter` contract, opens a bounded
  fixed-endpoint lifecycle connection per call, maps only the broker's closed adapter-error code back
  into the domain error, and best-effort cancels before disconnect. Its `build` implementation fails
  locally with `PermissionDenied` and never contacts the broker, so the caller still cannot turn the
  public receipt carrier into authority. The shipped command engine remains fail-closed until
  authenticated build execution and product-command wiring land. This does
  **not** yet claim the full PR: production installer completion, CLI command wiring, the authenticated
  Linux/macOS Real-Nix lanes, Fake↔Real parity, and clean-host self-hosted e2e remain.
- **Purpose:** turn the nightly Real-Nix lane on, capture/refresh goldens, prove Fake↔Real
  parity, and self-host the product on Real Nix end-to-end (`09` §7).
- **Owns:** `.github/workflows/nightly.yml` (Full), golden capture harness, parity diffing.
- **Depends:** PR-24 (wired CLI), **PR-39 (early M1.5 broker/helper contract — Real-Nix execution
  is gated on its accepted ADR, DR-017; listed directly so the Depends-parse CI in AC-R1 enforces
  it, not merely transitively)**, PR-27/PR-28 (Linux+macOS installers + the platform transports
  that implement the PR-39 contract), PR-29 (uninstall), **PR-30 (two-phase repair — repair is the
  in-scope V1 baseline (D-19/INV-12), so the technical preview must demonstrate `pkg repair` on
  Real Nix)**, PR-31, PR-32, PR-33. The demo runs clean-host install → install → rollback →
  uninstall → `repair` on Real Nix, so it requires the installers, uninstall, repair, and the full
  CLI surface — not only the M6 hardening batch. *(Two-phase mutating repair is an unconditional
  V1 milestone (M1.5 repair-scope note), so the PR-30 edge and the Real-Nix `pkg repair` demo are
  permanent; the PR-39 edge likewise stands — Real-Nix execution is unconditionally gated on the
  broker/helper contract.)*
- **Tests & gates:** G-E2E-REAL + G-FAULT + G-SECURITY + G-PERF + G-PLATFORM green on Linux x86_64 & macOS arm64.
- **Demo:** clean-host install → install package → rollback → uninstall, on Real Nix in CI.
- **Reviewers:** primary A; cross F,E; **A mandatory**.
- **Rollback:** hold the preview channel at the prior `sequence`.
- **Parallel:** no (gate apex).
- **Milestone:** M7.

### Milestone M8 — v1

#### PR-37 — v1 RC: compatibility matrix, migration dry-run, sign-off, revoke rehearsal
- **Purpose:** exercise compatibility/downgrade boundaries, run a **revocation rehearsal**
  (`10` §4.4, §8.2), and obtain 2-person + security sign-off.
- **Owns:** `tools/release/rc-checklist.md`, compat tests, min-policyVersion enforcement in CLI.
- **Depends:** PR-36.
- **Tests & gates:** all G-* green on all release platforms; revoke rehearsal passed.
- **Demo:** cut an RC; rehearse a key revocation end-to-end.
- **Reviewers:** primary A; cross F,E; **mandatory security + key-custodian + maintainer-lead**.
- **Rollback:** RC channel can be frozen/rolled back.
- **Parallel:** no.
- **Milestone:** M8.

#### PR-38 — v1.0 release
- **Purpose:** publish v1.0: signed channel, index, managed-Nix, CLI; advisory + compat notes.
- **Owns:** `CHANGELOG.md`, release notes, signed artifacts, public advisory (`10` §3,§8.3).
- **Depends:** PR-37.
- **Tests & gates:** all release gates G-* + G-SIGNOFF + G-ADVISORY.
- **Demo:** the public install one-liner against the signed channel.
- **Reviewers:** primary A; cross F,E; **mandatory security + maintainer-lead**.
- **Rollback:** publish higher-`sequence` rollback channel; CLI self-rollback.
- **Parallel:** no.
- **Milestone:** M8.

---

## 5. Parallelism Matrix

| Can run in parallel with | PRs |
|--------------------------|-----|
| **Spikes (M0.5)** | PR-4 ‖ PR-5 ‖ PR-6 ‖ PR-7 ‖ PR-8 (all independent after PR-1) |
| **State vs Channel vs Detect (M1)** | PR-9 ‖ PR-10 ‖ PR-11 (all depend only on PR-2/PR-3 + spikes) |
| **CLI skeleton (M4 start)** | PR-23 only needs PR-2 → can start during M1/M2 |
| **Lifecycle ops (M3→M4)** | PR-20 ‖ PR-21 (both consume PR-19) |
| **Hardening batch (M6)** | PR-30 ‖ PR-31 ‖ PR-32 ‖ PR-33 ‖ PR-34 ‖ PR-35 (PR-30 starts only after PR-39/M1.5 + PR-27/28 land) |
| **Broker contract milestone (M1.5)** | PR-39 (after PR-3 ‖ PR-10; gates PR-27/28/30/36 — not parallelizable with them) |
| **Installers (M5)** | PR-27 (Linux) ‖ PR-28 (macOS, depends on PR-26); PR-26 (shared build engine) ‖ PR-25 (doctor) ‖ PR-27 |

---

## 6. Critical Path (longest chain gating v1)

```
PR-0 → PR-1 → PR-2 → PR-3
  → PR-11(needs PR-5 spike) → PR-12(needs PR-9) → PR-13
  → PR-14(needs PR-6) → PR-16 → PR-19(needs PR-18) → PR-24
  → PR-36(needs PR-27/28/29/30/31/32/33) → PR-37 → PR-38

Early blocking contract gate (off the longest chain, but hard-blocking M5+):
PR-3 / PR-10 → PR-39 (M1.5) ── gates ──▶ PR-27/28 (M5) ──▶ PR-30 (M6) ──▶ PR-36 (M7)
```

**Interpretation:** the channel/TUF choice (S2 → PR-11) and the resolve→install chain
(PR-13→16→19→24) are on the **longest** critical path; the installers (PR-27/28), local build
(PR-26), and most of M6 hardening run **off** that longest chain and can be parallelized.
Store-prefix spike S1 (PR-4) gates PR-9/12/27 and must not slip past M0.5. **PR-39 is repositioned
as an early blocking design/contract milestone (M1.5), right after the minimum core-types/state
prerequisites (PR-3, PR-10) and well before the broker/helper/platform integration it gates
(PR-27/28), the two-phase repair (PR-30), and Real-Nix execution (PR-36). Because it no longer
depends on the installers (the prior PR-27/28→PR-39 edges were circular with the
PR-39→PR-27/PR-28 edges and are removed), it creates no late-stage bottleneck (it no longer sits
late, after the installers): it can be
developed in parallel with M2/M3/M4 and must land before M5. It sits off the longest chain
(PR-2→PR-10→PR-39→PR-27/28→PR-30→PR-36 is shorter than the resolve→install chain into PR-36), but
no broker/helper integration, repair, or Real-Nix work merges before its accepted ADR lands
(DR-017). The broker-internal, host-global admission gates (build admission lease + GC admission
gate / GC-inhibit permit) are owned — contract + reference — by **PR-39**, so PR-30's GC-inhibit
permit and PR-26's build admission trace to PR-39, not to the M4 GC PR (PR-22 owns reclamation +
the per-user state-mutation `flock` only).**

---

## 7. Milestone → PR → Exit-Criteria Map

| Milestone | PRs | Exit criteria |
|-----------|-----|---------------|
| **M0 Foundations** | 0–3 | Workspace builds; Fake Nix + Fast CI green; domain types & JSON contract stable. |
| **M0.5 Spikes** | 4–8 | All five DRs (DR-001..005) accepted; no irreversible architecture locked before this. |
| **M1 Managed Nix & state** | 9–12 | Managed Nix can be detected/provisioned in a temp root; state schema v1 + journal + integrity landed; channel verify works on fixtures. |
| **M1.5 Broker/helper contract & capability** | 39 | Early BLOCKING design/contract milestone: broker↔CLI and broker↔helper framed RPC, peer auth, operation lifecycle, child containment, opaque expiring single-use capability storage/expiry, restart handshake, **and the broker-internal in-memory admission gates (machine-wide build admission lease + GC admission gate / GC-inhibit permit, AC-S19/S23)** — **designed + reference-implemented on FakeNix/in-process** (real OS transports land in PR-27/28); adapter split enforced (NixAdapter has no repair; MaintenanceAdapter/helper owns repair). **Unconditional BLOCKING gate for PR-27/28 (broker/helper integration), PR-30 (two-phase repair), and PR-36 (Real-Nix execution)** — full two-phase mutating repair is an accepted V1 milestone (DR-017; non-atomic residual RISK-22). |
| **M2 Catalog & resolve** | 13–16 | Pinned Nixpkgs fetched + narHash-verified; disposable deterministic index built/queried; Selector→evaluated derivation plan resolves under pure eval, with realization deferred to acquisition. |
| **M3 Install/activate** | 17–20 | Substitute+verify; atomic activation; install pipeline with rollback-on-failure; remove/upgrade with mixed revisions. |
| **M4 Generations & UX** | 21–25 | Generations/history/rollback/pin; GC+leases; full CLI wired to Fake Nix; completions + doctor. |
| **M5 Local build & installers** | 26–29 | Shared cross-platform local-build engine w/ sandbox+approval (Linux + macOS native); Linux + macOS installers with authenticated helpers + `nixbld` build group plus `nixbld*` users on Linux and `_nixbld*` users on macOS; bounded uninstall. |
| **M6 Hardening & ops** | 30–35 | Two-phase repair (needs M1.5/PR-39); security lane; perf gate; release signing; observability; docs/support export. |
| **M7 Technical Preview** | 36 | Real-Nix nightly green on Linux x86_64 + macOS arm64; Fake↔Real parity; self-hosted e2e. |
| **M8 v1** | 37–38 | RC with compat matrix + revoke rehearsal + sign-off; v1.0 published with advisory. |

---

## 8. Dependencies on Other Plans (per-PR alignment)

| Plan | Owns which PR decisions |
|------|-------------------------|
| `00` | scope/invariants → PR-0, PR-2, PR-24 honesty copy |
| `01` | architecture/NixAdapter contract → PR-3, PR-9, PR-12, PR-39 |
| `02` | channel/TUF schema → PR-5, PR-11, PR-33 |
| `03` | index model → PR-14, PR-15, PR-33 |
| `04` | resolve/install/build pipeline → PR-13, PR-16, PR-17, PR-19, PR-20, PR-26, PR-30 |
| `05` | state/locks/gen/GC → PR-10, PR-18, PR-21, PR-22 |
| `06` | CLI/UX → PR-23, PR-24, PR-25, PR-34, PR-35 |
| `07` | installers/runtime → PR-4, PR-9, PR-12, PR-27, PR-28, PR-29, PR-39 |
| `08` | security → security reviews on PR-3,9,10,11,12,16,17,18,19,26,27,28,29,30,31,33,34,36,37,38,39 |
| `09` | test lanes → PR-3, PR-31, PR-32, PR-36 |
| `10` | release/ops → PR-33, PR-34, PR-35, PR-36, PR-37, PR-38 |

---

## 9. Risky-Before-Irreversible Guardrails

- **No installer layout is merged before S1 (PR-4).** If S1 unexpectedly shows `/nix/store`
  is unusable, the entire managed-Nix model (PR-9/12/27/28) replans — cheap now, expensive later.
- **No channel implementation before S2 (PR-5).** A wrong crypto choice (e.g., accidental
  "TUF-lite") is the single most expensive mistake to unwind post-release.
- **No macOS build-security commitment before S3 (PR-7).** If `aarch64-darwin`/`x86_64-darwin`
  cache coverage gaps are material, **or** if native macOS local builds (sandbox, `_nixbld`
  users, Xcode toolchain, honest resource boundary) prove unviable, v1 must curate the catalog, ship a
  product cache, or narrow the build policy — a scope decision, not a late surprise.
- **No local-build UX before S5 (PR-8).** Sandbox escape and the honest resource boundary (no per-build cap) must be proven.
- **No perf budgets set before S4 (PR-6).** Otherwise budgets are fiction and the perf gate
  (PR-32) misfires.
- **No broker/helper integration, two-phase repair, or Real-Nix execution before PR-39 (M1.5).**
  The broker/helper framed RPC, peer auth, operation lifecycle, child containment, capability
  storage/expiry, and restart handshake are a blocking **design/contract** milestone that lands
  **early** (right after the core-types/state prerequisites PR-3/PR-10), before the broker/helper
  platform integration (PR-27/28), two-phase repair (PR-30), and any Real-Nix execution (PR-36)
  (`12` DR-017); PR-27/28/30/36 **must not merge** before PR-39's accepted ADR lands. This is a
  *dependency* made explicit and decoupled from the installers (a contract must precede the
  transports that implement it), not a V1 scope broadening: the boundary was already accepted
  (D-19/DR-016); PR-39 only specifies the wire. Full two-phase mutating repair (PR-30) is an
  **accepted, unconditional V1 milestone** (DR-017; non-atomic residual RISK-22) — no post-V1
  deferral.

---

## 10. Testable Acceptance Criteria (roadmap-level)

- **AC-R1** The DAG in §3 matches the actual PR graph (no PR merges with an unsatisfied
  dependency; enforced by a CI check that parses `Depends:` headers).
- **AC-R2** No PR in the trust surface merges without the §2 security review (enforced by
  CODEOWNERS + a required-label rule).
- **AC-R3** Every spike PR closes with an accepted DR in `12` before its dependent PR opens.
- **AC-R4** M7 exit is gated on Real-Nix nightly green on both Linux x86_64 and macOS arm64.
- **AC-R5** M8 exit is gated on a completed revocation rehearsal + 2-person sign-off.
- **AC-R6** PR-39 is an **early** blocking design/contract milestone (M1.5, after PR-3/PR-10) and
  **does not depend on PR-27/PR-28** (that would be circular — PR-27/28 implement the PR-39
  contract). PR-27 (Linux helper), PR-28 (macOS helper), PR-30 (two-phase repair), and PR-36
  (Real-Nix execution) do not merge before PR-39 lands its accepted ADR (DR-017); the unprivileged
  `NixAdapter` exposes no repair method (AC-S21) and the DAG in §3 carries the PR-39→PR-27 /
  PR-39→PR-28 / PR-39→PR-30 / PR-39→PR-36 edges with **no** PR-27→PR-39 or PR-28→PR-39 back-edge
  (enforced by the `Depends:` parse in AC-R1). (PR-30/repair edges are permanent: full two-phase mutating repair is an accepted, unconditional
  V1 milestone (DR-017; non-atomic residual RISK-22), so the PR-39→PR-30 and PR-36→PR-30 edges
  never drop; PR-39 unconditionally gates PR-27/28/30/36.)
- **AC-R7** No M4 PR is credited with supplying a **broker-hosted** gate. The broker-internal,
  host-global **admission gates** (machine-wide build admission lease, AC-S19; GC admission gate /
  shared GC-inhibit permit, AC-S23) are owned — contract + FakeNix/in-process reference — by
  **PR-39** (M1.5) and hosted at runtime by the broker (PR-27/28 transports). **PR-22** (M4) owns
  only GC reclamation logic + the per-user state-mutation lease (a filesystem `flock`); PR-30's
  GC-inhibit permit traces to **PR-39**, not to PR-22 or PR-18. The §4 `Depends:` attributions and
  §6 interpretation must agree with this (any label crediting an M4 PR with a broker-hosted gate is
  a defect).

---

## 11. Primary Sources

- `[NIX-MANUAL]`, `[NIXPKGS-MANUAL]` — underpin the contract (PR-3) and resolve/build PRs.
- `[TUF]`/`[TOUGH]` — underpin the channel PRs (PR-5/11/33).
- `[RB]` — reproducibility framing for index determinism (PR-14) and perf (PR-32).
- Workspace/CI tooling: `cargo`, `cargo deny`, `cargo audit`, `criterion`, GitHub Actions.

---

## 12. Unresolved Questions (→ `12`)

- Q1 Whether aarch64-linux uses native runners or QEMU (affects PR-36 platform matrix cost).
- Q2 Whether v1 ships a product-hosted cache (affects T-CACHE-2 and PR-33 scope).
- Q3 Final perf budgets (PR-32) once S4 numbers land.
- Q4 Exact TUF threshold at GA (PR-33/37, DR-002).
- Q5 Exact broker/helper framed-RPC transport, capability-token format, and restart-handshake
  bytes — **open by design**, resolved inside PR-39 (M1.5) and recorded in its ADR (DR-017); the
  planning docs deliberately leave the wire unspecified.
