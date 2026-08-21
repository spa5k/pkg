# `pkg` Plan Set — Canonical Entrypoint

> **Status:** Active V1 plan and implementation navigator. This `README.md` is the
> **index and navigator** for the reconciled plan set `00`–`13`. It owns no new decisions;
> it summarizes and links. Every binding decision lives in
> [`00-overview-and-decisions.md`](00-overview-and-decisions.md); every open go/no-go question
> lives in [`12-open-decisions-and-risks.md`](12-open-decisions-and-risks.md).

---

## 1. What `pkg` is (V1 summary)

`pkg` (working codename) is a single **Rust** binary providing a **brew-/paru-style
imperative package workflow** — `search`, `info`, `install`, `remove`, `list`, `outdated`,
`update`, `upgrade`, `pin`/`unpin`, `history`, `rollback`, `gc`, `repair`, `doctor`,
`completion` — built on top of a **fully hidden, bundled, product-managed Nix**.

The load-bearing V1 design choices (full set: **D-01…D-19** in doc 00 §7):

- **Nix is invisible and unconfigurable.** Users never type or configure raw Nix; no flakes,
  overlays, `NIX_PATH`, `--impure`, user substituters, or trust keys reach the daemon
  (**D-01**, **INV-03**). `pkg` drives Nix **only as a subprocess over JSON** — it does not
  link Nix's C++ (**D-02**).
- **Exclusive managed ownership of `/nix`.** `pkg` bundles and pins a `nix-daemon` + store at
  `/nix/store`; if it detects any *unmanaged* Nix it **fails closed** and **never auto-deletes**
  user state (**D-03**, **D-04**, **INV-01/02**).
- **Exactly pinned Nixpkgs + a disposable derived index.** The catalog is one pinned Nixpkgs
  revision (**D-05**); search/list/info use a regenerable index that is **never authoritative**
  for what can be installed (**D-06**); install **re-evaluates the exact attribute on host**
  (**D-07**).
- **A small signed channel descriptor distributed via real TUF** pins the Nix runtime, Nixpkgs
  rev/narHash, index hashes, substituters/keys, supported systems, and policy/sequence/expiry
  (**D-08**, **D-09**). `cache.nixos.org` is the **only** artifact cache in V1 (**D-10**).
- **Rust owns activation as a store-independent symlink forest.** Each generation's activation
  is a deterministic **symlink forest** under `<user-state>/activations/gen-<id>/` that `pkg`
  materializes **outside `/nix/store`** and that invokes **no Nix**; `current` is a relative
  symlink to it, a `treeDigest` binds the path→target records, and the broker is the sole
  **mediator/requester** for **one root set per selected output** (the privileged root-helper/service
  is the sole filesystem writer that atomically publishes it before the `current` swap; the broker
  is an allowed-user, not a trusted-user). V1 collisions resolve as
  `abort`/`keep-first`/`keep-last` only (no `keep-all`/`--force`) (**D-18**, **INV-11**).

**Platforms:** `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin` (**D-14**).
Cache substitution is first and preferred on **every** platform; on a cache miss, v1 may
build locally for the host's **native** Nix system on **both Linux and macOS**, only after a
deterministic build preview and explicit single-operation approval (evaluation/planning
never realize outputs — the exact pinned installable is evaluated with import-from-derivation
disabled, and `nix build` begins only at acquire as pure substitution first, then an approved
local build), under `sandbox=true`/
`sandbox-fallback=false` and the daemon's unprivileged build users (`nixbld` / `_nixbld`). A
machine-global local-build admission lease (distinct from per-user state leases) serializes
approved local builds across users. No
Rosetta, cross-compilation, emulation, or remote builders in v1 (**D-11**, **INV-08**).

**Multi-user ownership split (D-17 / INV-10):** immutable service/trust assets and
machine-global ancestors are **root-owned and shared**; the broker owns two private domains:
its `0700` home and mutable authenticated channel/index/source datastore leaves, and the separate
`0700` raw-log leaf (`/var/lib/pkg/log/broker` on Linux and `/Library/Application Support/pkg/log/broker` on macOS); the
**authoritative package environment state** (manifest, lock, generations, activation, journal)
is **per-user, keyed by OS uid**, owned by that user. Two narrow, distinct privilege boundaries exist (D-19): an **unprivileged singleton broker**
(authenticates the caller, is the **sole general Nix daemon client and sole spawner of the bundled `nix` CLI for all normal operations**, and the sole
**mediator/requester** for per-output GC-root operations and for asking the helper to run repair; it also hosts the **broker-internal in-memory build/GC admission gates** — not backing-file `flock`s, while the per-user state-mutation lease remains a filesystem `flock`) and a **narrow
privileged root-helper/service** (the **sole root-set filesystem writer** that atomically publishes/removes per-output root sets, handles service control / runtime upgrade / `/nix` ownership, and is the **sole fixed local-store repair executor** running two-phase `nix --store local store repair` as root — the user CLI never calls it directly). Nix 2.34.8's daemon protocol rejects `repairPath` even for root. The raw Nix daemon socket is never
exposed to users; the broker is an **allowed-user but not a trusted-user** (root is the sole
trusted-user; detail in docs 01/07/08). `pkg repair` is **user-initiated and verified non-atomic** (INV-12): cache repair deletes the live path before restore and local repair moves the old output aside before replacement, so `pkg repair` warns affected commands may be temporarily unavailable, journals per path, and marks a path repaired only after a final read-only verify (raw Nix logs are **service-private**; public/user-state logs are sanitized NDJSON). This hidden-Nix broker boundary is **accepted**; its
detailed framed RPC / peer-auth / operation-lifecycle / child-containment / capability-storage+expiry / restart-handshake contract is fixed by **PR-39** and documented normatively in doc 13. Real OS transports and Real-Nix execution remain downstream work (DR-017).

---

## 2. Status legend (used across all plan documents)

| Mark | Meaning |
|---|---|
| ✅ **Confirmed** | Current, verifiable Nix/Nixpkgs behavior with a primary-source citation. |
| 🛠 **Decision** | A `pkg` product design choice (`D-NN`) that constrains implementation. |
| ⚠️ **Spike** | Requires a short verification spike before commitment; a default is stated (`S1`–`S5`). |

Document status line is always **Draft (planning only — no implementation code)**.

---

## 3. Recommended reading order

1. [`00-overview-and-decisions.md`](00-overview-and-decisions.md) — **read first.** Decisions,
   invariants, glossary, CLI surface, cross-doc map.
2. [`01-system-architecture.md`](01-system-architecture.md) — layers, subprocess contract,
   directory layout, data-contract names.
3. [`02-trust-and-update-model.md`](02-trust-and-update-model.md) — TUF layout, channel
   descriptor schema, update/runtime-upgrade flows.
4. [`03-nixpkgs-source-and-index.md`](03-nixpkgs-source-and-index.md) — source pinning, index
   schema, the authoritative on-host install-eval.
5. [`04-resolution-install-build.md`](04-resolution-install-build.md) and
   [`05-state-locks-generations-gc.md`](05-state-locks-generations-gc.md) — the execution
   track (pipeline + state machine; read together).
6. [`06-cli-and-user-experience.md`](06-cli-and-user-experience.md) and
   [`07-platform-installation-and-runtime.md`](07-platform-installation-and-runtime.md) — the
   user + platform track.
7. [`08-security-model.md`](08-security-model.md) → [`09-testing-and-validation.md`](09-testing-and-validation.md)
   → [`10-release-and-operations.md`](10-release-and-operations.md) — the assurance track, in
   order.
8. [`11-pr-roadmap.md`](11-pr-roadmap.md) and [`12-open-decisions-and-risks.md`](12-open-decisions-and-risks.md)
   — the build plan and the living registry of decisions, risks, and go/no-go spikes.
9. [`13-broker-helper-contract.md`](13-broker-helper-contract.md) — the accepted framed-RPC,
   maintenance capability, admission, containment, and restart contract downstream transports implement.

---

## 4. Document map — what each owns and consumes

| # | Document | **Owns** (authority) | **Consumes** (inputs) |
|---|---|---|---|
| 00 | [Overview & Decisions](00-overview-and-decisions.md) | All `D-NN`/`INV-NN`, glossary, scope, platform set, CLI surface, spike IDs | (root — nothing) |
| 01 | [System Architecture](01-system-architecture.md) | Layered components, **Nix subprocess contract** (§11), **state-directory layout** (§9), data-contract *names*, privilege model | 00 decisions |
| 02 | [Trust & Update](02-trust-and-update-model.md) | **Channel descriptor schema** (§7), **TUF role/target layout** (§6.4), update + runtime-upgrade flows, trust bootstrap | 00, 01 |
| 03 | [Nixpkgs Source & Index](03-nixpkgs-source-and-index.md) | Source fetch+verify, **index schema** (§7), **install-eval contract** (§9), `packages.json.br` relationship | 00, 01, 02 |
| 04 | [Resolution/Install/Build](04-resolution-install-build.md) | Resolve→preflight→acquire→verify→stage→activate→**commit** pipeline, build preview/approval, sandbox/caps | 00, 01, 02, 03 |
| 05 | [State/Locks/Gen/GC](05-state-locks-generations-gc.md) | **Authoritative state model**, schema migrations, journal, generations, atomic `current`, GC roots, `gc`/`repair` | 00, 01, 04 |
| 06 | [CLI & UX](06-cli-and-user-experience.md) | Per-command flags/exit codes, `--json` + progress protocol, approvals, error rendering | 00, 01, 04, 05 |
| 07 | [Platform Install/Runtime](07-platform-installation-and-runtime.md) | Bundled runtime, generated `nix.conf`, installers, daemon supervision, unmanaged-Nix refusal, uninstall | 00, 01, 02, 08 |
| 08 | [Security Model](08-security-model.md) | Threat catalog (T-*), trust boundaries, control matrix, crypto/key policy | 00–07 |
| 09 | [Testing & Validation](09-testing-and-validation.md) | Test pyramid, `FakeNix`/`pkg-testkit`, parity job, CI lanes, release gates | 00–08 |
| 10 | [Release & Operations](10-release-and-operations.md) | Artifacts/topology, release gates, key custody, channel ops, compat policy, incident runbooks | 00–09 |
| 11 | [PR Roadmap](11-pr-roadmap.md) | PR DAG (PR-0…PR-39), milestones, parallelism matrix, critical path, guardrails | 00–10 |
| 12 | [Open Decisions & Risks](12-open-decisions-and-risks.md) | **DR-*** decision records, **S1–S5** spike registry, **RISK-*** register, v1 defaults/deferrals | 00–11 |
| 13 | [Broker/Helper Contract](13-broker-helper-contract.md) | Framing, peer auth, lifecycle, `MaintenanceAdapter`, capabilities, admission, containment, restart | 00, 01, 04, 05, 08, 09, 11, 12 |

> Schema-authority rule (from 00 §7): the *canonical JSON shape* of each artifact is owned by
> exactly one doc; **doc 05 owns all versioning/migrations**. Doc 01 owns the *names*;
> doc 02 owns `descriptor.json`; doc 03 owns index records.

---

## 5. System dependency diagram

```mermaid
flowchart LR
  subgraph Net["Untrusted transport (authenticated by hash/signature)"]
    TUF["pkg TUF repo<br/>(signed metadata)"]
    REL["releases.nixos.org<br/>(Nix tarballs)"]
    GH["github.com/NixOS/nixpkgs @ rev"]
    CACHE["cache.nixos.org<br/>(substituter — D-10)"]
    PJ["packages.json.br<br/>(optional — D-06)"]
  end

  subgraph Usr["User surface (D-01: Nix hidden)"]
    CLI["pkg CLI (brew/paru-style)"]
  end

  subgraph Core["pkg (Rust) — owns state, locks, generations, activation, GC roots (D-12)"]
    CMD["command services"]
    DOM["domain: manifest / lock / generations"]
    DRV["nix-driver adapter<br/>(JSON-only, ARCH-INV-01)"]
    UP["updater (TUF client, D-09)"]
    IDX["index builder/loader (D-06)"]
    ACT["activator (symlink forest outside store, no Nix; treeDigest)"]
  end

  subgraph Priv["Managed Nix runtime (D-02/D-03) — root-owned service, multi-user (§8 doc 01)"]
    BROK["private broker (unprivileged singleton)<br/>(sole GENERAL daemon client & sole bundled-CLI spawner;<br/>sole GC-root/repair mediator; in-memory build/GC admission; D-18/D-19)"]
    HELP["root-helper/service<br/>(SOLE root-set FS writer; ONE two-phase repair maintenance client;<br/>non-setuid; D-19)"]
    DAEM["nix-daemon (bundled, pinned)"]
    STORE[("/nix/store (owned by pkg, INV-02)")]
  end

  CLI --> CMD --> DOM
  DOM --> DRV --> BROK --> DAEM --> STORE
  CMD --> UP --> TUF
  UP -. pins .-> REL
  UP -. pins .-> GH
  IDX --> GH
  IDX -. optional .-> PJ
  DRV -. substitute+verify sig .-> CACHE
  DOM --> ACT -. points at .-> STORE
  ACT -. per-output root-set requests .-> BROK -. atomic publish via .-> HELP
```

---

## 6. Install transaction sequence (canonical happy path)

Derived from doc 04 §5 (pipeline), doc 01 §12.2, and the canonical crash-consistency
contract in doc 05 §8.4. The generation transaction creates the **per-output GC roots
before** the atomic `current` swap, so the swap always lands on a durably-rooted,
fully-documented **forest**; the **committed journal row is appended after** the swap.
The operation lease (doc 05 §12) protects the staged forest only during stage→rooted
(before the roots exist) and serializes `gc` for the whole transaction.

```mermaid
sequenceDiagram
  participant U as user
  participant CLI as pkg CLI
  participant R as resolver
  participant N as nix-driver → broker → daemon
  participant DOM as domain (manifest/lock/gen)
  participant ACT as activator
  participant BROK as private broker (→ daemon)
  U->>CLI: pkg install ripgrep
  CLI->>R: resolve(ripgrep, currentSeq, system)
  R->>N: nix derivation show --recursive <exact pinned installable>   (evaluate-only; JSON unconditional; NO realization)
  N-->>R: recursive derivation graph (v4 envelope) + expected output paths
  CLI->>N: preflight: cache NarInfo traversal over the full closure (exact known bytes for cache-present paths; absent NarInfo + buildable drv ⇒ build required)
  N-->>CLI: cache classification + sanitized BuildPreview (buildPlanDigest pointer to the private canonical plan)
  CLI->>CLI: [build required?] approval gate on the canonical BuildPlan digest, then acquire (post-approval): substitute per D-10 and/or approved native build per D-11
  CLI->>N: verify: store verify + path-info (narHash/sigs)
  CLI->>ACT: stage: materialize symlink forest <user-state>/activations/gen-N (path→store-target; treeDigest) — NO Nix (D-18)
  CLI->>DOM: prepared: write generations/gen-N.json + fsync (immutable metadata)
  CLI->>BROK: rooted: broker mediates per-output root-set publish under gcroots/pkg/users/<uid>/ (privileged writer fsyncs; D-17/D-18)
  CLI->>ACT: activated: atomic swap current → activations/gen-N (relative symlink, D-16)
  CLI->>DOM: write manifest.json + lock.json (fsync) to match gen-N
  CLI->>DOM: committed: append committed row to journal (fsync)
  DOM-->>CLI: committed generation N
  CLI-->>U: installed ripgrep 14.1.0
```

> Failure before the `current` swap **discards the prepared/rooted generation and leaves
generation N-1 active** (D-16); the staged forest/per-output roots are unreachable from
`current` and recovery deletes them. Failure after the swap leaves generation N rooted and documented;
recovery finalizes `manifest`/`lock` + the `committed` row. Cancellation is
SIGTERM-to-subprocess-group + lease release + staging cleanup (doc 04 §9, doc 05 §8.4).

---

## 7. Machine-global vs per-user ownership (D-17 / INV-10)

| Artifact / location | Scope | Owner | Rationale |
|---|---|---|---|
| `/nix/store` + `/nix/var/nix/*` | machine-global | root (exclusive, INV-02) | Fixed store prefix; cache.nixos.org path hashing. |
| `/opt/pkg/{bin,nix/<ver>}` runtime | machine-global | root (read-only to users) | Bundled pinned `nix`; `nix/current` atomic swap (doc 02 §10). |
| `/var/lib/pkg/` service ancestor and immutable trust/config assets | machine-global | root | Protects service and trust boundaries; users cannot list private state. |
| `/var/lib/pkg/broker-home/channel/{tuf,descriptor.json}` | machine-global | broker, private `0700` | Mutable authenticated channel state; embedded trust bootstrap remains root-owned. |
| `/var/lib/pkg/broker-home/{index/<seq>,nixpkgs/<rev>}` | machine-global | broker, private `0700` | Disposable authenticated index and verified pinned source data. |
| `/var/lib/pkg/log/broker` | machine-global | broker, private `0700` | Separate raw adapter/Nix log and authority-audit leaf. |
| `/var/lib/pkg/cache` and `/var/lib/pkg/log` ancestor | machine-global | root | Service downloads and protected log ancestry. |
| `/nix/var/nix/gcroots/pkg/users/<uid>/*` (one per selected output) | **per-user (uid)** | root-owned symlinks, uid-scoped dir | One GC root per selected output, atomically published by the privileged root-helper/service (broker-mediated) before the `current` swap (D-18, ARCH-INV-06). |
| `<user-state>/manifest.json` | **per-user (uid)** | that uid, 0700 | Desired state — authoritative. |
| `<user-state>/lock.json` | **per-user (uid)** | that uid, 0700 | Realized state — authoritative. |
| `<user-state>/generations/`, `current`, `journal/`, `logs/` | **per-user (uid)** | that uid, 0700 | Generation history + activation pointer + journal. |
| `<user-state>/activations/gen-<id>/` (+ `current` → it) | **per-user (uid)** | that uid, 0700 | Rust-materialized symlink forest (entries point into `/nix/store`/sources); activation invokes no Nix; `treeDigest`-bound (D-18). |
| `$XDG_CONFIG_HOME/pkg/config.toml` | **per-user (uid)** | that uid | Prefs **only** — cannot override trust/substituters/store (INV-03). |

`<user-state>` = `$HOME/.local/share/pkg/` (Linux) or
`~/Library/Application Support/pkg/` (macOS), where HOME is the authenticated uid's
system/passwd home. `XDG_DATA_HOME` is not authoritative in this alpha because the
broker, helper, root authorization, and uninstall bind the per-uid namespace to that
home. There is no fallback root. Explicit alternate roots are read-only inspection
origins (doc 01 §9.3, doc 05 §4).

---

## 8. Authoritative source vs derived state

| Thing | Status | Authority |
|---|---|---|
| Channel descriptor (`descriptor.json`) | **Authoritative** | TUF targets metadata; `pkg` cross-checks descriptor fields *and* TUF target hashes (defense in depth, doc 02 §12). |
| TUF metadata (root/timestamp/snapshot/targets) | **Authoritative** | Embedded `root.json` is the only a-priori trust anchor (doc 02 §8). |
| Pinned Nixpkgs source | **Authoritative** | `descriptor.nixpkgs.{rev,narHash}`; verified via `nix flake metadata` (CAT-INV-01). |
| **Index** records | **Derived / disposable** | Regenerable from Nixpkgs; hash-pinned in descriptor; **never** a source of store paths or realizability (D-06/07, INV-07). |
| On-host install evaluation | **Authoritative for realizability** | `nix build`/`path-info` on host is the *only* statement a package is realizable here (CAT-INV-03). |
| `manifest.json` (desired state) | **Authoritative (user-owned)** | User intent; per-user, keyed by uid. |
| `lock.json` (realization) | **Authoritative (user-owned)** | Exact realized identities; `storePath` is the join key (D-13). |
| Generation record | **Authoritative snapshot** | Immutable (manifest + lock + activation.treeDigest + forest path) snapshot; content-hashed (doc 05 §5.3). |
| `cache.nixos.org` narinfo/signatures | **Authoritative for path integrity** | Nix verifies substituted paths against the descriptor-pinned key (D-10). |
| Nix profile state | **Not authoritative** | Ignored/mirrored, never trusted (D-12, DR-008). |
| `pname@version` | **Display metadata only** | Never a key (D-13, INV-06). |

---

## 9. Canonical data-contract flow

```mermaid
flowchart TD
  TUF["TUF metadata<br/>root → timestamp → snapshot → targets<br/>(doc 02 §6.4)"]
  DESC["descriptor.json<br/>sequence / policyVersion / expiry / supportedSystems<br/>nixRuntime · nixpkgs.rev+narHash · index.perSystem.sha256 · substituters/keys<br/>(doc 02 §7)"]
  SRC["Nixpkgs source @ rev<br/>fetched + narHash-verified (CAT-INV-1)<br/>(doc 03 §6)"]
  IDX["Index per (seq,system)<br/>disposable, sha256-verified (CAT-INV-2)<br/>(doc 03 §7)"]
  RES["Resolver: selector → attrPath<br/>(doc 03 §9.1, 04 §5.1)"]
  EVAL["On-host eval → realization<br/>storePath / drvPath / narHash / outputs<br/>(CAT-INV-3, doc 03 §9.2)"]
  LOCK["lock.json — realization per entry id<br/>(doc 01 §10.2 / 05 §5.2)"]
  GEN["generation gen-N.json<br/>manifestHash + lockHash + activation.treeDigest + forest path<br/>(doc 01 §10.3 / 05 §5.3)"]
  ACT["activation = symlink forest under activations/gen-N/<br/>current → it (relative symlink, atomic D-16)<br/>(doc 04 §5.5, D-18)"]
  ROOT["GC roots (one per selected output)<br/>gcroots/pkg/users/<uid>/* → outputs<br/>(doc 05 §8.3, ARCH-INV-06, D-18)"]

  TUF -->|verifies| DESC
  DESC -->|pins rev+narHash| SRC
  DESC -->|pins index sha256| IDX
  SRC --> IDX
  IDX --> RES
  SRC --> EVAL
  RES --> EVAL
  EVAL --> LOCK
  LOCK --> GEN
  GEN -->|prepared: gen-N.json fsynced| ROOT
  ROOT -->|rooted (GC root) → activated (current swap)| ACT
```

Key invariant: a corrupt/absent **index** never blocks a known-attribute install (it re-evaluates
on host); a corrupt/stale **descriptor** is a hard stop requiring `pkg update` (doc 00 §14,
doc 03 §12).

---

## 10. Command-to-subsystem routing

`channel` = TUF/descriptor · `index` = search catalog · `resolve` = selector→realization ·
`pipeline` = resolve→preflight→acquire→verify→stage→activate→commit · `state` = manifest/lock/
generations · `store` = activation + GC roots + verify/gc.

| Command | channel | index | resolve | pipeline | state | store | Owned detail |
|---|:--:|:--:|:--:|:--:|:--:|:--:|---|
| `doctor` | read | — | — | — | read | check | [06 §6.1](06-cli-and-user-experience.md) |
| `update` | **✓** | (lazy fetch) | — | — | — | — | [06 §6.8](06-cli-and-user-experience.md), [02 §9](02-trust-and-update-model.md) |
| `search` | — | **✓** | — | — | — | — | [06 §6.2](06-cli-and-user-experience.md) |
| `info` | — | **✓** | (candidates) | — | — | — | [06 §6.3](06-cli-and-user-experience.md) |
| `install` | read seq | — | **✓** | **✓** | stage+commit | **✓** | [06 §6.4](06-cli-and-user-experience.md), [04 §5](04-resolution-install-build.md) |
| `remove` | — | — | — | **✓** | stage+commit | **✓** | [06 §6.5](06-cli-and-user-experience.md) |
| `list` | — | — | — | — | read lock | — | [06 §6.6](06-cli-and-user-experience.md) |
| `outdated` | read seq | **✓** | — | — | read lock | — | [06 §6.7](06-cli-and-user-experience.md) |
| `upgrade` | read seq | — | **✓** | **✓** | stage+commit | **✓** | [06 §6.9](06-cli-and-user-experience.md), [05 §7](05-state-locks-generations-gc.md) |
| `pin`/`unpin` | — | — | — | — | edit manifest | — | [06 §6.10](06-cli-and-user-experience.md) |
| `history` | — | — | — | — | read gens | — | [06 §6.11](06-cli-and-user-experience.md) |
| `rollback` | — | — | — | rebuild act. | new gen + `current` | **✓** re-root | [06 §6.12](06-cli-and-user-experience.md), [05 §8](05-state-locks-generations-gc.md) |
| `gc` | — | — | — | — | prune gens | **✓** `nix store gc` | [06 §6.13](06-cli-and-user-experience.md), [05 §9](05-state-locks-generations-gc.md) |
| `repair` | — | — | — | re-acquire | read | **✓** `nix store verify` | [06 §6.14](06-cli-and-user-experience.md), [05 §10](05-state-locks-generations-gc.md) |
| `completion` | — | — | — | — | — | — | [06 §6.15](06-cli-and-user-experience.md) |

---

## 11. Failure / recovery boundary summary

Policy is fixed in doc 00 §14; per-operation matrices live in doc 01 §13, doc 04 §11, doc 05
§13, and the full model in doc 08. Core rules:

- **Never break the working environment (D-16):** any failed install/upgrade/rollback leaves
  the previously-committed generation active via the atomic `current` swap; staged temp state
  is discarded.
- **Never trust unverifiable bytes (INV-09):** any hash/signature/sequence failure aborts the
  operation; no silent fallback to unsigned data.
- **Fail closed, never auto-delete (D-04):** foreign-Nix detection or store-ownership
  ambiguity refuses and prints manual remediation; user data is never destroyed automatically.
- **Disposables are disposable (INV-07):** a corrupt/missing index never blocks a known-attr
  install (re-evaluates on host); a corrupt/stale descriptor is a hard stop requiring
  `pkg update`.
- **Restart-safe:** an interrupted op is detected via the journal/lease and resumes from the
  last committed generation — never from a half-built activation tree (doc 04 §9, doc 05 §11).
- **GC safety (INV-05):** every realized output pinned in a retained generation has a GC root;
  `gc` prunes generations *before* running `nix store gc` so only rooted closures survive.

---

## 12. Security / trust boundary summary

Five trust boundaries crossed in normal operation (full catalog in doc 08 §2.2, threats T-* in
doc 08 §6, control matrix in doc 08 §8):

1. **Internet → product CDN → channel client.** Signed TUF metadata must verify to the embedded
   root; anti-rollback/freeze via sequence + timestamp expiry (T-CHAN-*).
2. **Internet → cache.nixos.org → store.** Substituted paths verify against the
   descriptor-pinned key only; users cannot change substituters/keys (D-10, T-CACHE-*).
3. **User space → privileged (private broker / root-helper-service).** Caller is authenticated by
   uid; strict serde, size caps, no expression/substituter passthrough; the broker (an
   **allowed-user, not a trusted-user**; root is the sole trusted-user) is the **sole general**
   client of the private daemon and the sole spawner of the bundled `nix` CLI for normal ops, the
   sole **mediator/requester** for per-output GC-root operations and for helper-run repair, and the
   host of the **broker-internal in-memory build/GC admission gates**; the **narrow privileged
   root-helper/service** is the sole root-set filesystem writer that atomically publishes/removes
   per-output root sets under uid-scoped dirs **and the sole fixed local-store repair executor** running
   two-phase `nix --store local store repair` as root (the user CLI never calls it directly; raw Nix logs are
   service-private); the raw Nix daemon socket is never exposed to users (T-DAEMON-*, T-INST-*,
   T-PATH-*).
4. **User space → host FS (state, `~`).** Per-user 0700 state dirs; `O_NOFOLLOW`/`openat`;
   store-relative symlinks; atomic writes; tamper-evident journal anchored to the channel
   (DR-013, T-STATE-*, T-PATH-*).
5. **Store → user runtime (PATH).** Activation maps provenance → executed code. V1 provides
   **provenance + reproducibility + fast catalog revocation, not runtime isolation** (DR-009,
   T-RUN-1, honest disclosure).

Primary goals G1–G6 (doc 08 §3): authenticity of catalog/runtime; integrity of installed
software; anti-rollback/anti-freeze; state integrity & recoverability; least privilege; no
silent privilege escalation/persistence. Top watch-list risks (doc 12 §3.1): malicious/insecure
pinned package (RISK-01/20), cache.nixos.org single-source compromise (RISK-02), store-prefix
coexistence (RISK-04), crypto correctness (RISK-05), supply chain (RISK-13).

---

## 13. PR roadmap overview

Per-PR detail, the full DAG, reviewer model, and per-PR gates live in
**[`11-pr-roadmap.md`](11-pr-roadmap.md)** (DAG §3, entries §4, milestone/exit map §7,
guardrails §9). Go/no-go decisions, the DR registry, and the risk register live in
**[`12-open-decisions-and-risks.md`](12-open-decisions-and-risks.md)** (DRs §1, spikes §2,
risks §3, defaults/deferrals §4).

**Milestones → PRs:**

| Milestone | PRs | Theme | Exit criteria (summary) |
|---|---|---|---|
| **M0 Foundations** | 0–3 | repo, CI/lint/deny, `pkg-core` types, `NixAdapter`+`FakeNix`+Fast CI | workspace builds; Fake Nix + Fast CI green; JSON contract stable |
| **M0.5 Spikes** | 4–8 | S1–S5 (parallel, gated) | all five DRs **Accepted** before irreversible architecture |
| **M1 Managed Nix & state** | 9–12 | unmanaged-Nix detection, state schema v1+journal, channel descriptor+`tough`, provision Nix | detect/provision in temp root; state v1 landed; channel verify on fixtures |
| **M1.5 Broker/helper contract & capability** | 39 | Early BLOCKING design/contract milestone: broker↔CLI/broker↔helper framed RPC, peer auth, operation lifecycle, child containment, capability storage/expiry, restart handshake, **and the broker-internal in-memory admission gates (machine-wide build admission lease + GC admission gate / GC-inhibit permit, AC-S19/S23)** — **designed + reference-implemented on FakeNix/in-process** (real OS transports land in PR-27/28); adapter split enforced | **unconditional blocking gate** for PR-27/28 (broker/helper integration), PR-30 (two-phase repair), and PR-36 (Real Nix) — full two-phase mutating repair is an accepted V1 milestone (DR-017; non-atomic residual RISK-22); wire design lands here (DR-017) |
| **M2 Catalog & resolve** | 13–16 | fetch+verify Nixpkgs, deterministic index, query API, resolver | Selector→evaluated derivation plan under pure eval; realization deferred; narHash verified |
| **M3 Install/activate** | 17–20 | substitute+verify, GC roots+activation+`current`, install pipeline, remove/upgrade+mixed-rev | install w/ rollback-on-failure; mixed revisions |
| **M4 Generations & UX** | 21–25 | generations/history/rollback/pin, GC+leases, CLI skeleton, wired commands, completion+doctor | full CLI wired to Fake Nix |
| **M5 Local build & installers** | 26–29 | Cross-platform local build (sandbox+approval, Linux + macOS native), Linux + macOS installers, uninstall | installers + bounded uninstall |
| **M6 Hardening & ops** | 30–35 | repair (needs M1.5/PR-39), security lane, perf gate, release signing, observability, docs/support | all G-* lanes green on fixtures |
| **M7 Technical Preview** | 36 | Real-Nix nightly CI, Fake↔Real parity, self-hosted e2e | Real-Nix e2e green on Linux x86_64 + macOS arm64 |
| **M8 v1** | 37–38 | RC + revoke rehearsal + sign-off; v1.0 release | all release gates + advisory |

**Parallel workstreams (doc 11 §5):**

- **Spikes (M0.5):** PR-4 ‖ PR-5 ‖ PR-6 ‖ PR-7 ‖ PR-8 (all independent after PR-1).
- **M1:** PR-9 ‖ PR-10 ‖ PR-11 (state, detect, channel).
- **M1.5:** PR-39 (broker/helper contract) — after PR-3/PR-10; gates PR-27/28/30/36, so develop alongside M2/M3/M4 and land before M5.
- **M3→M4:** PR-20 ‖ PR-21 (lifecycle ops both consume PR-19).
- **M5:** PR-27 (Linux installer) ‖ PR-28 (macOS installer+build, depends on PR-26); PR-25 ‖ PR-26 ‖ PR-27.
- **M6:** PR-30 ‖ PR-31 ‖ PR-32 ‖ PR-33 ‖ PR-34 ‖ PR-35 (whole hardening batch).
- **CLI skeleton (PR-23)** only needs PR-2 → can start during M1/M2.

**Critical path (doc 11 §6):**

```
PR-0 → PR-1 → PR-2 → PR-3
  → PR-11 (needs S2/PR-5) → PR-12 (needs PR-9) → PR-13
  → PR-14 (needs S4/PR-6) → PR-16 → PR-19 (needs PR-18) → PR-24
  → PR-36 (needs PR-27/28/29/30/31/32/33) → PR-37 → PR-38

Early blocking contract gate (off the longest chain, but hard-blocking M5+):
PR-3 / PR-10 → PR-39 (M1.5) ── gates ──▶ PR-27/28 ──▶ PR-30 ──▶ PR-36
```

The **channel/TUF choice (S2)** and the **resolve→install chain** are on the longest critical
path; installers, local build, and most of M6 hardening run **off** it and parallelize.
**Store-prefix spike S1 must not slip past M0.5** (it gates PR-9/12/27). **PR-39 is repositioned
as an early blocking design/contract milestone (M1.5)**, right after the core-types/state
prerequisites (PR-3, PR-10) and before the broker/helper integration (PR-27/28), repair (PR-30),
and Real-Nix execution (PR-36) it gates. It **no longer depends on the installers** (the prior
PR-39→PR-27/28 edges were circular with PR-27/28→PR-39 and are removed), so it creates no
late-stage bottleneck and can be developed in parallel with M2/M3/M4; but no broker/helper
integration, repair, or Real-Nix work merges before its accepted ADR lands (DR-017).

---

## 14. Remaining go/no-go spikes (S1–S5)

These are **open** (DR-001…005 are `Proposed`, not `Accepted`) — listed here as unresolved, not
solved. No irreversible architecture merges before the corresponding DR is `Accepted`
(doc 11 §9). Full detail: [doc 12 §2](12-open-decisions-and-risks.md), [doc 00 §11](00-overview-and-decisions.md).

| Spike | PR → DR | Question (unresolved) | Default if spike confirms |
|---|---|---|---|
| **S1 — store prefix / runtime layout** | PR-4 → DR-001 | Can `pkg` exclusively own `/nix/store` under a managed daemon socket/state, with no V1 need for a relocatable store? How to safely detect/refuse unmanaged Nix? | Hard requirement on `/nix/store`; fail-closed otherwise (D-04). |
| **S2 — TUF fit / key custody** | PR-5 → DR-002 | Does real TUF via `tough` express the small target set (rev/narHash, Nix version, index hashes, substituters/keys, systems, policyVersion, sequence, expiry) with threshold + revocation? | Use `tough`; 1-of-1 v1 → 2-of-3 at GA; offline root (DR-002). |
| **S3 — macOS cache coverage** | PR-7 → DR-003 | Do v1 attrs substitute for `aarch64-darwin`/`x86_64-darwin`? Is a notarized+signed installer feasible, and are native macOS local builds (sandbox, `_nixbld` build users, Xcode toolchain, the honest resource boundary with **no** per-build cap in stock Nix) viable? | Darwin cache coverage + approved native sandboxed local builds; signed/notarized installer/runtime (DR-003). |
| **S4 — Nixpkgs reevaluation / index cost** | PR-6 → DR-004 | What are realistic single-attr realize + four-system index meta-eval costs? flake `narHash` vs raw GitHub archive hash difference? | Disposable index for browse; on-host re-eval for install; publisher precompute. |
| **S5 — managed-daemon sandbox / approval / resource-boundary** | PR-8 → DR-005 | Does `sandbox=true`/`sandbox-fallback=false` work under the managed daemon on both Linux and macOS? Can we intercept builds for preview/approval? What resource boundary actually holds (max-jobs bounds concurrency; daemon timeout/max-silent-time/max-build-log-size; disk/free-space/load preflight; `use-cgroups` is Linux cleanup/statistics, not caps; service-manager ceilings are Pending)? | Local builds (Linux **and** macOS, native) only after explicit single-operation approval + fail-closed readiness; honest resource boundary, no stock per-build cap (DR-005). |

> SPK-02 (Nix-as-subprocess JSON stability) is **not** a standalone spike: it is enforced
> continuously by pinning the managed Nix version and isolating all Nix output — including the
> nominally-internal `--log-format internal-json` — behind the single versioned `nix-driver`
> adapter, validated by the Fake↔Real parity job (doc 01 §11, doc 09 §4.3).

---

## 15. Initial implementation starting instructions

Mirror the PR DAG (doc 11). The first four PRs unblock everything else and contain **no Nix
dependency**, so they can begin immediately:

1. **PR-0 — Repo + CI + plans cross-links.** Establish the repo, this `plans/` directory as the
   source of truth, `CONTRIBUTING.md` (reviewer model from doc 11 §2), and a docs-linkcheck CI
   that fails on broken plan cross-references. *(No code.)*
2. **PR-1 — Cargo workspace, toolchain, lint/deny/audit + `pkg-core` scaffold.** The
   permanent workspace plus a real member crate (`pkg-core`) are required so `cargo build`/
   `check`/`clippy`/`doc`/`fmt` are operable (a memberless virtual workspace fails all of
   them). `Cargo.toml` (workspace), `Cargo.lock`, `rust-toolchain.toml`, `deny.toml`,
   `rustfmt.toml`, `clippy.toml`, `.github/workflows/ci-fast.yml` (G-LINT: `fmt`,
   `clippy -D warnings`, `doc`, `build`, `cargo deny check`, `cargo audit`), and the
   `crates/pkg-core/` scaffold (manifest + empty `lib.rs` only — no domain logic). The
   project license was deferred at this checkpoint; DR-015 now records Apache-2.0.
3. **PR-2 — `pkg-core` domain types & logic.** `identity`, `selector`, `realization`,
   `channel`, `version`, `system` — the intent-vs-realization vocabulary (D-13) and the
   display-only `pname@version` distinction, plus property tests for version compare +
   identity equality. May **extend** the `pkg-core` manifest/lib laid down in PR-1 (it does
   not re-create the scaffold).
4. **PR-3 — `NixAdapter` trait + JSON contract + `FakeNix` + Fast CI.** The seam that lets the
   entire core be built/tested **without Real Nix** (the JSON-only contract from doc 04, a trust
   boundary per doc 08 T-DAEMON-2). Fast-CI wires unit + contract (serde round-trips) on `FakeNix`.

**In parallel (after PR-1):** open the five spike PRs (**PR-4…PR-8**) so DR-001…005 land before
M1 architecture is locked. **PR-23 (CLI skeleton)** needs only PR-2 and can start in M1/M2.

**Hard ordering constraints (doc 11 §9):** no installer layout before S1; no channel
implementation before S2; no macOS build-security commitment (cache coverage **and** native local-build readiness) before S3; no local-build UX before
S5; no perf budgets before S4.

---

## 16. Reconciliation summary

This plan set was produced from parallel drafts and reconciled for cross-document consistency.
The non-obvious corrections encoded across `00`–`12`:

- **Mixed exact revisions (INV-04).** A generation may legitimately contain outputs realized
  from **multiple Nixpkgs revisions**: each selector pins `channel:current` |
  `channel:pinned:<id>` | `rev:<gitsha>`. **Every** such rev is exact/pinned, never floating.
  `upgrade` re-resolves only the named/non-pinned selectors at the current `channelSeq`
  (doc 05 §7, PR-20).
- **UID-scoped authoritative state (D-17 / INV-10).** Manifest/lock/generations/activation/
  journal are **per-user keyed by OS uid**, *not* a single shared profile. This supersedes the
  earlier single-shared-profile assumption (former UD-00.4). Root owns immutable service/trust
  assets and machine-global ancestors; the broker owns its private home/datastore and separate raw-log leaf. GC roots are scoped to
  `/nix/var/nix/gcroots/pkg/users/<uid>/` (doc 01 §9, doc 05 §4, PR-18, RISK-21).
- **Generation schema / per-output-GC-root-before-swap ordering.** The per-output GC roots are
  created **before** the atomic `current` swap (the **rooted** step precedes the **activated**
  step), and the `committed` journal row is appended **after** the swap (doc 04 §5.7, doc 05
  §8.4). All immutable generation metadata (`gen-N.json`) is fsynced first (**prepared**); the
  per-output GC roots are fsynced next (**rooted** — atomically published by the privileged
  root-helper/service, broker-mediated); then `current` is swapped (**activated**);
  then `manifest`/`lock` and the `committed` row are written. A crash before the swap leaves the
  previous generation active and the staged forest/roots unreachable (recovery deletes them); a
  crash after the swap always leaves a rooted, documented active generation. Nix treats any
  symlink in a scanned gcroots dir as a root, so **no** `--add-root` operation is used.
- **Activation as a store-independent symlink forest (D-18 / INV-11).** The activation tree is
  **not** a Nix `buildEnv` store object: `pkg` (Rust) materializes each generation's activation
  as a deterministic **symlink forest** under `<user-state>/activations/gen-<id>/` (entries
  point at `/nix/store` targets or approved sources) and `current` is a relative symlink to it.
  A `treeDigest` binds the sorted path→store-target/source records. Activation invokes **no
  Nix**; Nix owns downloads/builds/store only. The broker is the sole **mediator/requester** for
  **one root set per selected output** (not one per generation); the privileged root-helper/service
  is the sole filesystem writer that atomically publishes them before the `current` swap (the
  broker is an allowed-user, not a trusted-user; root is the sole trusted-user). V1 collision policies are
  `abort` (default) / `keep-first` / `keep-last` only — no `keep-all`, no `--force`;
  `keep-first`/`keep-last` pick a deterministic per-file winner while the losing package's
  other files remain visible. (doc 01 §10, doc 04 §5.5, doc 05 §8, doc 06 §6.4/§7, doc 07 §10;
  supersedes the prior `buildEnv` framing — `01`/`04`/`05` are all converted;
  there is no remaining E-track `buildEnv` reconciliation.)
- **Index authority (D-06 / D-07).** The index is **derived and disposable**, hash-pinned in
  the descriptor; it is **never** a source of store paths, narHash, or realizability. A
  corrupt/absent index never blocks a known-attr install (re-evaluates on host). `list` reads
  the **lock**, not the index (doc 03 §7.3/§11, doc 01 §10.2).
- **TUF target scope (doc 02 §6.4).** TUF targets are exactly: `descriptor.json`, the per-system
  **managed-Nix runtime tarballs**, and the per-system **index files**. The **pinned Nixpkgs source
  is fetched directly from `github.com/NixOS/nixpkgs` at the locked flake `rev`/`narHash`** (pinned
  *in* `descriptor.json`, narHash-verified via `nix flake metadata`, CAT-INV-01) — it is
  authoritative but **not** a product-published TUF source target. The **`pkg` CLI itself is NOT a
  TUF channel target** — it is published alongside with a pinned checksum + Sigstore attestation
  (doc 10 §2).
- **Closure preflight (doc 04 §5.2).** Download/build classification is over the **full
  recursive closure**, not the output root. A **single** closure path with an absent narInfo
  (and a non-empty builder) ⇒ build required ⇒ build preview. Preflight never reports
  "binary available" unless *every* closure path is a cache hit.
- **Tests and PR dependencies.** Test lanes (`G-*` in doc 09 §8) and release gates (doc 10 §3)
  are referenced as acceptance criteria by the PR DAG (doc 11); PR-0's docs-linkcheck makes
  broken DR/RISK/`D-NN`/`INV-NN` references fail CI. The **Fake↔Real parity job** (doc 09 §4.3)
  continuously attests the stable subprocess surface rather than treating Nix JSON stability as
  a one-off spike.
- **Broker/helper privilege split & two-phase repair (D-19 / DR-017).** The **unprivileged singleton
  broker** is the sole general daemon client and sole bundled-`nix`-CLI spawner for all normal
  operations (an `allowed-user`, never a `trusted-user`); the **privileged root helper** is the sole
  root-set filesystem writer **and the sole fixed local-store repair executor** running two-phase
  `nix --store local store repair` as root (Phase 0 read-only verify via broker → Phase A cache-only repair via
  helper, `max-jobs=0`/`builders` empty, stop-before-build → Phase B approved local repair via
  helper, bounded nonzero `max-jobs`/`builders` empty, broker build mutex + shared GC permit). The
  helper resolves an opaque expiring single-use capability bound server-side to uid / rooted
  generation / typed `StorePath` set / `RepairBuildPlan` digest / `policyVersion` / mode; replay /
  stale / mismatch / cross-UID fail closed; invalidated on restart. **Build admission and GC
  admission are broker-internal in-memory gates, not backing-file `flock`s**; only the per-user
  state-mutation lease is a filesystem `flock`.
- **Repair is verified non-atomic (INV-12 / RISK-22).** Cache repair deletes the live path before
  restore; an approved local repair moves the old output aside before replacement. `pkg repair` is
  explicitly user-initiated, warns affected commands may be temporarily unavailable, journals per
  path, runs a final read-only verify before marking anything repaired, auto-resumes only cache
  repair after a crash, and requires fresh single-operation approval before repeating a local
  repair build. `pkg` never claims repair is atomic or generation-switched.
- **Raw logs service-private; public logs sanitized NDJSON (D-19).** Raw Nix logs never reach
  `--json`/`--jsonl`/public/user-state logs (AC-S25).
- **Blocking broker/helper wire milestone (PR-39 / DR-017).** The detailed framed RPC, peer auth,
  operation lifecycle, child containment, capability storage/expiry, restart handshake, **and the
  broker-internal in-memory admission gates** (machine-wide build admission lease, AC-S19; GC
  admission gate / shared GC-inhibit permit, AC-S23) are a blocking **design/contract** milestone
  that lands **early** (M1.5, after the core-types/state prerequisites PR-3/PR-10) and **before**
  broker/helper integration (PR-27/28), repair (PR-30), and Real-Nix execution (PR-36) — tracked as
  **PR-39**, unconditionally gating PR-27/28 (broker/helper integration), PR-30 (two-phase repair),
  and PR-36 (Real-Nix execution) — full two-phase mutating repair is an accepted V1 milestone
  (DR-017; non-atomic residual RISK-22). It is a **contract**: it does **not** depend on the installers (PR-27/28 implement the
  PR-39 contract, so the prior PR-39→PR-27/28 dependency was circular and is removed). The wire
  schema itself is **open** and lands in PR-39; it is not invented in planning. The admission gates
  are owned here — PR-22 owns only GC reclamation + the per-user state-mutation `flock`, so no M4 PR
  is credited with a broker-hosted gate.

---

## 17. Document-set verdict

The set `00`–`12` is **internally reconciled and navigable**: every cross-cutting concept
(intent vs realization, the channel descriptor, the index, the subprocess contract, the
state/generation/GC-root model, the per-user/machine-global split) resolves to a single owning
document and a single schema owner, with migrations centralized in doc 05. Decisions (`D-*`/
`INV-*`), threats (`T-*`), risks (`RISK-*`), decision records (`DR-*`), and PRs (`PR-*`) form a
closed reference graph enforced by CI linkcheck. The architecture is sound and bounded for V1
(hidden, exclusively-managed, TUF-pinned Nix; per-user authoritative state; rollback-safe
generations). The set is **honest about what is not yet decided**: five go/no-go spikes
(S1–S5) gate the irreversible parts of the build, and several high-severity residuals
(RISK-01/02/20) are disclosed rather than hidden. **Recommended next action:** begin PR-0…PR-3
and open PR-4…PR-8 in parallel so DR-001…005 land before M1 locks in; plan **PR-39 (broker/helper
framed-RPC + capability milestone)** to land **early** — right after the core-types/state
prerequisites (PR-3/PR-10, M1.5) and **before** the broker/helper integration (PR-27/28), repair
(PR-30 — full two-phase mutating repair is an accepted, unconditional V1 milestone), and Real-Nix
execution (PR-36) it gates — so the wire design (and the broker-internal build/GC admission gates)
is not left vague and does not depend on (i.e. is not blocked by) the installers (DR-017).
