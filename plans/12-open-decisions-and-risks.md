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
- **Status:** Proposed (pending **S1 / PR-4**).
- **Context:** Nix historically assumes `/nix/store`; relocatable prefixes exist via `--store`
  for some operations but many assumptions bake the path in. v1 must own its store and must
  not collide with or corrupt a user's unmanaged Nix. (`01`,`07`)
- **Decision (default pending spike):** The product uses the standard `/nix/store` under an
  **exclusive managed** model. If an unmanaged Nix is detected, the product **fails closed**
  with remediation and **never auto-deletes** it. Coexistence with a separate prefix is
  **deferred** to v2. (`00`,`08` T-INST-4, G6)
- **Consequences:** v1 cannot be installed alongside another Nix. Honest, bounded, reversible.
- **Owner:** F (Foundations). **Source:** `[NIX-MANUAL]` (store model); spike S1.

### DR-002 — Channel signing: real TUF via `tough`
- **Status:** Proposed (pending **S2 / PR-5**); target `Accepted` before PR-11.
- **Context:** Need rollback/freeze/mix-match protection, threshold signatures, and revocation
  for a small target set. The brief forbids inventing "TUF-lite" crypto. (`02`,`08` §7)
- **Decision:** Use the **real TUF** specification via the Rust `tough` crate (AWS Bottlerocket).
  Root key offline; v1 thresholds 1-of-1 with documented rotation, target **2-of-3 at GA**.
  Sigstore considered for a future release-attestation layer; **deferred**. (`08` §7, `10` §4)
- **Consequences:** Adds a mature dependency; metadata is tiny; gets anti-rollback/freeze free.
- **Owner:** F + A. **Source:** `[TUF]`, `[TOUGH]`, `[SIGSTORE]`.

### DR-003 — macOS build security + signing/notarization (supersedes the former binary-only decision)
- **Status:** Proposed (pending **S3 / PR-7**); supersedes the earlier "macOS is binary-only in v1" framing.
- **Context:** Apple requires Developer-ID signing + notarization for a usable installer/runtime. The prior policy made macOS binary-only, but `cache.nixos.org` substitution coverage is imperfect and a blanket refusal on every Darwin cache miss is a poor, surprising experience. macOS *can* build natively for `x86_64-darwin`/`aarch64-darwin`, so the question is **how** to do it securely, not whether to. (`07`,`08` T-BUILD-*)
- **Decision:** On macOS, cache substitution is tried first and preferred; on a cache miss, v1 may build locally for the host's **native** Darwin system — never via Rosetta, cross-compilation, emulation, or remote builders — under the same gates as Linux (D-11): a deterministic build preview, explicit single-operation approval (cancel default), `sandbox=true`/`sandbox-fallback=false`, and the managed daemon's `_nixbld` build users/group. `pkg` fails closed if sandbox or build-user readiness cannot be verified and never claims macOS isolation identical to Linux's. The installer/runtime + `pkg` binary must be signed + notarized; locally-built Nix outputs are **not** individually Apple-notarized by `pkg` (installer codesigning ≠ package building).
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

### DR-005 — Linux local builds: sandbox + caps + explicit approval
- **Status:** Proposed (pending **S5 / PR-8**).
- **Context:** Local builds run Nixpkgs build scripts on the host; sandboxing is the control.
  (`04`,`08` T-BUILD-1/3)
- **Decision:** Local builds (Linux **and** macOS, native system only) are permitted **only** after an explicit, non-default user approval following a deterministic closure/derivation preview; `sandbox=true`/`sandbox-fallback=false`, platform-appropriate caps (cgroups/RLIMIT on Linux; RLIMIT/disk/load guards on macOS — no cgroups invented), build-user isolation (`nixbld`/`_nixbld`), and fail-closed readiness verification. macOS shares this policy per DR-003 (no longer binary-only). Approval never overrides a hard policy refusal (unsupported/broken/impure derivation, or sandbox/build-user unavailable).
- **Consequences:** Users opt into build risk knowingly; sandbox escapes remain a residual.
- **Owner:** E + A. **Source:** `[NIX-MANUAL]` (sandboxed builds); spike S5.

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
  and GC roots. Nix profile state is ignored/mirrored, not trusted.
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

---

## 2. Go / No-Go Spikes (S1–S5)

| Spike | Question | Success criterion | Blocks (no-merge before DR) | Decision deadline | Status | DR |
|-------|----------|-------------------|------------------------------|-------------------|--------|----|
| **S1** (PR-4) | Is `/nix/store` viable for exclusive managed use, and how do we detect/safely refuse unmanaged Nix? | Concrete layout + detection method + refusal text validated on Linux & macOS; go/no-go on alternative prefix. | PR-9, PR-12, PR-27, PR-28 | end of M0.5 | Proposed | DR-001 |
| **S2** (PR-5) | Does real TUF via `tough` express our target set + revocation + threshold? | Signed fixture metadata verified end-to-end; revocation dry-run passes; threshold demo. | PR-11, PR-33 | end of M0.5 | Proposed | DR-002 |
| **S3** (PR-7) | Do v1 attrs substitute for `x86_64-darwin`/`aarch64-darwin`, **and** are native macOS local builds viable (sandbox, `_nixbld` users, Xcode toolchain, resource caps, fail-closed)? Is a notarized installer feasible? | Availability matrix for fixture attrs; a real native sandboxed Darwin build under `_nixbld`; notarized installer/runtime builds & validates. | PR-26, PR-28, PR-36 (macOS lane) | end of M0.5 | Proposed | DR-003 |
| **S4** (PR-6) | What are realistic resolve/reeval costs and index-build costs? | Measured time/memory table; proposed budgets. | PR-14, PR-16, PR-32 | end of M0.5 | Proposed | DR-004 |
| **S5** (PR-8) | Does sandbox+caps+approval work for local builds on **both Linux and macOS**? | Sandboxed build blocked from network on both; cgroups/RLIMIT effective (Linux) and RLIMIT/guards effective (macOS); `_nixbld` build users ready; approval + fail-closed demonstrable. | PR-26, PR-30 | end of M0.5 | Proposed | DR-005 |

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
| **RISK-07** | Local-build sandbox escape / resource exhaustion (Linux **and** macOS) | T-BUILD-1/2/3 | L | H | M | E | `sandbox=true`/`sandbox-fallback=false` on both, RLIMIT/cgroups (Linux)/RLIMIT+guards (macOS), seccomp where feasible, `nixbld`/`_nixbld` builder-user isolation, fail-closed readiness; macOS shares this surface per DR-003 | M | kernel CVEs; macOS sandbox narrower than Linux; build telemetry |
| **RISK-08** | Path/symlink attack on state or activation | T-PATH-1/2/3/4 | M | H | H | E | 0700 dirs, O_NOFOLLOW, openat, store-relative symlinks, ancestor-perm checks | L | security lane (AC-S4) |
| **RISK-09** | State tampering / concurrent-writer corruption | T-STATE-1/2/3/4, T-CONC-1/2 | M | H | H | E | integrity anchor (DR-013), flock+lease+journal, atomic current, fail-closed | L | fault lane (AC-S2/S5) |
| **RISK-10** | Privileged helper privilege escalation / unauth IPC | T-INST-3/5, T-DAEMON-1 | L | H | H | E | caller auth, allowlist, prefer polkit/launchd over setuid, drop privs | L | security lane (AC-S6) |
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
| Runtime app isolation | ❌ none | firejail/bwrap integration → v2 | DR-009, RISK-01 |
| Vuln/CVE feed | ❌ none | integration → v2 | RISK-01 |
| Attribute denylist | ❌ none (re-pin only) | per-attribute blocking → v2 | RISK-20 |
| Telemetry | opt-in, minimal | richer usage analytics → later | DR-012 |
| Windows | ❌ | not in v1 | — |
| Threshold root signing | 1-of-1 v1, 2-of-3 GA | higher thresholds → post-GA | DR-002 |
| Project license (source) | ❌ none (all rights reserved) | chosen license + SPDX headers → when DR-015 is superseded | DR-015 |
| aarch64-linux CI | nightly (runner TBD) | native runners → when justified | RISK-19 |

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
| `05` | state/locks/gen/GC feed DR-008/013 and RISK-08/09. |
| `06`,`07` | UX/installer feed DR-003/007/012 and RISK-06/10/15. |
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
