# 12 — Open Decisions, Risks, Spikes & Defaults

**Owner:** Assurance track (plans 08–12). **Status:** Draft v1 (living document; planning only).
**Depends on:** `00`..`11`. **Feeds into:** every other plan (this is the single registry of
decisions, risks, spikes, and v1 defaults/deferrals referenced throughout).

> **How this document is used.** Spikes in `11` (S1–S5) write their conclusions here as
> Decision Records (DR-*). Threats in `08` (T-*) are tracked here as Risks (RISK-*). Release
> gates in `10` and test lanes in `09` reference these entries by ID. When a decision changes,
> the DR is **superseded**, not deleted.

---

## 0. Glossary (shared across all plans)

| Term | Meaning |
|------|---------|
| **Selector** | User intent, e.g. "openssl" (display pname@version); **not** a unique identity. (`05`,`06`) |
| **Realization** | The exact pinned-Nixpkgs attribute + version + system actually installed. (`00`,`04`) |
| **Identity** | A Realization's canonical identity is its **store path** (`StorePath`); attribute/rev/system/output are resolution provenance/lookup data, not a competing identity. Two equal Realizations ⇒ same Identity. |
| **Channel descriptor** | The signed metadata selecting managed-Nix version, Nixpkgs rev/narHash, index hashes, allowed substituters/keys, systems, policyVersion, sequence, expiry. (`02`) |
| **Managed Nix** | The pinned Nix runtime the product bundles/owns; users never invoke it directly. (`01`,`07`) |
| **Disposable index** | Derived search/list/info data; regenerable; **not** a trust root. (`03`,`08` T-IDX) |
| **policyVersion** | Monotonic integer on the channel; the CLI enforces a minimum. (`10`) |
| **Trust surface** | PRs/areas where a bug can violate authenticity/integrity: channel/keys, state integrity, helper/privilege, substitute, eval purity, uninstall, release signing. (`11` §2) |

---

## 1. Decision Records (DR-*)

Format: **ID · Status · Date · Supersedes/Superseded-by · Context · Decision · Consequences ·
Owner · Source.** Statuses: `Proposed` · `Accepted` · `Superseded` · `Deferred`.

### DR-001 — Store prefix & managed-Nix coexistence
- **Status:** **Accepted 2026-08-09** after recorded F/E/A architecture and security review of
  S1 / PR-4 and the subsequent native managed-Nix host evidence. The technical recommendation
  is unchanged: standard `/nix/store`, exclusive ownership, stock pinned Nix, and fail-closed
  two-phase preflight. **AC-D1 is cleared** for PR-9/PR-12/PR-27/PR-28, subject to each PR's own
  tests and gates. See `spikes/s1-store-prefix/findings.md`.
- **Context:** Nix historically assumes `/nix/store`; relocatable prefixes exist via `--store`
  for some operations but many assumptions bake the path in. v1 must own its store and must
  not collide with or corrupt a user's unmanaged Nix. (`01`,`07`) Spike S1 corrected the prior
  over-broad claim "stock Nix is not relocatable at all": stock Nix *is* chroot-relocatable on
  **Linux only** (`--store <root>`; logical store stays `/nix/store`; programs run only by
  chroot-ing) and *can* change the logical store dir (`local?store=…`), but the latter is
  documented "not recommended" and **makes it impossible to use `cache.nixos.org`**.
- **Decision:** The product uses the standard logical `/nix/store` with **stock (unmodified,
  pinned-upstream) Nix** under an **exclusive managed** model. Product binary/state/config/logs
  live **outside** `/nix` (e.g. `/usr/local/bin/pkg`, `/var/lib/pkg/` (Linux) / macOS
  machine-global **`/Library/Application Support/pkg`** — the leading slash = the machine-global
  `/Library`, distinct from per-user `~/Library/Application Support/pkg` — / per-user
  `$XDG_DATA_HOME/pkg`). **Multi-user with daemon** on both Linux and macOS (single-user is
  unsupported on macOS). The daemon uses the standard socket `/nix/var/nix/daemon-socket/socket`
  (safe *only because* preflight first proves exclusive ownership); a product-specific socket is
  a v2 defense-in-depth option, not a substitute for exclusive ownership. **Go/no-go on
  alternative prefix: NO-GO** — every non-standard option fails at least one hard V1 requirement
  (an alternate logical store-dir and a compile-time store-dir (Nix Meson option
  `-Dlibstore:store-dir=`) break `cache.nixos.org` reuse; a chroot store is Linux-only and
  cannot run programs natively). If an unmanaged or ambiguous Nix artifact is detected, the
  product **fails closed** with remediation and **never auto-deletes** it (no `--force`).
  Coexistence with a separate prefix is **deferred** to v2. (`00` D-03/04/10/14, INV-02;
  `08` T-INST-4, G6; `07` I1/I4)
- **Consequences:** v1 cannot be installed alongside another Nix. Honest, bounded, reversible.
  The spike detector is **install/preflight only** and is the **unprivileged early read-only
  scan**: any Nix artifact — up to and including a lone pkg ownership marker — is a REFUSE
  (exit 2); there is no runtime/mode recognition in the spike. **Two-phase preflight contract:**
  the unprivileged scan can REFUSE (advisory) but can **never authorize** installation — only a
  FULL read-only **privileged** preflight re-run by the signed installer/helper **immediately
  before any mutation** can authorize proceeding. This closes the unprivileged permission gap
  (e.g. `/var/root` is unreadable as non-root) and shrinks the TOCTOU window to the moment
  before mutation. Remediation is split by result: **ambiguity-only** (no unmanaged/marker
  evidence) prints an advisory that **contains no removal/uninstall instructions** and demands
  the privileged read-only recheck; **definite** unmanaged/marker evidence provides **bounded
  vendor-uninstall guidance** (pkg never removes it). Once the DR is Accepted, PR-9 may rely on
  the spike's detector contract (signal IDs; exit 0=clean / 2=refuse / 64=usage; `--root`/`--json`;
  fail-closed-on-ambiguity; no `PKG_PROBE_*` env bypass; pkg ownership marker is corroborating
  only and is itself a refusal at install time). Runtime/`doctor` recognition of an existing
  `/nix` tree as pkg-owned is **deferred to PR-9/PR-12** and must require an
  **authenticated/validated ownership receipt** PLUS verification of the **complete expected
  managed-artifact set** — never a path or marker alone. PR-12/PR-27/PR-28 may rely on the layout
  invariants (standard `/nix/store`; product files outside `/nix`; multi-user daemon; standard
  socket behind exclusive ownership) and the refusal copy.
- **Acceptance limits (honesty):** A real Nix-free macOS host produced a safe
  ambiguity/refusal as non-root** (unreadable `/var/root`), a **root Alpine container produced
  CLEAN**, and **all positive-platform artifacts are fixture-driven**. What remains inference /
  unvalidated for the production detector/installer: two-daemon collision and — critically — **the
  privileged macOS CLEAN** (the two-phase privileged preflight was NOT run in the spike). Real-Nix
  and privileged validation remains mandatory in the PR-9/PR-12 Real-Nix lanes and PR-27/PR-28
  installers; Acceptance does not waive those gates. The later S5 native macOS lane did validate
  native cache-path execution and managed-daemon behavior, but it was not a clean-host installer
  authorization run.
- **Cross-plan note (scoped):** This DR **supersedes** the stale, over-broad
  relocatability/socket statements in E-owned `plans/07` §6.2 — specifically "stock Nix is **not
  relocatable** to an arbitrary prefix" (it is chroot-relocatable on Linux and can change the
  logical store-dir, just not in any way that meets V1's requirements) and spike deliverable #4
  ("prefer product prefix" for the daemon socket), which the DR resolves to the **standard
  socket behind exclusive ownership**. `plans/07` is **E-owned and must be reconciled by its
  owner in PR-9/PR-12**; this spike does **not** edit `plans/07` and no tracking issue is claimed.
- **Owner:** F (Foundations). **Source:** `[NIX-MANUAL]` local-store/multi-user/installation
  pages (versions recorded in spike); Nix source installer scripts + `src/libstore/meson.options`/
  `src/libstore/meson.build` at tag `2.34.8`, commit
  `f3f1c3c5b8ad91850e0f7c590cf177f7ab022024`; spike S1 (`spikes/s1-store-prefix/findings.md`).

### DR-002 — Channel signing: real TUF via `tough`
- **Status:** **Proposed** post **S2 / PR-5** — see `spikes/s2-tough/findings.md`. The
  spike's **technical recommendation and evidence are complete** (all documented S2 success
  criteria in `12` §2 — signed fixture metadata verified end-to-end; revocation proven with
  real `tough` refusals; per-role threshold semantics demonstrated; plus rollback, freeze,
  endless-data, and drained-stream tamper refusals, each pinning the exact `tough` error
  variant — are supported by executed evidence, 20 tests passing), but the DR remains
  **Proposed, not Accepted**: per `11` §2 / `CONTRIBUTING` §5 a spike DR is `Accepted` only
  after the spike owner **and** the affected area owners (**F + A** for DR-002) sign off.
  That recorded sign-off has not happened, so the **AC-D1 gate is NOT cleared** by this
  entry and the dependent PRs (**PR-11** the production channel client, **PR-33**) must not
  merge on this basis until the DR is Accepted.
- **Context:** Need rollback/freeze/mix-match protection, threshold signatures, and revocation
  for a small target set. The brief forbids inventing "TUF-lite" crypto. (`02`,`08` §7)
- **Decision:** Use the **real TUF** specification via the Rust `tough` crate (AWS Bottlerocket,
  pinned **exactly `0.24.0`**, `default-features = false`). Root key offline; v1 thresholds
  1-of-1 with documented rotation, target **2-of-3 at GA**. Sigstore considered for a future
  release-attestation layer; **deferred**. (`08` §7, `10` §4)
- **Consequences:** Adds a mature dependency (`tough` → `aws-lc-rs`/`aws-lc-sys`, which
  compiles AWS-LC from source via CMake — a **new native C/C++/CMake/pkg-config build step**
  the production workspace does not yet have; recorded as a DR-002 consequence in the
  spike). Channel metadata is tiny. **Rollback / freeze / mix-and-match / threshold /
  revocation are all enforceable by `tough`'s real client — but they are NOT free:**
  - **Anti-rollback requires a persistent, single-writer datastore** that survives across
    `pkg update` runs. Without it, `tough`'s rollback guard is never entered (no
    previously-seen `timestamp.json` to compare) and an older-but-validly-signed metadata
    set is accepted. The spike proves this directly: a *fresh* datastore accepts the old
    valid repo while the *same* datastore that previously saw a newer version refuses it.
    **Anti-rollback is a datastore responsibility, not a property of the metadata bytes.**
  - **`ExpirationEnforcement::Safe` is mandatory** on normal update/install paths and
    refuses signed metadata past its `expires` field against the real wall clock;
    `Unsafe` is prohibited there.
  - **Descriptor product-semantic validation is PR-11's duty, not `tough`'s.** `tough`
    supplies the cryptographic/TUF guarantees for `descriptor.json` (authentication,
    integrity, rollback, freeze, mix-and-match, threshold); it does **not** check
    `descriptor.expiresAt` or any build-time value, and the policy fields
    (`schemaVersion`, `policyVersion`, `sequence`, `expiresAt`, systems allowlists,
    `substituters`/`trustedPublicKeys` allowlists, descriptor-hash ↔ TUF-target-hash
    cross-checks) are deferred to PR-11.
- **Open gates (honesty):** (1) **F + A sign-off pending** — per `11` §2 / `CONTRIBUTING` §5
  a spike DR requires spike-owner + affected area-owner (F, A) sign-off; this entry records
  only the technical basis, hence **Proposed**. (2) **Transport is spike-only** — the spike
  loads over `FilesystemTransport` (a local signed repo); PR-11 selects the production
  transport (likely HTTPS) and is not bound by this choice. (3) **Endless-data is only
  partially exercised** — the spike's size-limit test exercises `max_timestamp_size` only;
  the other conservative caps are configured and applied but not adversarially exercised.
- **Owner:** F + A. **Source:** `[TUF]`, `[TOUGH]`, `[SIGSTORE]`; spike S2
  (`spikes/s2-tough/findings.md`).

### DR-003 — macOS build security + signing/notarization (supersedes the former binary-only decision)
- **Status:** Proposed (pending **S3 / PR-7**); technical evidence harness implemented, but Complete real Preflight / native-build / notarization evidence still pending; supersedes the earlier "macOS is binary-only in v1" framing.
- **Context:** Apple requires Developer-ID signing + notarization for a usable installer/runtime. The prior policy made macOS binary-only, but `cache.nixos.org` substitution coverage is imperfect and a blanket refusal on every Darwin cache miss is a poor, surprising experience. macOS *can* build natively for `x86_64-darwin`/`aarch64-darwin`, so the question is **how** to do it securely, not whether to. (`07`,`08` T-BUILD-*)
- **Decision:** On macOS, cache substitution is tried first and preferred; on a cache miss, v1 may build locally for the host's **native** Darwin system — never via Rosetta, cross-compilation, emulation, or remote builders — under the same gates as Linux (D-11): a deterministic build preview, explicit single-operation approval (cancel default), `sandbox=true`/`sandbox-fallback=false`, and the managed daemon's `nixbld` build-user group / `_nixbld*` build users. `pkg` fails closed if sandbox or build-user readiness cannot be verified and never claims macOS isolation identical to Linux's. The installer/runtime + `pkg` binary must be signed + notarized; locally-built Nix outputs are **not** individually Apple-notarized by `pkg` (installer codesigning ≠ package building).
- **Consequences:** macOS users no longer fail on every cache miss; they opt into native builds knowingly. Residual: macOS now shares the T-BUILD surface (mitigated by the same gates, with the honest caveat that Nix's macOS sandbox uses different, generally narrower primitives than Linux's), plus native toolchain availability (Xcode/CLT) tracked in RISK-06. Cache coverage gaps become an availability/perf concern, not hard binary-only enforcement (also RISK-06).
- **Owner:** E. **Source:** `[NIX-MANUAL]` (sandboxed builds, macOS build users); spike S3.

### DR-004 — Resolve UX & index strategy gated on reevaluation cost
- **Status:** Proposed (pending **S4 / PR-6**).
- **Context:** Realizing an attribute at a pinned rev has measurable cost; the disposable
  index accelerates search/list/info but install re-evaluates the exact attribute. (`03`,`04`)
- **Decision (default pending spike):** Search/list/info use the disposable index; install
  re-evaluates the exact selected attribute under pure eval. Perf budgets (`09` §6.7) are
  finalized from S4 numbers. Pre-built upstream `packages.json.br` may accelerate *official*
  channels but is **not** assumed permanent or cross-platform-complete.
- **Consequences:** Resolve may show a progress step on cache-miss; documented in UX.
- **Owner:** E + F. **Source:** `[NIXPKGS-MANUAL]`; spike S4.

### DR-005 — Managed-daemon local builds: sandbox + approval + resource-boundary
- **Status:** **Accepted 2026-08-09** after E/A architecture and security review of the S5 / PR-8 Linux Docker and native macOS managed-host evidence, including reboot persistence on macOS. The DR decision gate for PR-26 is cleared; PR-26 remains subject to its own dependencies, production approval-receipt/admission-lease implementation, and tests.
- **Context:** Local builds run Nixpkgs build scripts on the host; `sandbox=true`/`sandbox-fallback=false` is the primary control, and the single-operation build preview/approval gate makes each build explicit. (`04`,`08` T-BUILD-1/3). The earlier "sandbox + caps" framing overstated what stock Nix provides.
- **Decision:** Local builds (Linux **and** macOS, native system only) are permitted **only** after an explicit, non-default **single-operation** user approval following a deterministic closure/derivation preview; `sandbox=true`/`sandbox-fallback=false`; build-user isolation (group `nixbld` on both platforms; users `nixbld*` on Linux and `_nixbld*` on macOS); and fail-closed readiness verification. macOS shares this policy per DR-003 (no longer binary-only). Approval never overrides a hard policy refusal (unsupported/broken/impure derivation, or sandbox/build-user unavailable). **Evaluation/planning never realize outputs:** the exact pinned installable is evaluated with `nix derivation show --recursive` (top-level derivation JSON version 4; Nix 2.34.8 has no `derivation show --json-format` selector) and import-from-derivation disabled; `nix build` begins only at acquire — pure substitution first (with `max-jobs=0`), then an approved local build if needed. Immediately before local-build execution `pkg` acquires the machine-global local-build admission lease, recomputes the exact derivation/readiness `BuildPlan` and compares its digest to the approved one, then re-measures disk/free-space/load outside the digest; on mismatch or failed preflight it fails/re-prompts as specified and releases the lease on all exits. **Honest resource boundary:** stock Nix 2.34.8 provides **no** per-build memory/CPU/IO cap. What holds: `max-jobs=1` bounds concurrent derivations per client/connection (so `pkg` adds a machine-global local-build admission lease across users); `cores` only supplies the `NIX_BUILD_CORES` cooperation hint; `timeout`/`max-silent-time`/`max-build-log-size` are daemon-enforced bounds; disk/free-space/load preflight; Nix `use-cgroups` (experimental feature `cgroups`, **Linux-only**) creates a per-build cgroup for process grouping, lingering-process cleanup, and CPU statistics — it does **not** write `memory.max`/`cpu.max`/`pids.max`/IO limits, so it is **not** a resource cap; macOS has no cgroup equivalent. The Rust `pkg` client and the `nix` CLI it spawns are **socket clients** of the long-lived `nix-daemon`; RLIMIT or cgroup membership applied to those client processes does **not** constrain the builders the daemon spawns. Regular input-addressed derivations are filesystem-sandboxed and network-denied; fixed-output derivations remain filesystem-sandboxed but are intentionally network-enabled (their output hash is the integrity boundary); `__noChroot` is rejected under `sandbox=true`. **Service-manager ceilings are Pending defense-in-depth, not accepted enforcement (distinct semantics).** systemd `MemoryMax`/`TasksMax`/`CPUQuota` (Linux) would be an **aggregate service-cgroup ceiling over the daemon plus all descendants** (a coarse whole-unit limit, not a stable per-build control). launchd `SoftResourceLimits`/`HardResourceLimits` (macOS) are **inherited per-process RLIMIT ceilings** (`CPU`/`Data`/`FileSize`/`NumberOfFiles`/`NumberOfProcesses`/`ResidentSetSize`/`Stack`; no `AddressSpace` key in `launchd.plist`) — **not** an aggregate daemon-subtree ceiling, and several keys are advisory or alter system `sysctls` for system daemons (dangerous system-wide). The two must not be lumped together as one coarse limit.
- **Consequences:** Users opt into build risk knowingly. Sandbox escapes (T-BUILD-1) and **resource exhaustion (T-BUILD-2) remain disclosed residuals (RISK-07)** — there is no hard per-build memory/CPU/IO guarantee. Approval is one operation only (bound to the canonical `BuildPlan` digest + policy version; `--yes` pre-approves that one op; no `PKG_YES_TO_BUILDS`/same-session skip/`build.always_local_after_preview`).
- **Owner:** E + A. **Source:** `[NIX-MANUAL]` (sandboxed builds, cores-vs-jobs, experimental features); Nix 2.34.8 source — `src/libstore/include/nix/store/local-settings.hh`, `src/libstore/unix/build/{linux,darwin}-derivation-builder.cc` + `derivation-builder.cc`, `src/libstore/derivations.cc` (https://github.com/NixOS/nix/tree/2.34.8); https://releases.nixos.org/nix/nix-2.34.8/manual/advanced-topics/cores-vs-jobs.html ; https://releases.nixos.org/nix/nix-2.34.8/manual/development/experimental-features.html ; spike S5.

### DR-006 — cache.nixos.org is the single v1 binary source
- **Status:** Accepted (from `00` baseline); residual tracked as RISK-04.
- **Context:** v1 trusts one binary cache. (`01`,`08` T-CACHE-2)
- **Decision:** Admit **only** `cache.nixos.org` (+ product key if/when self-hosted) via the
  signed channel. A product-hosted cache / multi-source threshold trust is **deferred** to v2.
- **Consequences:** A cache.nixos.org compromise is a high-impact residual (RISK-04); bounded
  by pinning Nixpkgs (closure reproducibility) + `repair`.
- **Owner:** A. **Source:** `[HYDRA]`, `[NIX-MANUAL]`.

### DR-007 — Exclusive managed Nix; fail-closed; never auto-delete
- **Status:** Accepted (from `00`/`07` baseline).
- **Context:** Two Nix installs → ambiguous trust, store collisions. (`07`,`08` T-INST-4)
- **Decision:** v1 takes exclusive ownership; on detecting unmanaged Nix it refuses with
  remediation and **never** removes the user's install.
- **Consequences:** Some users must remediate manually; safety over convenience.
- **Owner:** E. **Source:** product baseline.

### DR-008 — Rust owns state/locks/generations/activation/GC roots; Nix profile not authoritative
- **Status:** Accepted (from `00`/`05` baseline).
- **Context:** If Nix profiles were authoritative, rollback/GC/pin semantics would be fragile.
  (`05`)
- **Decision:** The product owns the desired-state set, exact locks, generations, activation,
  and GC roots. Nix profile state is ignored/mirrored, not trusted. **Activation is realized as
  a store-independent symlink forest outside `/nix/store` (no Nix); per-output GC roots are
  created before the `current` swap — see DR-016 / `00` D-18 / INV-11.**
- **Consequences:** Consistent rollback & GC; more code in Rust (noted in `11` PR-18/21/22).
- **Owner:** E. **Source:** product baseline.

### DR-009 — No runtime isolation of installed apps in v1 (honesty)
- **Status:** Accepted.
- **Context:** Once activated, installed binaries run with user privileges. Sandboxing
  installed apps at runtime is a large scope. (`08` T-RUN-1, §11)
- **Decision:** v1 provides **provenance + reproducibility + fast catalog revocation**, **not**
  runtime isolation. This is disclosed to users (`06`,`10`). Runtime isolation (e.g., firejail/
  bwrap integration) is **deferred**.
- **Consequences:** T-RUN-1 is a high residual; mitigated by disclosure + revocation.
- **Owner:** A. **Source:** `[RB]`, `[NIXPKGS-MANUAL]` (Nixpkgs not curated).

### DR-010 — The disposable index is not a trust root
- **Status:** Accepted.
- **Context:** An index could be poisoned to mislead search. (`08` T-IDX-1/2/4)
- **Decision:** Packages are **resolved and realized** from pinned Nixpkgs, never "installed
  from the index." Index is convenience metadata; hash pinned in signed channel; regenerated
  on mismatch.
- **Consequences:** A poisoned index can mislead UX but cannot change installed bytes.
- **Owner:** F + A. **Source:** product baseline.

### DR-011 — No arbitrary Nix/flake/overlay/impure controls exposed to users
- **Status:** Accepted (from `00`/`02` baseline).
- **Context:** Exposing expression/substituter/key controls would re-open the attack surface
  Nix's native trust model presents. (`02`,`08` T-DAEMON-1, T-EVAL-1)
- **Decision:** Users get brew/paru-style commands only. The product never passes user
  expressions, substituters, or trust keys to the daemon. Pure eval enforced.
- **Consequences:** Less flexibility (intentional); smaller, auditable surface.
- **Owner:** F. **Source:** product baseline.

### DR-012 — Logs/telemetry default to redacted/opt-out-friendly
- **Status:** Accepted.
- **Context:** Logs can leak secrets; telemetry can leak behavior. (`08` T-LOG-1/2/3)
- **Decision:** Structured logs `0600`, allowlist fields, denylist redactor, control-char
  escaping. Telemetry opt-in and minimal. Crash records redacted; no core dumps by default.
- **Consequences:** Safer support data; small engineering cost.
- **Owner:** A. **Source:** privacy minimization norms.

### DR-013 — State integrity via channel anchor, not a client-side secret key
- **Status:** Accepted.
- **Context:** A MAC keyed by a client-side secret protects nothing against local root (the key
  is on disk). The goal is **detection + fail-closed**, not secrecy. (`08` §7.3, T-STATE-1)
- **Decision:** State is tamper-evident via a generation Merkle root anchored to a field in the
  last-applied signed channel `targets`. Tampering fails closed to the last good generation.
  The per-generation `treeDigest` (sorted path→store-target/source records of the activation
  forest, DR-016) is included in that anchor, so a tampered activation forest is detected.
- **Consequences:** No key management on the client; root can still tamper (we detect only).
- **Owner:** E + A. **Source:** product baseline.

### DR-014 — The product does not compile Nix; it distributes a pinned runtime by hash
- **Status:** Accepted.
- **Context:** Linking unstable Nix C++ or building Nix per-release is fragile. (`00`,`01`)
- **Decision:** Managed Nix is a pinned upstream release tarball, hash-recorded in signed
  channel `targets`. We trust upstream Nix by hash, not rebuild.
- **Consequences:** Nix updates follow upstream releases; CVE response is a channel bump.
- **Owner:** F. **Source:** `[NIX-MANUAL]`.

### DR-015 — Project license and source-header policy
- **Status:** Deferred.
- **Context:** The project has not yet chosen an open-source license. Until the human owner
  explicitly selects one and records it as a superseding Accepted DR, no public license grant
  exists, no `license` field is set in any `Cargo.toml`, and no `SPDX-License-Identifier`
  headers are added to source files. This is a *project* licensing decision and is deliberately
  independent of which licenses the project permits in its *dependencies* (the `cargo-deny`
  allowlist in `deny.toml`). (`11` PR-1/PR-2)
- **Decision:** All rights reserved by default. The workspace and `pkg-core` manifests carry
  **no** `license` field and `publish = false`; source files carry **no** `SPDX-License-Identifier`
  headers. The `cargo-deny` permissive license allowlist in `deny.toml` is a **dependency**
  policy only — it is not a project license and grants no rights to `pkg` itself. This
  deferral is lifted only by a later Accepted DR that supersedes DR-015.
- **Consequences:** The project is unambiguously "all rights reserved" until then; nobody may
  redistribute or reuse `pkg` source under any license. The build/lint toolchain (PR-1)
  remains fully operable with no `license` field. Contributors must not add SPDX headers or a
  `license` field until DR-015 is superseded.
- **Owner:** project owner (human). **Source:** PR-1 correction.

### DR-016 — Activation is a Rust-owned symlink forest outside the store; activation invokes no Nix
- **Status:** **Accepted** (V1 architecture decision; supersedes the former "activation = Nix `buildEnv` store object" framing captured in `04` Q4.1 and referenced across `01`/`04`/`05`/`06`/`07`).
- **Context:** The prior design realized each generation's activation as a Nix `buildEnv` store
  object, so activation depended on a per-generation Nix build and a single generation-level GC
  root. That couples the user-facing activation to Nix, adds a Nix build to every
  install/remove/upgrade, and makes collisions a whole-tree Nix decision. (`00`, `04` Q4.1,
  `05`, `07`)
- **Decision:** `pkg` (Rust) owns activation as a **deterministic per-generation symlink forest**
  materialized **outside `/nix/store`** under `<user-state>/activations/gen-<id>/`; forest entries
  point at `/nix/store` targets or approved sources. `current` is a **relative symlink** into
  `activations/`. A `treeDigest` binds the sorted path→store-target/source records, so the forest
  is content-addressable and collision policy is enforced in Rust. **Activation invokes no Nix**
  (no `buildEnv`, no activation-expression `nix build`); Nix owns downloads/builds/store only. The
  private broker is the sole **mediator/requester** for **one root set per selected output** (not
  one per generation); the narrow privileged root-helper/service is the sole filesystem writer
  that atomically publishes/removes those root sets before the `current` swap (the broker is an
  **allowed-user, not a trusted-user**; root is the sole trusted-user — detail in docs 01/07/08).
  V1 collision policies are **only** `abort` (default),
  `keep-first`, `keep-last` (no `keep-all`, no `--force`); `keep-first`/`keep-last` pick a
  deterministic per-file winner while the losing package's other (non-colliding) files remain
  visible. (`00` D-12/D-17/D-18, INV-05/INV-11; `06` §6.4/§7; `07` §6.1/§7.4/§10)
- **Consequences:** Activation is store-independent, deterministic, and needs no per-generation
  Nix build; collisions are resolved per-file in Rust. **Cross-plan reconciliation (relative to this DR):** `01`, `04`, and `05` are **all converted** to
  the symlink-forest / per-output-root / `abort`|`keep-first`|`keep-last` model — `01`: the
  `activator` materializes the symlink forest, activation invokes ZERO Nix commands, and there is
  one root per selected output; `04`: Q4.1 is RESOLVED → Rust symlink forest with `buildEnv`
  explicitly rejected, and §12.2 collision policy is abort/keep-first/keep-last only (no
  `keep-all`, no `--force`); `05`: now carries the symlink-forest / per-output-root /
  abort|keep-first|keep-last model with no `buildEnv` store-object framing remaining. There is
  **no remaining E-track `buildEnv` reconciliation**. The managed **private broker** boundary is
  the **accepted hidden-Nix V1 ownership shape** — an **unprivileged singleton** broker that
  authenticates the caller, is the **sole general Nix daemon client and sole spawner of the bundled
  `nix` CLI for all normal operations**, and is the sole **mediator/requester** for per-output
  GC-root operations and for helper-run repair (hosting the broker-internal in-memory build/GC
  admission gates), with the narrow privileged **root-helper/service** as the **sole root-set
  filesystem writer** that atomically publishes/removes per-output root sets **and the one
  exceptional maintenance client** running the two-phase `nix store repair` as root; the broker is
  an allowed-user but **not** a trusted-user (root is the sole trusted-user). The full privilege
  split + two-phase repair + capability + admission/log rules are fixed in D-19/DR-017. This boundary is **accepted**, so creating the boundary itself is **not** the next
  milestone: the next required V1 milestone is its **detailed framed RPC / peer-auth / privilege /
  concurrency design**, tracked as the blocking **PR-39** milestone in `11`'s PR DAG (it gates
  Real-Nix execution / PR-36 and the two-phase repair / PR-30); the wire design itself remains
  open (DR-017). Security residuals (T-PATH-*,
  T-STATE-*) are unchanged in severity; the forest is per-user 0700 and the `treeDigest` (now in the
  DR-013 anchor) makes tamper evident.
- **Owner:** E + F. **Source:** V1 architecture decision (this registry); cross-refs `00`
  D-18/INV-11, `06`, `07`.

### DR-017 — Broker/helper privilege split & two-phase store repair (boundary accepted; wire design open)
- **Status:** **Accepted** — and the **full privileged two-phase mutating store repair is an unconditional V1 milestone** (accepted product decision; not conditional, not deferrable to post-V1). This covers the privilege-split boundary and the two-phase repair semantics (Phase 0 read-only verify via broker → Phase A cache-only repair via helper → Phase B approved local rebuild via helper; `00` D-19/INV-12): **PR-30 is in scope for V1 and is unconditionally gated by PR-39**. The **exact framed RPC / peer-auth / operation-lifecycle / child-containment / capability-token / restart-handshake wire design remains OPEN** and is tracked as the blocking **PR-39** milestone in `11` (it unconditionally gates broker/helper integration / PR-27/28, two-phase repair / PR-30, and Real-Nix execution / PR-36). The verified **non-atomic** residual is retained as **RISK-22**. No implementation detail of the wire is resolved here and none must be inferred.
- **Context:** The hidden-Nix boundary is accepted (DR-016), but the operational facts it implies needed to be pinned: who may drive the daemon, who may mutate the store, how repair works given Nix 2.34.8 trust rules, how admission/GC coordination is represented, and what is logged where. (`00` D-19, `01` ARCH-INV-01/05/06/07, `05` §10, `07` I7, `08` T-INST-7/T-DAEMON-1)
- **Decision (boundary facts, accepted):** (1) The **unprivileged singleton broker** is the **sole general Nix daemon client and sole spawner of the bundled `nix` CLI for all normal operations** (a daemon `allowed-user`, never a `trusted-user`; root is the sole `trusted-user`, and `trusted-users` are root-equivalent). (2) The **privileged root helper** is the **sole root-set filesystem writer** and the **one exceptional maintenance client**; repair requests require a `trusted-user` (verified Nix 2.34.8), so every repair mutation runs as the helper's single fixed two-phase op against a broker-chosen validated typed `StorePath` set drawn from broker-held generation state. (3) **Two-phase repair:** Phase 0 read-only `nix store verify` (broker) → Phase A cache-only `nix store repair` (helper; managed pinned substituters/keys; `max-jobs=0`; `builders` empty; auto on a signed cache hit; **must stop before any build** on a cache miss) → Phase B approved local rebuild (helper; bounded nonzero `max-jobs`; `builders` empty; serialized by the broker's machine-wide build mutex and holding a shared GC-inhibit permit; the ordinary public build preview / explicit single-operation approval; internal `RepairBuildPlan`/digest covers every output Nix may rebuild). (4) **Capability:** opaque, expiring, single-use, bound server-side to uid / existing pkg-owned rooted generation/closure / typed `StorePath` set / `RepairBuildPlan` digest / `policyVersion` / mode; stale/replayed/mismatched/cross-UID capabilities **fail closed**; invalidated on helper/broker restart. (5) **Admission:** build admission and GC admission are **broker-internal in-memory gates, not backing-file `flock`s** (a single broker cannot represent independent shared holders portably); the per-user state-mutation lease remains a filesystem `flock`. (6) **Logs:** raw Nix logs are **service-private only**; public/user-state logs are sanitized NDJSON.
- **Consequences:** The boundary is fixed and citable (D-19, INV-12). Repair is **verified non-atomic** — cache repair deletes the live path before restore and an approved local repair moves the old output aside before replacement — disclosed as residual **RISK-22**, mitigated by user-initiated + availability warning + per-path journal + final read-only verify + cache-only auto-resume + fresh-approval-for-local. Confused-deputy repair is **T-INST-7** (mapped to RISK-10). The **wire design is not resolved here** and must not be inferred; it lands in PR-39 before any Real-Nix execution.
- **Owner:** E + A. **Source:** `[NIX-MANUAL]` (store verify vs repair are separate modern commands; `--repair` requires a `trusted-user`; verified Nix 2.34.8); `01` ARCH-INV-01/05; `05` §10; `08` T-INST-7.

---

## 2. Go / No-Go Spikes (S1–S5)

| Spike | Question | Success criterion | Blocks (no-merge before DR) | Decision deadline | Status | DR |
|-------|----------|-------------------|------------------------------|-------------------|--------|----|
| **S1** (PR-4) | Is `/nix/store` viable for exclusive managed use, and how do we detect/safely refuse unmanaged Nix? | Concrete layout + detection method + refusal text validated on Linux & macOS; go/no-go on alternative prefix. | PR-9, PR-12, PR-27, PR-28 | end of M0.5 | Proposed (technical evidence complete; F/E/A sign-off pending) | DR-001 |
| **S2** (PR-5) | Does real TUF via `tough` express our target set + revocation + threshold? | Signed fixture metadata verified end-to-end; revocation dry-run passes; threshold demo. | PR-11, PR-33 | end of M0.5 | Proposed (technical evidence complete; F + A sign-off pending) | DR-002 |
| **S3** (PR-7) | Do v1 attrs substitute for `x86_64-darwin`/`aarch64-darwin`, **and** are native macOS local builds viable (sandbox, `_nixbld` users, Xcode toolchain, the honest resource boundary with **no** per-build cap in stock Nix, fail-closed)? Is a notarized installer feasible? | Availability matrix for fixture attrs; a real native sandboxed Darwin build under `_nixbld`; notarized installer/runtime builds & validates. | PR-26, PR-28, PR-36 (macOS lane) | end of M0.5 | Proposed (harness implemented; Complete real Preflight/native-build/notarization evidence pending) | DR-003 |
| **S4** (PR-6) | What are realistic resolve/reeval costs and index-build costs? | Measured time/memory table; proposed budgets. | PR-14, PR-16, PR-32 | end of M0.5 | Proposed | DR-004 |
| **S5** (PR-8) | Does managed-daemon sandbox + single-operation approval + the honest resource boundary work for local builds on **both Linux and macOS**? | A **regular** derivation build that is filesystem-sandboxed + network-denied under `sandbox=true`; a fixed-output derivation intentionally network-enabled (hash boundary); `nixbld` group + `nixbld*`/`_nixbld*` users ready; single-operation approval + fail-closed demonstrable; recorded **managed-host behavioral evidence** (boundary = `max-jobs`/timeout/max-silent-time/max-build-log-size + disk/free-space/load preflight; `use-cgroups` is cleanup/statistics on Linux; service-manager ceilings have **distinct** systemd-vs-launchd semantics and are Pending) — **not** a verification of per-build caps (none exist in stock Nix). | PR-26, PR-30 | end of M0.5 | Proposed | DR-005 |

> **Guardrail:** Per `11` §9, no irreversible architecture merges before these DRs are
> accepted. If any spike returns **no-go**, the dependent milestone replans before code lands.

---

## 3. Risk Register (RISK-*)

Severity = Likelihood × Impact (rubric in `08` §6.0). Each RISK-* represents a **threat
family** (per `08` §8) and lists the constituent `08` threats it covers; the table shows
the primary/representative threat IDs inline. (Criterion: every family has ≥1 RISK; see
AC-D2.)

| ID | Risk | Linked threat | L | I | Sev | Owner | Mitigation | Residual | Trigger / monitoring |
|----|------|---------------|---|---|-----|-------|------------|----------|----------------------|
| **RISK-01** | A pinned Nixpkgs attribute contains malicious build logic or upstream compromise | T-RUN-1, T-IDX-4 | M | H | **H** | A | provenance, `repair` re-verify, fast catalog revocation, honest disclosure; vuln feed **deferred** | H (inherited) | upstream advisories; CI provenance checks |
| **RISK-02** | cache.nixos.org compromise serves signed-but-malicious paths | T-CACHE-2 | L | H | **H** | A | single-source trust (DR-006); closure reproducibility via pinned rev; `repair`; second source deferred | H (low likelihood) | NixOS security advisories; narHash drift checks |
| **RISK-03** | Channel signing key compromise | T-CHAN-5, T-REL-2 | L | H | H | A | TUF threshold + offline root + short timestamp expiry + revocation procedure (`10` §4.4) | M (until rotation) | signing audit logs; anomaly alerts |
| **RISK-04** | Store-prefix/coexistence surprise invalidates installer layout | T-INST-4 | M | H | H | F | spike S1 **before** PR-9/12/27/28 (`11` §9) | L (if S1 on time) | S1 DR |
| **RISK-05** | TUF/crypto choice wrong (e.g., accidental bespoke scheme) | T-CHAN-1/2/3 | L | H | H | F | spike S2 mandates real TUF/tough (DR-002) | L | S2 DR; crypto review |
| **RISK-06** | Darwin build availability / security / toolchain (cache coverage gaps + native macOS build readiness) | T-BUILD-1/2/3, T-CACHE-1 | M | M | M | E | native macOS builds gated like Linux (D-11/DR-003); cache coverage is an **availability/perf** signal (full-closure preflight), not binary-only enforcement; `_nixbld` users + Xcode/CLT + `sandbox-fallback=false` verified at install/doctor; curate catalog or ship product cache only if coverage gaps are material | M | S3 DR; per-release availability CI; toolchain/sandbox readiness checks |
| **RISK-07** | Local-build sandbox escape / resource exhaustion (Linux **and** macOS) | T-BUILD-1/2/3 | L (escape) / M (exhaustion) | H | **H** | E | `sandbox=true`/`sandbox-fallback=false` on both (Nix's own Linux namespace/chroot sandbox, not bubblewrap; regular-derivation network denial; fixed-output network-enabled with hash boundary); `max-jobs=1` bounds concurrency; `timeout`/`max-silent-time`/`max-build-log-size` daemon bounds; disk/free-space/load preflight; `use-cgroups` cleanup/statistics on Linux (**not** caps); `nixbld` group + `nixbld*`/`_nixbld*` builder-user isolation; service-manager ceilings Pending defense-in-depth; fail-closed readiness; macOS shares this surface per DR-003. **No stock per-build memory/CPU/IO cap, so resource exhaustion is not mitigated by a cap** | M (escape, rare) / **H (exhaustion — disclosed residual; no per-build cap in stock Nix 2.34.8)** | kernel CVEs; macOS sandbox narrower than Linux; build telemetry |
| **RISK-08** | Path/symlink attack on state or activation (incl. the symlink forest) | T-PATH-1/2/3/4 | M | H | H | E | per-user 0700 dirs, O_NOFOLLOW, openat, store-relative forest entries, ancestor-perm checks, `treeDigest` re-validation of the forest (D-18) | L | security lane (AC-S4) |
| **RISK-09** | State tampering / concurrent-writer corruption | T-STATE-1/2/3/4, T-CONC-1/2 | M | H | H | E | integrity anchor (DR-013), flock+lease+journal, atomic current, fail-closed | L | fault lane (AC-S2/S5) |
| **RISK-10** | Privileged helper privilege escalation / unauth IPC / confused-deputy repair | T-INST-3/5/7, T-DAEMON-1 | L | H | H | E | caller auth, allowlist, prefer polkit/launchd over setuid, drop privs; **repair is the helper's one fixed maintenance op against a broker-chosen validated typed `StorePath` set resolved from an opaque expiring single-use capability** (replay/stale/mismatch/cross-UID fail closed; accepts no public path/installable/derivation/expression/flake/argv/option/substituter-key/env-override/output-selection/verb; returns only sanitized per-path outcome) | L | security lane (AC-S6/S14/S22) |
| **RISK-11** | Index poisoning misleads users | T-IDX-1/2 | M | M | M | F | disposable + hash-in-channel + regen-on-mismatch (DR-010) | L | determinism CI; hash checks |
| **RISK-12** | Channel rollback/freeze attack | T-CHAN-1/2 | M | M | M | F | real TUF expiry + version monotonicity (DR-002) | L | security lane (AC-S1/S2) |
| **RISK-13** | Release/dependency supply-chain compromise (Rust crate malice) | T-REL-1/4 | M | H | H | A | offline root signing, `cargo deny`/`audit`, lockfile review, reproducible CLI builds, attestation | M | release gate G-DEPS; advisories |
| **RISK-14** | Logs/telemetry leak secrets | T-LOG-1/2/3 | M | M | M | A | redactor + allowlist + opt-in telemetry (DR-012) | L | redactor golden tests (AC-S9) |
| **RISK-15** | Uninstall removes user's Nix or leaves privileged residue | T-UNINST-1/2/3 | L | H | M | E | asset-manifest-driven uninstall, dry-run, never touch unmanaged Nix (DR-007), post-uninstall verify | L | e2e uninstall tests |
| **RISK-16** | Non-reproducible local build diverges from cache ("two valid builds") | T-BUILD-4 | M | M | M | E | prefer cache when present; `--check` in repair; document nondeterminism | M | reproducibility CI |
| **RISK-17** | Perf regression slips through (no/loose budgets) | (reliability) | M | M | M | A | S4 budgets + regression gate PR-32 (`10` G-PERF) | L | nightly perf trend |
| **RISK-18** | Eval impurity leaks host info or enables unexpected fetches | T-EVAL-1/2 | L | M | M | F | pure-eval, restrict-eval, allowed-uris, env scrub (DR-011) | L | contract + security lanes |
| **RISK-19** | aarch64-linux CI cost (QEMU vs native) destabilizes the matrix | (operations) | M | L | M | A | decide runner strategy in `12` Q1; budget CI | M | CI cost/burn dashboards |
| **RISK-20** | v1 cannot block a single bad attribute within a pinned rev | T-RUN-1 | M | H | H | A | re-pin/rollback; freeze; **attribute denylist deferred** | H | incident drills (`10` §8) |
| **RISK-21** | Cross-user state tampering / UID confusion on a shared host (D-17 multi-user) | T-INST-6, T-INST-3, T-DAEMON-1 | M | H | H | E | per-user `<user-state>` 0700 keyed by uid (INV-10); UID-authenticated helper/daemon; GC roots scoped to `/nix/var/nix/gcroots/pkg/users/<uid>/` (ARCH-INV-06) | L | security lane (AC-S11/S12) |
| **RISK-22** | `pkg repair` is verified non-atomic (cache repair deletes the live path before restore; approved local repair moves the old output aside before replacement) — affected commands may be temporarily unavailable mid-repair, and a crash between delete/aside and replacement can leave a path absent | (availability) T-CACHE-3 | M | M | **M** | E | repair is **explicitly user-initiated** and warns affected commands may be temporarily unavailable; **journals per path**; runs a **final read-only `nix store verify`** before marking anything repaired; **auto-resumes only cache repair** after a crash; an **approved local repair build requires fresh single-operation approval** (the `mode=build` capability is single-use and invalidated on restart); never claims atomicity or generation-switching (D-19, INV-12) | M (disclosed) | fault lane (crash between delete/restore + between move-aside/replace); AC-S5 |

### 3.1 Top-5 watch list (revisited every milestone)
1. **RISK-01 / RISK-20** — malicious/insecure package in pinned Nixpkgs (core honesty risk).
2. **RISK-02** — cache.nixos.org single-source compromise.
3. **RISK-04** — store-prefix coexistence (must clear S1 before M1).
4. **RISK-05** — crypto correctness (must clear S2 before channel PRs).
5. **RISK-13** — release/dependency supply chain.

---

## 4. v1 Defaults vs. Explicitly Deferred

| Capability | v1 DEFAULT | DEFERRED to | DR / RISK |
|------------|:----------:|:-----------:|-----------|
| Managed Nix (bundled, pinned) | ✅ exclusive | coexistence w/ user Nix → v2 | DR-001, DR-007 |
| Channel signing | real TUF / tough, 1-of-1 → 2-of-3 at GA | Sigstore attestation layer → later | DR-002 |
| Binary cache | cache.nixos.org only | product-hosted / multi-source → v2 | DR-006, RISK-02 |
| macOS local build | ✅ native + sandbox + approval (like Linux) | none (cross-compilation/Rosetta/emulation/remote builders remain out of scope) | DR-003, RISK-06 |
| Linux local build | ✅ sandbox + approval | seccomp hardening refinements → later | DR-005, RISK-07 |
| Search index | disposable, narHash-pinned | upstream packages.json.br as permanent API → not assumed | DR-004, DR-010 |
| Activation | Rust symlink forest outside `/nix/store` (no Nix); per-output GC roots; collisions abort/keep-first/keep-last | (buildEnv store-object activation is **superseded** by D-18, not deferred) | DR-016, D-18 |
| Runtime app isolation | ❌ none | firejail/bwrap integration → v2 | DR-009, RISK-01 |
| Vuln/CVE feed | ❌ none | integration → v2 | RISK-01 |
| Attribute denylist | ❌ none (re-pin only) | per-attribute blocking → v2 | RISK-20 |
| Telemetry | opt-in, minimal | richer usage analytics → later | DR-012 |
| Windows | ❌ | not in v1 | — |
| Threshold root signing | 1-of-1 v1, 2-of-3 GA | higher thresholds → post-GA | DR-002 |
| Project license (source) | ❌ none (all rights reserved) | chosen license + SPDX headers → when DR-015 is superseded | DR-015 |
| aarch64-linux CI | nightly (runner TBD) | native runners → when justified | RISK-19 |
| Store repair | ✅ two-phase (read-only verify via broker; cache-only repair via helper; approved local repair via helper) | (none — single fixed helper maintenance op) | D-19, DR-017 |
| Repair atomicity | ❌ verified non-atomic (user-initiated; warns; per-path journal; final read-only verify) | atomic / generation-switched repair → v2+ | INV-12, RISK-22 |
| Build/GC admission | ✅ broker-internal in-memory gates | backing-file `flock` admission → not used (not portable for shared holders) | D-19 |
| Broker/helper framed RPC | ⏳ blocking milestone **PR-39** (wire design OPEN) | wire schema lands in PR-39, not invented in planning | DR-017 |

---

## 5. Assumptions (must hold; tracked if they move)

1. Upstream Nix continues to publish release tarballs with stable hashes we can pin.
2. `cache.nixos.org` retains the Ed25519 key and continues serving pinned-rev closures for v1
   target systems (S3 confirms darwin coverage).
3. `tough` remains maintained and tracks the TUF spec.
4. The target platforms (Linux x86_64/aarch64, macOS arm64) remain stable.
5. Users accept that v1 cannot coexist with their own Nix (DR-007).

If any assumption breaks, the affected DRs/PRs/RISKs are re-opened and re-planned.

---

## 6. Dependencies on Other Plans

| Depends on | Why |
|-----------|-----|
| `00` | scope/invariants seed DR-007/008/011 and RISK baselines. |
| `01`,`02` | architecture + channel/TUF feed DR-001/002/006/013/014. |
| `03`,`04` | index + resolve/build feed DR-004/005/010 and RISK-07/16/18. |
| `05` | state/locks/gen/GC/activation feed DR-008/013/016 and RISK-08/09. |
| `06`,`07` | UX/installer/activation feed DR-003/007/012/016 and RISK-06/08/10/15. |
| `08` | every T-* maps to a RISK-*; §7 crypto feeds DR-002/013. |
| `09`,`10` | test lanes & release gates consume DR/RISK as acceptance criteria. |
| `11` | spikes S1–S5 produce DR-001..005; PRs reference RISK-* for prioritization. |

---

## 7. Maintenance

- **Cadence:** reviewed at each milestone exit (M0.5, M1…M8) in `11`.
- **Change protocol:** to change a decision, mark the DR `Superseded` and add a new DR that
  references it; update dependent plans' "Unresolved Questions" sections.
- **Audit:** the security lane (`09`) and release gates (`10`) reference DR/RISK IDs; a
  broken reference fails CI (linkcheck, PR-0).

---

## 8. Testable Acceptance Criteria

- **AC-D1** Every spike S1–S5 has a corresponding DR with `Accepted` status before its
  blocking PRs (per `11`) are merged.
- **AC-D2** Every `08` threat *family* (T-INST, T-DAEMON, T-EVAL, T-IDX, T-CHAN, T-CACHE,
  T-BUILD, T-PATH, T-STATE/CONC, T-LOG, T-REL, T-UNINST, T-RUN) is represented by ≥1 RISK-*;
  every **high-severity** individual threat has an explicit RISK-*; and every threat has a
  control in `08` §8 plus a test in `09`'s security lane (directly or via an AC-S*).
- **AC-D3** The Defaults-vs-Deferred table (§4) is consistent with `06` user-facing copy and
  `10` release notes (no silent deferrals).
- **AC-D4** No DR is `Accepted` while a dependency (assumption §5 or prior DR) is broken;
  enforced by the §7 change protocol and CI linkcheck.

---

## 9. Primary Sources

- `[NIX-MANUAL]`, `[NIXPKGS-MANUAL]`, `[NIXOS-SA]`, `[HYDRA]`, `[RB]`, `[TUF]`, `[TOUGH]`,
  `[SIGSTORE]` — as defined in `08` §13; each DR/RISK cites the relevant source inline.
