# 04 — Resolution, Install & Build Pipeline

> Owner: execution track. This document is **planning only**; it specifies no Rust code.
> Sibling plans it depends on are cross-referenced by number. See [Dependencies](#14-dependencies-on-other-plans).

## 1. Purpose

Define the **exact end-to-end machine-readable pipeline** that turns a user
command (`install`, `remove`, `upgrade …`) into a realized, activated,
content-addressed set of store paths owned by this product — while never
exposing raw Nix inputs to the user and never leaving the previous working
generation inactive on failure.

Concretely this document specifies, for the lifecycle of one mutating
operation:

1. **Resolve** — turn a *user-intent selector* into an *exact realized
   identity* (derivation + outputs + narHash + Nixpkgs revision).
2. **Preflight** — preview closures, builds, downloads, collisions, disk and
   policy violations *before* mutating anything.
3. **Acquire** — fetch from the trusted substituter (cache.nixos.org) or, on a
   cache miss, build locally for the host's native system after explicit user
   approval (Linux and macOS; D-11).
4. **Verify** — confirm NAR integrity and required signatures.
5. **Stage** — build a candidate generation (activation tree) in a staging
   area that the live `current` pointer does not see.
6. **Activate** — atomically swap `current` to the staged generation.
7. **Commit** — transactionally persist desired-state, lock, generation
   manifest, and the operation journal.

It also defines cache-hit/cache-miss semantics, the build preview/approval
gate, cancellation, restart recovery, resource/sandbox limits, structured
logs/progress, exit codes, and package/binary collision + multi-output
handling.

All Nix interaction is via the bundled, pinned Nix runtime (plan 07) using
**JSON output only**. The product never parses human-oriented Nix output.

## 2. Scope / Non-scope

**In scope**

- The seven-phase pipeline and its state machine.
- Machine-readable subprocess contracts for every Nix invocation (argv +
  JSON response shapes + error shapes).
- Selector → identity resolution rules (including pins and version ranges).
- Cache-hit vs cache-miss, cross-platform local-build preview/approval (Linux
  **and** macOS, native system only), and the concrete conditions under which a
  cache miss becomes `ACQUIRE_NO_BINARY`.
- Collision policy and multi-output selection.
- Progress event protocol, structured logs, exit codes.
- Cancellation, restart recovery, resource and sandbox limits.
- Failure matrix and the "previous generation stays active" invariant.

**Non-scope (owned elsewhere)**

- Trust, signing, the channel descriptor, and key management → **plan 02**.
- The disposable search index and the Nixpkgs catalog snapshot → **plan 03**.
- State schema, migrations, generations, GC roots, leases, corruption
  recovery → **plan 05**.
- CLI flag grammar, CLI/inline rendering, human output formatting, completion → **plan 06**.
- Installer, daemon, store prefix, privilege, PATH, uninstall → **plan 07**.
- Threat model, test lanes, release ops → **plans 08–10**.

## 3. Invariants

| # | Invariant | Enforcement |
|---|-----------|-------------|
| I1 | A failed operation leaves the **previous generation active** and the desired-state/lock **byte-for-byte unchanged**. | All mutation happens under a staging path; commit is one atomic `rename(2)`/`symlink(2)` swap (plan 05). |
| I2 | Every Nix subprocess emits machine-readable JSON **where the subcommand supports it** — via `--json`, or via `--log-format internal-json` for build logs; **unconditionally** for `nix derivation show` (the bundled Nix 2.34.8 emits JSON and does **not** accept `--json`). **Exempted** from structured output are the enumerated **status-only verify/gc commands** that have no JSON mode (e.g. `nix store verify`) — which instead must satisfy a checked **postcondition** (zero exit status **plus** a follow-up integrity recheck via a JSON-capable command). The product never regex-scrapes human output to derive data. | A single `nix` adapter module is the only legal caller; CI lint forbids other `Command::new("nix*")` and forbids any new non-JSON Nix call unless it carries an enumerated exemption + postcondition. |
| I3 | Only the channel descriptor's pinned Nixpkgs revision(s) and approved substituters/keys are used. No arbitrary flakes/URLs/overlays/trust edits. | Adapter references Nixpkgs **only** by the descriptor-pinned flake-ref `github:NixOS/nixpkgs/<rev>?narHash=<h>` (or its locked store path) — never a mutable channel or user URL. `--expr`, `--impure`, `--override-input`, `--inputs-from`, and `file://`/`path:` flakes are **never** passed (doc 01 §11.1); substituters/trusted keys fixed by the **generated, root-owned `nix.conf`** (plan 07) only — never per-call `--substituters`/`--trusted-public-keys` flags, which `pkg` never passes (trust policy is managed, not user-supplied). Evaluation is pure (`allow-import-from-derivation = false`; no `--impure`). |
| I4 | A realized identity is identified by its **store path** (which embeds the content hash), not by `pname@version`. Display metadata is never used as a key. | Lock and generation manifest key on store path; `pname`/`version` are display-only fields. |
| I5 | Local builds occur **only for the host's native Nix system** (Linux and macOS), **only after** the user has seen a deterministic build preview and explicitly approved a single operation, and **only** under `sandbox=true`/`sandbox-fallback=false` through the daemon's unprivileged build users. No Rosetta/cross-compilation/emulation/remote builders. Approval never overrides a hard policy refusal (unsupported/broken/impure derivation, or sandbox/build-user unavailable). | Preflight computes the build plan; a `BuildRequired` event gates on `Host::nativeSystem ∈ descriptor.buildPolicy.nativeLocalBuilds(mode=allow-with-gates) && sandbox_ready && build_users_ready && user_approval == true`. A cache miss with no buildable path, or a disallowed build, yields `ACQUIRE_NO_BINARY`. |
| I6 | Acquire/verify/stage are **idempotent and resumable**; restarting the product resumes from the persisted operation journal without redoing completed Nix work unnecessarily. | Journal is append-only + fsynced; Nix daemon keeps realised paths so re-running `nix build` is cheap. |
| I7 | The activation tree is a **Rust-materialized, user-owned symlink forest** (not a Nix store object), built from already-verified selected output store paths; **activation invokes ZERO Nix commands**. Collisions are detected by the Rust forest walk; integrity is enforced by `treeDigest`. | Stage phase walks each selected output **without following or dereferencing symlinks** — an encountered source symlink becomes a forest leaf targeting that **encountered absolute store entry itself**, never its dereferenced ultimate destination — creates merge dirs + leaf symlinks to absolute validated `/nix/store` targets, rejects **path-escape of the constructed relative activation path** and file-vs-dir conflicts, and computes `treeDigest`. |

## 4. Concepts and data model

### 4.1 User-intent selector vs realized identity (summary; full schema in plan 05)

```jsonc
// Selector — what the user typed/installed (intent). Stored in manifest.json (doc 01 §10.1).
{
  "id": "sel_018f",                       // stable within the manifest
  "selector": "ripgrep",                  // user intent string (D-13)
  "attribute": "ripgrep",                 // resolved Nixpkgs attribute (may equal selector)
  "versionPref": { "kind": "any" },       // "any" | "exact" | "min" | "range"
  "outputs": null,                        // null => meta.outputsToInstall
  "sourceRev": "channel:current",         // or "channel:pinned:<id>" | "rev:<gitsha>"
  "pinned": false,
  "pinnedTo": null                        // realized.storePath when set by `pin`
}

// Realized identity — the exact thing activated. Stored in lock.json + generation manifest.
{
  "storePath": "/nix/store/a1b2...-ripgrep-14.1.0",
  "deriver":  "/nix/store/9z8y...-ripgrep-14.1.0.drv",
  "outputs": { "out": "/nix/store/a1b2...-ripgrep-14.1.0", "man": "/nix/store/...-ripgrep-14.1.0-man" },
  "outputsToInstall": ["out", "man"],
  "system": "x86_64-linux",
  "nixpkgsRev": "abcd1234...deadbeef",
  "narHash": "sha256-...",
  "pname": "ripgrep", "version": "14.1.0",
  "closureNarSize": 4821034,
  "license": ["MIT"], "broken": false, "insecure": []
}
```

`pname@version` is **display metadata, not a key.** Two distinct attributes can
share `pname`; only `storePath` is unique. See plan 05 §"Identity".

### 4.2 Pipeline phase state

```jsonc
// One row in the operation journal (plan 05) per phase transition.
{ "opId": "op_2024-...-a1", "phase": "stage", "status": "started",
  "prevStateHash": "sha256-...", "channelSeq": 42,
  "ts": "2024-08-02T23:14:00Z" }
```

## 5. Pipeline overview

```mermaid
flowchart TD
    A[CLI command] --> R{resolve}
    R -->|selector| RV[drvPath + outputs]
    RV --> P[preflight]
    P -->|preview| APP{approval?}
    APP -->|no build, cache hit| AC[acquire: substitute]
    APP -->|build required, approved| AB[acquire: build]
    APP -->|build denied or not approved| FAIL[abort: leave gen N active]
    AC --> V[verify]
    AB --> V
    V --> S[stage: Rust symlink forest (ZERO Nix)]
    S --> SX{collisions?}
    SX -->|yes, abort policy| FAIL
    SX -->|ok| A2[activate: atomic swap]
    A2 --> C[commit: journal + persist]
    C --> DONE([generation N+1 active])
    FAIL -.rollback staging.-> PREV[generation N still active]
```

### 5.1 Resolve

**Goal:** map selectors to exact derivations using only the channel's pinned
Nixpkgs.

The product never lets the user pass a raw `nixpkgs#…` URL or flake ref. It
constructs the evaluation invocation from the channel descriptor (plan 02).

For each selector the adapter issues exactly one evaluation call and reads
JSON. Two equivalent strategies; **strategy A is preferred** because it returns
the full derivation document and meta in one round trip.

**Strategy A — `nix derivation show` (JSON emitted unconditionally):**

```
nix derivation show \
  github:NixOS/nixpkgs/<rev>?narHash=<h>#legacyPackages.<system>.<attribute>
```

`nix derivation show` in the bundled Nix 2.34.8 emits JSON **unconditionally**
and does **not** accept a `--json` flag (I2; *confirmed* [^drv-show]). The
`<rev>` and `<narHash>` come **only** from the signed channel descriptor
(plan 02). The flake-ref-with-`narHash` form is the canonical Nixpkgs reference
(doc 01 §11, plan 03 §9.2): no `--override-input`, no `--expr`, no mutable
channel, no `NIX_PATH`. Evaluation is **pure** — locked flake inputs, no
`--impure`, and the managed `nix.conf` sets `allow-import-from-derivation = false`
(plan 07 §5.2), so nothing in the closure is realized during evaluation.

For the **`BuildPlan` derivation closure** the single canonical planning command
is the **recursive** form (§5.2.1, §5.3.1):

```
nix derivation show --recursive \
  github:NixOS/nixpkgs/<rev>?narHash=<h>#legacyPackages.<system>.<attribute>
```

Response shape — the JSON **v4 envelope** `{"version":4,"derivations":{...}}`
(*confirmed current Nix behavior* [^drv-show]); the per-derivation document under
`.derivations[<drvPath>]` is the same as earlier Nix:

```jsonc
{
  "version": 4,
  "derivations": {
    "/nix/store/9z8y...-ripgrep-14.1.0.drv": {
      "name": "ripgrep-14.1.0",
      "outputs": { "out": { "path": "...", "hashAlgo": "...", "hash": "..." },
                   "man": { "path": "...", ... } },
      "inputSrcs": [...], "inputDrvs": {...}, "platform": "x86_64-linux",
      "builder": "...", "args": [...], "env": { "pname": "ripgrep", "version": "14.1.0", ... }
    }
    /* ...one entry per derivation in the recursive closure... */
  }
}
```

**Strategy B — `nix eval` for cheap drvPath lookups** when only the path is
needed (e.g., checking if already installed): `nix eval --raw github:NixOS/nixpkgs/<rev>?narHash=<h>#legacyPackages.<system>.<attr>.drvPath`.
Strategy B is a **cheap drvPath/`meta` probe only**; it is **not** the canonical
evaluation route for the `BuildPlan` derivation closure (that route is always
Strategy A's `nix derivation show --recursive`, §5.2.1).

Meta (outputsToInstall, license, broken, knownVulnerabilities) is read via
`nix eval --json ….<attr>.meta` or extracted from the derivation's `env`.
`meta.outputsToInstall` is a **confirmed nixpkgs convention** [^outputs-to-install].

**Resolve rules:**

1. `attribute` must resolve to a single derivation (not a nested attrset of
   packages). Ambiguous attribute → `RESOLVE_AMBIGUOUS` with candidates.
2. `version_pref`:
   - `any` → the attribute's current derivation.
   - `exact:<v>` → reject if `version != v` (no fuzzy). Suggest the matching
     sibling attribute (e.g., user asks `python@3.11` → candidate `python311`).
   - `min:<v>` / `range` → evaluate; reject if out of range.
3. If selector is `pinned_to` an existing realized identity (set by `pin`), the
   pinned `store_path` is the target and Resolve is a no-op (verify it still
   exists in store; if GC'd, re-acquire from substituter, else error).
4. Mixed Nixpkgs revisions are **expected and legal**: each top-level selector
   resolves against its own `source_rev` (current channel for new installs /
   upgrades; pinned rev for unchanged packages). The generation manifest may
   list multiple `nixpkgs_rev` values. Closures are content-addressed so this
   is sound.

**Resolve failures → exit codes (see §11):** `RESOLVE_NOT_FOUND`,
`RESOLVE_AMBIGUOUS`, `RESOLVE_BROKEN` (`meta.broken`),
`RESOLVE_UNSUPPORTED_SYSTEM` (`meta.badPlatform`/not built for this `system`),
`RESOLVE_INSECURE` (`meta.knownVulnerabilities` non-empty and not allowlisted).

### 5.2 Preflight

Compute a **preview** with **no mutation**. Two **distinct** planning sources,
neither of which realizes any uncached output:

- **Derivation closure (build-dependency planning source):** for each target the
  adapter runs the single canonical planning evaluation
  `nix derivation show --recursive <exact pinned installable>` (§5.1 Strategy A) and
  **unions the returned `.derivations` maps** (keyed by drv path) into one
  operation-wide closure. This is **pure evaluation**
  (`allow-import-from-derivation = false`): it returns
  the evaluated recursive **derivation** graph — the JSON v4 envelope
  `{"version":4,"derivations":{<drvPath>:{...}}}` — and each derivation
  document's **expected output paths**, with **nothing realized**. This is the
  authoritative source for the build-dependency closure, the set of derivations
  needing a local build, and each derivation's `fixedOutput`/network
  classification (from its `env`/outputs). **It does not and cannot yield the
  eventual NAR size, runtime references, or realized closure size** for any
  output that has not yet been substituted or built.
- **Cache classification (NarInfo traversal — NOT `path-info` of unrealized outputs):**
  over the **full recursive closure** (every expected output path and its
  input-addressed closure), the adapter determines cache availability by querying
  the **managed** cache (`cache.nixos.org`) — either via a **private, policy-fixed** cache
  inspection command `nix path-info --store https://cache.nixos.org ...` (the store URL is
  **policy-fixed** to the managed cache, **never** user-provided) or the daemon's internal
  NarInfo/remote-cache-store traversal — configured once in the root-owned `nix.conf`
  (plan 07 §5.2), **never** via a per-call `--substituters` flag. For a
  **cache hit** the adapter reads the path's NarInfo and **recursively follows the
  NarInfo's references**, so exact **NarInfo-recorded download/NAR bytes and
  references are known for cache-present paths only**. A path whose NarInfo is
  **absent** and whose derivation has a non-empty builder is a **cache miss** ⇒
  **build required** for that path ⇒ a build preview for the op.
  `nix path-info <unrealized-output>` is **not** a valid way to learn an
  uncached/realized closure — it is absent for paths not yet in the store; the
  realized NAR size / runtime references / realized closure size for an **unbuilt**
  cache-miss are **unknowable at plan time** and are obtained only **post-build**
  via `nix path-info` of the realized output (§5.4). The preflight **never**
  reports "binary available" unless **every** closure path is a cache hit (has a
  NarInfo or is already present in the local store).
- **Build plan & preview (§5.2.1):** the build engine computes the **private
  canonical `BuildPlan`** (cache-miss derivations needing a local build, each
  bound by derivation-document digest + safe `fixedOutput`/network
  classification; the deterministic cache-classification digest + exact **known**
  cache bytes; readiness; resources/admission) and derives a **public sanitized
  `BuildPreview`** carrying the product `platform` shape, selector/package names,
  build count/names, the fixed-output/network notice, exact **known** cache
  download/content bytes, the **unknown-local-output count**, **heuristic**
  size/time estimates (explicitly outside the digest), and a product `readiness`
  summary (sandboxed/build-isolation/native-build + honest resource boundary) —
  never the Nix system triple, builder-user group, or cgroup internals. V1
  reports build time as unavailable until authenticated historical observations
  exist; a future time estimate may use a wide-error-bar historical mean per
  `platform` × input size, but is never deterministic or digest-bound.
- **Disk budget:** sum of **known** new cache-download bytes (exact, NarInfo-
  recorded) plus a **heuristic** allowance for unbuilt cache-miss outputs vs free
  space at `/nix/store` (`statvfs`), using the deterministic
  `admission.diskHeadroomRatio` (`new_bytes × ratio`). The same deterministic
  ratio is re-applied at build time (§5.3.1). The V1 bootstrap allowance is
  deliberately simple and product-owned: **1 GiB per cache-miss path**, plus
  exact cache-present NAR content bytes. It is labeled approximate, remains
  outside the approval digest, and is not represented as a hard build-output
  cap. Overflow or an unavailable allowance fails closed before Nix runs.
- **Policy checks**: deny if `meta.license` is in the denylist, if
  `meta.unfree` and the product policy forbids unfree (configurable, default
  allow with notice), if `meta.broken`/`meta.insecure`.
- **Collision preview** (heuristic): from a lightweight index of `bin/` names
  in the current closure, flag *likely* file collisions between candidate
  packages and existing install set. Authoritative detection happens at Stage
  via the Rust symlink-forest walk (§5.5).

Emit a **PreflightReport** (rendered by the CLI per plan 06 §5 — a public
`type:"preflight"` record on the `--jsonl` stream and the inline renderer, or
the human approval preview; it is **not** written to `--json` stdout, which
emits only the single final result document, plan 06 §5.2). It carries
the **public sanitized `BuildPreview`** (§5.2.1.b), never the raw private `BuildPlan`;
size fields are split into **exact known cache bytes** and an **unknown-local-output
 count** rather than pretending an unbuilt closure's size is exact:

```jsonc
{
  "schemaVersion": 1,                    // PUBLIC record, versioned like every public line (plan 06 §5.3)
  "type": "preflight",
  "targets": [                           // sanitized, product-owned — NO store/drv paths, NO flake refs/argv
    { "selector": "ripgrep",             // user-intent selector (D-13)
      "packageName": "ripgrep",          // display pname; never a key
      "version": "14.1.0",               // display version; never a key
      "outputsToInstall": ["out","man"],
      "cache": "hit" }                   // hit | miss | partial — per-target cache SUMMARY (full detail in `preview`); no store/drv path here
  ],
  "totals": {
    "knownDownloadBytes": 1234567,       // EXACT, cache-present paths only (cache-reported download bytes)
    "knownContentBytes": 4821034,        // EXACT cache-reported UNCOMPRESSED content bytes (cache-present paths only); NOT filesystem disk usage
    "unknownLocalOutputs": 0,            // count of cache-miss paths whose eventual size is unknowable pre-build
    "diskFreeBytes": 9000000000, "diskOk": true },
  "removals": [], "upgrades": [],
  "preview": null,                       // the PUBLIC sanitized BuildPreview (§5.2.1.b); non-null when a local build is required
  "policy": { "ok": true, "notices": ["unfree: ripgrep is MIT (ok)"] },
  "collisionWarnings": [],
  "approvalRequired": false
}
```

### 5.2.1 Build approval subjects — PRIVATE canonical `BuildPlan` + PUBLIC sanitized `BuildPreview`

**Two objects, one approval.** Approval binds a single deterministic object, but
that object is **never serialized raw to the public CLI/`--json` or a future public
RPC.** Raw Nix implementation details — flake refs, drv paths, derivation
expressions, Nix argv, trust options, and store-control knobs — are **private to
the managed build engine**. The product therefore keeps **two** representations:

- a **PRIVATE canonical `BuildPlan`**, held by the managed service / build engine,
  bound by **digest** and an **operation handle**, identifying the exact
deterministic execution a user is approving;
- a **PUBLIC sanitized `BuildPreview`**, product-owned and the **only** thing
  rendered to the user (`--json`, human preview, future RPC), carrying a
  **`buildPlanDigest`** pointer to the private plan it summarizes.

The digest is computed over the **private canonical `BuildPlan`** (RFC 8785 JCS
canonical JSON). The build engine computes both objects; the **journal** persists
`buildPlanDigest` + `policyVersion` (plan 05 §5.4); any field change in the
canonical `BuildPlan` invalidates the approval (§7) and `pkg` re-prompts
(interactive) or exits `ACQUIRE_NEEDS_APPROVAL` (non-interactive). (V1 uses CLI
inline rendering, not a full-screen TUI — plan 06.)

#### 5.2.1.a PRIVATE canonical `BuildPlan` (managed-service-held; never serialized raw to CLI/RPC)

```jsonc
{
  "schemaVersion": 1,
  "nixRuntimeVersion": "2.34.8",          // exact bundled Nix runtime (plan 07 §5.1); pinned by descriptor, NOT user-settable
  "descriptorHash": "sha256:<lowercase hex>",  // hash of the trusted channel descriptor (doc 02 §7)
  "policyVersion": 7,                     // channel descriptor policyVersion (doc 02)
  "channelSeq": 42,                       // exact channel sequence number
  "nixpkgs": {                            // pinned Nixpkgs source (CAT-INV-01)
    "rev": "<40-char git sha>",               // descriptor.nixpkgs.rev
    "narHash": "sha256-..."                 // descriptor.nixpkgs.narHash (SRI)
  },
  "system": "aarch64-darwin",            // native build system only
  "targets": [                           // OPERATION-WIDE target set, SORTED by canonical selector id
                                          //   (NOT a singular target). The raw pinned installable
                                          //   flake-ref per selector is an internal engine detail, NOT a
                                          //   digest-bound command/source string.
    { "selectorId": "sel_018f",          // manifest selector id — canonical SORT KEY for targets[]
      "attribute": "ffmpeg",             // resolved Nixpkgs attribute
      "versionPref": { "kind": "any" },  // selector version preference
      "outputsToInstall": ["out","man"]  // resolved output selection
    }
    /* ...one entry per selector in the operation, SORTED by selectorId... */
  ],
  "derivationClosure": {                 // canonical recursive DERIVATION closure — UNION over all targets (§5.1 Strategy A; pure eval)
    "jsonVersion": 4,                    // Nix derivation-show envelope top-level version (validated)
    "closureDigest": "sha256:<lowercase hex>",  // JCS digest over the union canonical .derivations map (keyed by drv path)
    "derivationCount": 37                // number of DISTINCT derivations in the UNION closure
  },
  "builds": [                             // SORTED; the cache-miss derivations needing a LOCAL BUILD, each bound
                                          //   by its derivation-document DIGEST + a SAFE classification. NO raw
                                          //   command strings, NO per-miss narSizeBytes (unknowable pre-build).
    { "derivationDigest": "sha256:<lowercase hex>",  // JCS digest of this derivation's canonical document (the per-build identity)
      "name": "ffmpeg-6.1",              // derivation name (safe, display-derived)
      "system": "aarch64-darwin",
      "fixedOutput": false,              // fixed-output/network classification (safe)
      "networkEnabled": false }          //   (regular: false; fixed-output: true)
    /* ...one entry per buildable cache-miss derivation, SORTED by derivationDigest ... */
  ],
  "cacheClassification": {               // deterministic cache classification of the UNION closure, measured at plan time
    "classificationDigest": "sha256:<lowercase hex>",  // JCS digest over the sorted (path → present|absent) NarInfo map —
                                          //   THE cache identity (counts alone are NOT the identity)
    "hits": 34, "misses": 3,            // closure paths classified against cache.nixos.org at plan time
    "knownCacheBytes": {                  // EXACT bytes known from NarInfos of CACHE-PRESENT paths only
      "downloadBytes": 1234567,          // exact download total for cache-hit closure paths (NarInfo-recorded)
      "narBytes": 4821034 }              // exact uncompressed NAR total for cache-hit closure paths (NarInfo-recorded)
                                          //   NOTE: no per-miss narSizeBytes / estimatedClosureBytes here —
                                          //   an unbuilt cache-miss's eventual size is UNKNOWABLE pre-build (§5.2)
  },
  "readiness": {                          // STABLE cross-platform schema; explicit fields, NO absent-on-mac ambiguity (re-checked pre-build, §5.3.1)
    "sandbox": { "enabled": true, "fallback": false },
    "buildUsersGroup": "nixbld",          // build-users-group; nixbld (Linux nixbld*, macOS _nixbld*), created by installer
    "buildUsersReady": true,
    "useCgroupsEnabled": false,           // mirrors nix.conf use-cgroups. Linux: true when local builds are allowed; macOS: ALWAYS false
    "cgroupV2Ready": false                // cgroup v2 readiness.      Linux: true when local builds are allowed; macOS: ALWAYS false
  },
  "resources": {                          // exact unit-bearing settings (§8); product-managed, fixed
    "maxJobsPerConnection": 1,            // Nix max-jobs (per client/connection); NOT a CPU/mem/IO cap
    "machineGlobalMaxConcurrentBuildOperations": 1,  // pkg machine-global build admission permit (§5.3.1; broker-internal)
    "coresHint": 0,                       // Nix cores (NIX_BUILD_CORES); cooperation hint, not a cap
    "maxSilentTimeSeconds": 3600,         // Nix max-silent-time (daemon kills a stalled builder)
    "timeoutSecondsPerDerivation": 86400, // Nix timeout (per derivation)
    "maxBuildLogSizeBytes": 268435456     // Nix max-build-log-size (kills builder; never truncates)
  },
  "admission": {                          // deterministic ceilings INSIDE the digest;
                                          // measured free bytes/load are NOT (see PreflightReport)
    "diskHeadroomRatio": 1.2,             // new_bytes * ratio must fit free space (deterministic)
    "maxLoadavgCeiling": 8                // build.max_loadavg (V1 default 2 × logical CPU count)
  }
}
```

#### 5.2.1.b PUBLIC sanitized `BuildPreview` (the only object rendered to `--json`/human preview/future RPC)

```jsonc
{
  "schemaVersion": 1,
  "platform": { "os": "macos", "arch": "arm64" },  // PRODUCT platform shape — NOT a Nix system triple (the private BuildPlan keeps `system`, §5.2.1.a)
  "policyVersion": 7,                     // channel descriptor policyVersion (doc 02)
  "buildPlanDigest": "sha256:<lowercase hex>",  // POINTER to the private canonical BuildPlan this preview summarizes (§5.2.1.a)
  "targets": [                            // product-owned, sanitized — selector/package names, NOT raw flake refs/drv paths
    { "selector": "ffmpeg",
      "packageName": "ffmpeg",            // display pname; never a key
      "version": "6.1",                   // display version; never a key
      "outputsToInstall": ["out","man"] } ],
  "build": {                              // the local-build portion, sanitized
    "count": 3,                           // unknown-local-output count (cache-miss derivations needing build)
    "names": ["ffmpeg-6.1","libx264-...","..."],  // derivation names (display), SORTED
    "hasFixedOutput": true },             // fixed-output/network NOTICE (≥1 build is network-enabled)
  "cache": {                              // EXACT known cache bytes for cache-PRESENT paths only
    "knownDownloadBytes": 1234567,        //   cache-reported download bytes (cache-present paths; honest about the known portion)
    "knownContentBytes": 4821034 },       //   EXACT cache-reported UNCOMPRESSED content bytes (cache-present paths only); NOT filesystem disk usage
  "unknownLocalOutputs": 3,              // count of cache-miss paths whose eventual size is unknowable pre-build
  "estimates": {                          // HEURISTIC ONLY — explicitly OUTSIDE the digest (§5.2.1.a)
    "approxBuildMinutes": null,           //   V1 bootstrap has no authenticated historical timing observations
    "approxNewDiskBytes": 3226046506,     //   V1: 3 × 1 GiB misses + exact 4,821,034 cache-present NAR bytes
    "approxTotalClosureBytes": null },    //   null when any closure path is unbuilt (unknowable)
  "readiness": {                          // PRODUCT readiness SUMMARY — no Nix system triple, no builder-user/cgroup internals (private BuildPlan keeps those, §5.2.1.a)
    "sandboxed": true,                    // build runs under the Nix sandbox (maps to private sandbox.enabled)
    "buildIsolationReady": true,          // unprivileged build-user isolation is ready (maps to private buildUsersReady; group name stays private)
    "nativeBuild": true,                  // a native local build is possible on this host platform
    "resourceBoundary": {                 // HONEST resource-boundary summary — what the managed runtime actually guarantees (§8/RISK-07)
      "isolation": "sandbox",             // "sandbox" (filesystem-isolated; network-denied for regular derivations, network-enabled for fixed-output) | "none"
      "perBuildResourceCap": false,       // managed runtime enforces NO hard per-build memory/CPU/IO cap (RISK-07 disclosed residual)
      "notice": "Builds run sandboxed. The managed runtime applies no hard per-build memory/CPU/IO cap; daemon time/log ceilings and a single machine-global build bound the operation." } },
  "approvalRequired": true
}
```

The **public `BuildPreview` deliberately omits** all raw Nix implementation
details — no flake refs, no drv paths, no derivation expressions, no Nix argv,
no trust/substituter/store-control knobs, **no Nix system triples, and no
builder-user/cgroup internals** (those live in the private `BuildPlan`,
§5.2.1.a) — and carries **only** the `buildPlanDigest` pointer plus the friendly
`platform`/`readiness` summary above. Heuristic size/time **estimates** live
**only** here and are **never** part of the canonical `BuildPlan` digest.

- **Determinism (private plan):** repeated evaluation of the same operation-wide targets for the same **descriptor/system/operation-wide-targets/union-derivation-closure/cache-classification/readiness/resource snapshot** produces a byte-identical canonical `BuildPlan`. Array fields are sorted (`targets` and `builds` by their canonical keys — selector id and `derivationDigest` respectively); maps are key-ordered lexicographically; object keys use the stable order shown.
- **Canonicalization & digest:** `pkg` serializes the canonical `BuildPlan` (§5.2.1.a) to **RFC 8785 (JCS) canonical JSON**, then records the digest as `sha256:<lowercase hex>`. The digest binds **exactly**: `nixRuntimeVersion`, `descriptorHash`, `policyVersion`, `channelSeq`, `nixpkgs.rev`+`nixpkgs.narHash`, `system`, the operation-wide `targets[]` (sorted by canonical selector id), `derivationClosure` (`jsonVersion`/`closureDigest`/`derivationCount` over the UNION closure), the sorted `builds` (per-build `derivationDigest` + safe `fixedOutput`/`networkEnabled` classification — **no per-miss `narSizeBytes`**), `cacheClassification` (`classificationDigest`/`hits`/`misses`/exact known-cache `downloadBytes`+`narBytes` — **no `estimatedClosureBytes`**, since an unbuilt miss's size is unknowable), `readiness`, and the unit-bearing `resources` plus the deterministic `admission` ceilings. The **cache identity is `classificationDigest`, not the hit/miss counts** (counts alone must not be the identity). Dynamic **measured** free bytes / `loadavg` / timestamps / build-time **estimates** are **outside** the digest (they live only in the public `BuildPreview`); the deterministic disk-headroom ratio and load ceiling *settings* are **inside** it. The journal stores `buildPlanDigest` (plan 05 §5.4); the **approval receipt** binds the operation ID + this exact canonical `BuildPlan` digest + the `policyVersion` (§7).
- **No raw Nix details leak to the public surface:** the public `BuildPreview` (§5.2.1.b) is the **only** object serialized to `--json`/human preview/future RPC; the canonical `BuildPlan` (§5.2.1.a) is held privately by the managed build engine and addressed only by `buildPlanDigest` + operation handle.
- **Mutation invalidates approval:** immediately before executing a build,
  `pkg` acquires the machine-global build admission permit (§5.3.1), **re-runs readiness**
  (sandbox/build-user/cgroup) and **re-derives the exact derivation set**
  (`nix derivation show --recursive`); if the resulting canonical `BuildPlan` digest differs from the
  approved one, the approval no longer applies and the build path fails /
  re-prompts as already specified in §5.3/§7. The dynamic disk/free-space/load
  recheck happens **after** the digest comparison and is **outside** the digest (§5.3.1).

### 5.3 Acquire

Bring all target closure paths into the store. All acquire work is carried by
the **enforced singleton unprivileged broker** — the sole **general** daemon
client / sole spawner of the bundled `nix` CLI for all normal operations
(evaluate, cache/substitute, approved local build, `path-info`, read-only
`nix store verify`, liveness-respecting GC), a daemon `allowed-user` **never** a
Nix `trusted-user` (root is the only `trusted-user`; ARCH-INV-05/07, plan 01
§8/§11, plan 07 §7.4) — over its UID-authenticated closed-request channel; the
user CLI sends only closed requests and an opaque operation handle and
**never** reaches the daemon socket (ARCH-INV-01/07). The one narrow exception
to the broker's sole-general-client status is the root helper's privileged
**two-phase `nix store repair`** maintenance op (plan 01 §12.4 / plan 05 §10 /
plan 07 / plan 08); the install path **never** routes through the root helper
except for GC-root filesystem publication (§5.7) and **never** performs repair.

**GC-inhibit permit — acquired before any realization (the third admission
mechanism, plan 05 §8.5).** Before the broker dispatches **any**
substitute/build/realization step that can create unrooted store outputs, the
op's opaque handle acquires a **shared GC-inhibit permit** from the
broker-internal **machine-global GC admission gate**. This is **distinct** from
the per-user state lease (a user-owned filesystem `flock`, plan 05 §12) and from
the build-admission permit (§5.3.1, a broker-internal exclusive mutex). The
permit is held across the return to the CLI, the Rust forest staging, the
**prepared** candidate snapshots, and the broker-mediated root-helper
publication, and is released only once the op's per-output root set is
**durable** (the **rooted** step, §5.6/§5.7) — or the op aborts. `nix store gc`
takes the gate's **exclusive** side and waits fairly for all in-flight shared
permits to drain before collecting, so a GC started for any uid cannot reclaim
this op's realize→root outputs. The **per-user lease does not protect against
another uid's global GC** — only the gate does; do not conflate them. (The exact
framed-RPC, operation-handle lifecycle, handle-expiry, and disconnect semantics
of the gate are owned by the detailed-broker design — plan 07 §7.4 follow-on —
and are a **blocking** item for the next broker milestone; this plan states the
ordering invariant — permit-before-realize, release-after-rooted-or-abort — not
the wire format.)

**Cache hit (default path on both OS):** let Nix substitute from
cache.nixos.org. The bundled Nix is configured (plan 07) with
`substituters = https://cache.nixos.org` and the channel descriptor's
`trusted-public-keys` in the **managed, root-owned `nix.conf`**. Signature
verification is performed by Nix per `trusted-public-keys` / per-substituter
`public-keys` (*confirmed* [^substituters][^trustedkeys]). Trust/substituter
policy is **managed, not user-supplied** — the acquire command passes **no**
`--substituters`/`--trusted-public-keys`/`--option` flags of any kind.

```
nix build --json --max-jobs 0 --no-link \
  /nix/store/<x>.drv^out,man ...
```

Cache-only acquisition uses `--max-jobs 0 --no-link --json`; `--no-link` is
**never** combined with `--out-link` (Nix rejects that combination and we do not
want a named link for a pure-substitute acquire). The acquire targets are
**explicit derived-output paths** `/nix/store/<x>.drv^<out,man>` (or `^*`),
constructed by the adapter from the **validated output selection** parsed out of
the derivation document (§5.1) — never a bare opaque `.drv`, which does not build
outputs. `--max-jobs 0` makes a missing
substitute a hard error rather than silently falling back to a local build —
this keeps the **pure-substitution** acquire phase build-free on every platform at
the Nix invocation level, not merely as UI policy (I5). A miss here is not yet `ACQUIRE_NO_BINARY`: it hands control to
the explicit build path below.

**Cache miss → explicit build (Linux and macOS, native system only):**
preflight already produced a public sanitized `BuildPreview` (with `approvalRequired: true` and a `buildPlanDigest` pointer). If the
build is disallowed for a concrete reason — the descriptor's `buildPolicy`
denies the host system; the package is `meta.broken`/unsupported on this
`system`; the derivation requires forbidden impurity or unsandboxed execution;
or sandbox/build-user readiness cannot be verified — acquire fails with
`ACQUIRE_NO_BINARY` and a calm reason naming the path(s) and suggesting
`pkg info <attr>`. **Approval never overrides a hard policy refusal** (§8, I5).
Otherwise, if the user approved — interactive prompt **after** the final public
sanitized `BuildPreview` is displayed (carrying the `buildPlanDigest` of the
canonical plan), or `--yes` pre-approving that single operation
non-interactively (the same preview is still emitted and journaled; approval is
bound to the canonical `BuildPlan` digest and policy version, so it is invalid
if the plan changes) — acquire runs **with building enabled**
for the host's native system, targeting the **explicit derived-output paths**
`/nix/store/<x>.drv^out,man` (or `^*`) of the approved plan. **No**
`--substituters ""`/`--builders ""` flags are passed: the managed config already has
fixed cache/trust and empty remote builders, so a binary appearing meanwhile may safely
substitute and the actual provenance (cache vs. local-build) is recorded in the lock:

```
nix build --json --no-link \
  --max-jobs <N> --cores <C> \
  --max-silent-time <S> --timeout <T> \
  --keep-going \
  --log-format internal-json \
  /nix/store/<x>.drv^out,man ...
```

If the build was required but not approved, acquire exits
`ACQUIRE_NEEDS_APPROVAL` and stages nothing (cancel is the safe default).
`--log-format internal-json` is the **confirmed machine-readable log channel**
[^log-format]; the adapter parses it into the product's ProgressEvent stream
(§10). `--keep-going` lets one failing build not abort unrelated targets; the
product then reports partial results and refuses to stage.

### 5.3.1 Machine-global local-build admission permit

`max-jobs` is **per client/connection** in stock Nix 2.34.8, so it does **not** by
itself serialize local builds across different users/connections on the same
machine. `pkg` therefore enforces its own **machine-global local-build admission
permit**, distinct from and in addition to the **per-user package-state lease**
of plan 05 §12, and distinct from the **machine-global GC admission gate** of
plan 05 §8.5 (§5.3 above). There are **three different mechanisms** (mirroring
plan 05 §8.5); they are **not** the same:

- **Per-user state lease (plan 05 §12)** — a per-user mutation lock: an advisory
  `flock` on the **user-owned** path `<user-state>/run/lease`; serializes mutating
  ops for **one uid** against that uid's own `gc`/recovery. It does **not**
  serialize builds across users, and it is owned/operated by the invoking user.
  It is a **filesystem `flock`** and stays that way.
- **Build admission permit (this section)** — a **broker-internal** machine-global
  fair mutex/queue for **approved local builds only**, living **inside the
  enforced singleton unprivileged broker** (the sole **general** daemon client /
  `allowed-user`, never a Nix `trusted-user`; ARCH-INV-05/07). It is **not** a backing file and
  **not** an in-kernel `flock`: there is **no** `/var/lib/pkg/run/build-admission`
  file and **no** pid/boot-id record. The permit is owned by the **opaque
  operation handle** the broker mints per op; exactly one approved local-build
  operation holds it at a time, machine-wide across all uids
  (`resources.machineGlobalMaxConcurrentBuildOperations = 1`, §5.2.1). A waiter is
  queued **fairly** and may **cancel** at any time (cancel is the safe default
  and exits `CANCELLED`, 75). The broker grants/denies on the calling user's
  behalf over its UID-authenticated closed-request channel (ARCH-INV-06);
  **ordinary users never reach the gate directly**. Pure-substitution acquire
  (`--max-jobs 0`, no local build) does **not** take this permit.
- **GC admission gate (plan 05 §8.5 / §5.3 above)** — a **broker-internal** counted
  read/write structure whose shared **GC-inhibit permit** every mutating op
  acquires before the broker dispatches the substitute/build/realization, and
  whose exclusive side GC takes; it protects realize→root outputs from any user's
  broker GC. It is specified in plan 05 §8.5 (and summarized in §5.3 above).

**Waiting, cancellation, and revalidation.** A second operation that needs the
permit while the broker holds it for another is **queued fairly** (or the user
may cancel; cancel is the safe default and exits `CANCELLED`, 75). When the
broker finally grants the permit it **must not reuse a prior approval blindly**
— the world may have changed while it waited. On granting the permit the broker
performs, in order:

1. **Re-derive the exact derivation/readiness `BuildPlan`** (`nix derivation show
   --recursive`, §5.1/§5.2.1) and **compare its digest** to the approved
   `buildPlanDigest`. On mismatch, fail/re-prompt as in §7 (interactive re-prompts;
   non-interactive exits `ACQUIRE_NEEDS_APPROVAL`, 68) and release the permit.
2. **Re-validate approval and readiness**: confirm the approval receipt still binds
   the operation ID + this digest + `policyVersion` (§5.2.1/§7), and re-check
   `readiness.sandbox.enabled`/`fallback`, `buildUsersReady`/`buildUsersGroup`,
   and (Linux) `useCgroupsEnabled`/`cgroupV2Ready`. A hard refusal
   (`ACQUIRE_NO_BINARY`, 67) is never overridden by approval or `--yes`.
3. **Re-measure the dynamic disk/free-space/load** — these live **outside** the
   digest (§5.2.1). Re-apply the preflight disk-headroom
   (`new_bytes * diskHeadroomRatio`) and `loadavg ≤ maxLoadavgCeiling` checks.
   On a threshold failure, perform **exactly one immediate recheck** (re-`statvfs` /
   re-read `loadavg` once), and if it still fails **fail closed** (`PREFLIGHT_FAIL`,
   65) and release the permit — there is no retry loop after that single recheck.
   cgroups (when present) are a **per-build process-grouping, lingering-process
   cleanup, and accounting facility — neither a per-build cap/security boundary
   nor an aggregate daemon-subtree guardrail** (§8); the aggregate daemon-subtree
   ceiling is the distinct Pending systemd service ceiling, not `use-cgroups`.

**Release on every exit; the handle owns the permit.** The build-admission permit
is released on **every** exit from the local-build path — success, `BUILD_FAILED`
(69), `ACQUIRE_NO_BINARY` (67)/`ACQUIRE_NEEDS_APPROVAL` (68)/`PREFLIGHT_FAIL` (65)
refusals, and cancellation (75) — and always **after child containment**: the
broker owns the operation handle and the realize/build subprocesses it supervises,
so a disconnect/cancel/error triggers broker-owned cancellation + cleanup (stop
any spawned build, discard staging) and only **then** releases the permit; the
user process going away never strands the permit. A **broker crash fails all
in-flight handles at once** (their build subprocesses are children the broker
supervises, so the crash tears down the whole set) and no orphaned permit
survives; a replacement broker starts with an empty gate and completes its
**startup recovery barrier** (plan 05 §11/§12 — no command observes state until
recovery reconciles it) before it admits the first build, so admission is never
granted against an unreconciled op. The permit is held only across the local
build itself, and never across cache-hit (pure-substitution) installs. (The exact
process model by which the singleton broker supervises and contains its
realize/build/gc children — cgroups / launchd / signal handling, OOM policy — is
owned by the detailed-broker design, plan 07 §7.4 follow-on, and is a **blocking
cross-doc item** for the next broker milestone; this document depends on, but
does not specify, it.)


### 5.4 Verify

After acquire, confirm what was realised is what was resolved and is intact:

- **Store-path identity match**: the realised `outputs[*].path` equals the
  resolved `store_path`. (Defends against evaluator nondeterminism.)
- **NAR + signature verification**: `nix store verify --recursive <store-path>`
  recomputes NAR hashes (*confirmed* [^store-verify]); whether a given path is
  "trusted" (signed by a key in the descriptor's `trusted-public-keys`) vs.
  locally built is determined by the path's `.narinfo` signatures observed at
  substitution time (`sigsObserved` in the lock) and the build-provenance tag —
  **not** by an unverified verify flag. The exact trust-mode flag (if any) for
  the pinned runtime is pinned for the chosen managed Nix runtime and validated
  by the Fake↔Real parity job (doc 09 §4.3; doc 01 §11 / doc 00 §11 SPK-02 — not
  a standalone spike). For local builds (Linux and macOS), the locally built path is **not**
  signed by cache.nixos.org — NAR integrity is verified and the sandbox (§8)
  provides build integrity; the path is tagged `provenance: local-build` in the
  **lock** (doc 01 §10.2 / doc 05 §5.2), not the manifest (the manifest holds
  desired-state selectors only).
- **Closure completeness**: `nix path-info --json --json-format 2 --recursive` of the
  (now realized) target must list all referenced paths as present
  (*confirmed* [^path-info]).

### 5.5 Stage

Materialize the **activation tree** for the candidate generation **without touching
`current`**, from the already-verified selected output store paths. The activation
tree is a **Rust-materialized, user-owned symlink forest** under `<user-state>/` —
**not** a Nix store object — and **Stage invokes ZERO Nix commands**. (The buildEnv
activation-store approach was considered and rejected: see Q4.1.) Nix owns
downloads/builds/store; `pkg` owns the deterministic forest.

The forest is built deterministically by walking each selected output:

1. For every selected selector × output (in **stable source order**: canonical
   selector id, then output name, then relative path), `pkg` walks the output's
   store path **without following or dereferencing symlinks** (per-entry
   `O_NOFOLLOW`-style semantics: a symlink is encountered, never recursed
   through).
2. For each encountered entry it computes the entry's `relativePath`
   (POSIX-normalized, relative to that output root) and its `storeTarget`:
   - a **regular file** → `storeTarget` is that file's absolute store path;
   - an **encountered source symlink** → `storeTarget` is the **absolute path of
     that symlink entry itself under the selected output root**. The walk does
     **not** `readlink` it, and does **not** validate or dereference its ultimate
     destination — the store entry already belongs to the verified output closure,
     so dereferencing would be both unnecessary and incorrect. Following the
     forest leaf later is the OS/kernel's job at runtime, not the walk's.
   Directory entries become **merge directories** in the forest; leaf entries
   (regular files **or** symlinks) become **forest symlinks** pointing at their
   `storeTarget`.
3. **Reject** any entry whose **constructed `relativePath`** escapes the forest
   root (`..`, absolute, or traversal components in the *relative activation*
   path) — a **path-escape**. (Path-escape is purely about the constructed
   relative activation path; it is **never** derived from dereferencing a source
   symlink.) Also reject any **file-vs-directory conflict** (the same
   `relativePath` is a file in one output and a directory in another). These are
   hard `STAGE_***` failures (§11), **not** collision-policy cases.
4. **Collision policy** applies only to two *leaf* records mapping to the same
   `relativePath`. V1 policies are **only**: `abort` (default), `keep-first`,
   `keep-last` — there is **no** `keep-all`/shadowing (§12.2). `keep-first`/`keep-last`
   choose the per-file winner in the stable source order while **retaining every
   non-conflicting file**; all winners and losers are recorded in the generation
   metadata and the journal (§5.7, plan 05 §5.3).
5. Compute the **`treeDigest`** = SHA-256 over **RFC 8785 (JCS) canonical JSON** of
   the sorted records `{relativePath, storeTarget, sourceSelector, output}` of the
   final (post-collision-policy) forest. The generation record stores the
   `treeDigest` + `entryCount` + `collisionPolicy` (and the collision resolutions) —
   **not** the full record map (§5.7).

Stage filesystem steps (the forest sits at a `.staging` path the live `current`
does not see; the rename into the retained `gen-<id>` path is **deferred to
Activate** §5.6/§5.7, and runs only after the per-output root set exists):

1. Materialize the forest at `<user-state>/activations/gen-<id>.staging/`
   (merge directories + leaf symlinks; `fsync` the dir tree as it is built).
2. After all entries + collision policy are applied, compute the `treeDigest`
   and **verify it recomputes**; `fsync(<user-state>/activations/)`. The forest
   is now complete at `.staging/`; it is **not yet renamed** to
   `activations/gen-<id>/`, and `current` still points at the old generation.

Because Stage runs no Nix and writes only under `<user-state>/activations/`, a
failed/interrupted Stage leaves only `.staging` detritus that recovery deletes;
the previous generation stays active. The retained forest is **rebuildable** at
any time from the generation record + the verified rooted outputs
(plan 05 §8.4/§10).

### 5.6 Activate

Make the new generation the live activation. Per the generation transaction
(§5.7; full crash-consistency contract in plan 05 §8.4), the forest is materialized
at `.staging` and its `treeDigest` computed/fsynced (**stage**); the durable
candidate-view snapshots **and** the immutable record are written (**prepared**);
the complete per-output root set is created and fsynced and the GC-inhibit permit
released (**rooted**); only then is the `.staging` forest atomically renamed to
its retained `gen-<id>` path (**promote**) and `current` swapped (**activated**).
Both the root set and the rename precede the swap, so the swap lands on a
durably-rooted, fully-documented, retained forest; recovery restores the mutable
`manifest.json`/`lock.json` views from the **prepared** snapshots (plan 05 §8.4/§11).

```mermaid
sequenceDiagram
    participant CLI as product (CLI)
    participant BRK as private broker (unprivileged; allowed-user)
    participant FS as filesystem (<user-state>)
    participant HELP as root-helper (sole gcroots writer)
    CLI->>BRK: op handle + shared GC-inhibit permit acquired BEFORE realize (plan 05 §8.5)
    BRK->>FS: (stage) materialize activations/gen-<id>.staging forest (Rust; ZERO Nix) + compute/fsync treeDigest
    BRK->>FS: (prepared) write gen-<id>.manifest.json + gen-<id>.lock.json (+.sha256 sidecars) FIRST, fsync each; THEN gen-<id>.json + fsync file + dir
    BRK->>HELP: (rooted) closed request: publish per-output root set gen-<id>/<safe-id> -> each output (staged-tmp+rename) + fsync
    Note over BRK: rooted durable => release this op's shared GC-inhibit permit (plan 05 §8.5)
    BRK->>FS: (promote) rename activations/gen-<id>.staging -> activations/gen-<id> (idempotent) + fsync activations/
    BRK->>FS: (activated) symlink tmp: current.tmp -> activations/gen-<id> (relative)
    BRK->>FS: rename(current.tmp, current)   %% atomic on POSIX; linearization point
    BRK->>FS: write manifest.json + lock.json as byte-identical copies of the prepared snapshots + fsync
    Note over CLI,HELP: (superseded per-output root sets removed later by the GC phase under the gate's exclusive side)
```

The `current` swap is a **relative** symlink (`current -> activations/gen-<id>`);
the temp-symlink + `rename` + directory-`fsync` recipe is in plan 05 §8.2/§8.4.
`current` is valid **only** if it is a relative link to an existing retained
`activations/gen-<id>` whose `treeDigest` verifies (plan 05 §11). The previous
generation's outputs remain rooted by their own per-output root set until
`history`/`gc` pruning, so **rollback is free**. Because the new generation's
complete per-output root set already exists before the forest is promoted and
`current` is swapped, a crash *after* the swap can never leave `current`
pointing at an unrooted or undocumented forest; a crash *before* the swap leaves
the previous generation active and the in-flight generation unreachable from
`current` (its forest is still at `.staging/`, or already renamed to `gen-<id>/`
but never made `current`). The atomic `current` swap is the linearization
point: recovery completes the op **only** from a verified **activated** state
(`current` already swapped); **every** pre-swap state — stage-only / prepared /
**rooted** — is discarded even when complete, because a rooted state is durable
and safe (outputs GC-protected, record + `treeDigest` intact) but is **not
user-visible authorization to complete after restart** — the user never observed
the swap (§5.7, §9, plan 05 §8.4). The forest is **rebuildable** from the
generation record + the verified rooted outputs, so discarding an uncommitted
forest loses nothing.

### 5.7 Commit

The generation transaction (canonical crash-consistency contract in plan 05
§8.4) orders every filesystem step so the `current` swap is the linearization
point and **both** the per-output GC root set **and** the `.staging → gen-<id>`
rename precede it. Pipeline view (phase names are illustrative; the ordering and
the named states are normative):

1. **stage** (§5.5): Rust materializes the symlink forest at
   `activations/gen-<id>.staging/`, applies the collision policy, computes
   `treeDigest` and verifies it recomputes, and `fsync`s the `.staging` tree +
   the `activations/` dir. **Zero Nix commands.** The forest is **not yet
   renamed**; `current` unchanged.
2. **prepared**: **first** durably write the generation-scoped immutable
   **candidate-view snapshots** `generations/gen-<id>.manifest.json` and
   `generations/gen-<id>.lock.json` (the exact byte bodies the post-`activated`
   `manifest.json`/`lock.json` must equal), each with its `.sha256` sidecar;
   `fsync` each file. **Then** write the immutable record
   `generations/gen-<id>.json` (`kind:"pkg-symlink-forest"`, relative
   `treePath:"activations/gen-<id>"`, `treeDigest`, `entryCount`,
   `collisionPolicy`, sorted `outputRoots[]`, the collision resolutions,
   per-output `outputs[]`, `manifestHash`/`lockHash` (the snapshot body hashes),
   the relative `manifestSnapshot`/`lockSnapshot` paths (plan 05 §5.6),
   `generationHash`); `fsync` the file + `fsync` the `generations/` dir (so the
   snapshots are durable **before** the record that references them — plan 05
   §8.4). `current` unchanged. **`prepared` means BOTH the two candidate-view
   snapshots AND `gen-<id>.json` are durable.** Journal:
   `phase=commit,status=prepared` (carries the snapshot paths + body hashes).
3. **rooted**: the **privileged root-helper — the sole gcroots filesystem writer
   (D-17/ARCH-INV-05/06) — atomically publishes** the **complete per-generation
   root set** `gcroots/pkg/users/<uid>/gen-<id>/` containing **one symlink
   `<safe-id>` → store path per selected output** (each output root protects its
   closure), via the staged-tmp + `rename` protocol of plan 05 §8.3, on a
   **closed validated request from the broker only** (the broker is the sole
   mediator; the user CLI never calls the helper); the helper `fsync`s the dir
   tree. **No** `nix-store --add-root` is used (see [^add-root]). Every selected
   output's closure is now durably rooted, independent of the lease. Once this
   step's `rename` + parent-`fsync` are durable, this op's handle **releases its
   shared GC-inhibit permit** (plan 05 §8.5). `current` unchanged.
4. **promote**: atomically `rename(activations/gen-<id>.staging →
   activations/gen-<id>)` if not already renamed (idempotent across restarts);
   `fsync(<user-state>/activations/)`. The forest now sits at its retained path.
   `current` still unchanged.
5. **activated**: atomic `current` swap → `activations/gen-<id>` (relative
   temp symlink + `rename` + directory `fsync`). `current` now resolves to a
   rooted, documented, `treeDigest`-verified, retained forest.
6. write `manifest.json` and `lock.json` as **byte-identical copies** of the
   durable **prepared** snapshots (`generations/gen-<id>.manifest.json` /
   `.lock.json`); temp + `fsync` + `rename` + directory `fsync`; assert each
   hashes to `manifestHash`/`lockHash` recorded in `gen-<id>.json`. The snapshots
   are the source of truth for these views.
7. **committed**: append `phase=commit, status=committed` (with `nextStateHash`)
   to the journal; `fsync` the journal + `journal/` dir. Emit `Committed` with
   `generationId`.

**Crash behavior** (full recovery-state table in plan 05 §8.4; recovery rules in
§9): a crash before step 5 (the swap) leaves generation N active; the
pre-swap in-flight generation (stage-only / prepared / rooted — forest at
`.staging/` or already promoted to `gen-<id>/` but never `current`) is
unreachable from `current` and recovery cleans it, **including its candidate-view
snapshots** `gen-<id>.manifest.json`/`.lock.json` (and `.sha256` sidecars); the
forest is rebuildable from the record + rooted outputs, so nothing is lost. A
crash at or after step 5 leaves `current` → `activations/gen-<id>`, which is
already rooted and documented; recovery **restores the mutable `manifest`/`lock`
views from the durable prepared snapshots** (if they lag) and finalizes the
`committed` row (idempotent forward recovery). The transaction is restart-safe
and idempotent because the forest is deterministic (`treeDigest`-verified), the
snapshots are durable from **prepared** onward, and the Nix daemon retains
realised output paths (§9). This transaction's atomicity rests **solely** on
the single `current` swap (§5.6); it is not weakened by the `repair`
maintenance op, which is inherently non-atomic (two-phase) with its own
atomicity/recovery owned by plan 05 §10 / plan 07 — repair is never on the
install path (§13).

## 6. Cache-hit / cache-miss behavior matrix

| OS | substituter has path | build allowed & approved? | Action | Exit on failure |
|----|----------------------|--------------------------|--------|-----------------|
| Linux | yes | n/a | substitute | `ACQUIRE_NETWORK` |
| Linux | no  | allowed & approved | native local build (sandboxed) | `BUILD_FAILED` |
| Linux | no  | allowed, not approved | refuse, emit `BuildRequired` requiring approval | `ACQUIRE_NEEDS_APPROVAL` |
| Linux | no  | disallowed (unsupported/broken/impure; sandbox/build-user unavailable; policy-deny) | refuse | `ACQUIRE_NO_BINARY` |
| macOS | yes | n/a | substitute | `ACQUIRE_NETWORK` |
| macOS | no  | allowed & approved | native local build (sandboxed; `_nixbld`) | `BUILD_FAILED` |
| macOS | no  | allowed, not approved | refuse, emit `BuildRequired` requiring approval | `ACQUIRE_NEEDS_APPROVAL` |
| macOS | no  | disallowed (unsupported/broken/impure; sandbox/build-user unavailable; policy-deny) | refuse | `ACQUIRE_NO_BINARY` |

**Substituter reachability:** preflight probes the narInfo HEAD/GET; if the
substituter is unreachable, the product surfaces `CACHE_UNREACHABLE` and does
**not** silently fall back to a build. The user may run `pkg doctor` to
diagnose.

## 7. Build preview & user approval (Linux and macOS, native system)

The build preview renders the **public sanitized `BuildPreview`** (§5.2.1.b) —
the `preview` field of the PreflightReport, carrying the `buildPlanDigest`
pointer to the private canonical `BuildPlan` — together with the volatile
presentation annotations (exact known cache bytes, free bytes, heuristic
size/time estimates for the unbuilt cache-miss outputs). The interactive
prompt (plan 06) shows:

```
Target platform: macOS (arm64)   (native build; sandboxed)
The following need to be BUILT locally (no signed binary on cache.nixos.org):
  • ffmpeg-6.1            closure ≈ 320 MB   est. 8–14 min  (sandboxed)
  • libx264-<ver>         closure ≈  12 MB   est. 1–2 min   (sandboxed)
New downloads: 0 B   New disk ≈ 332 MB (estimate; includes unbuilt outputs)   Free: 9.0 GB
Proceed with local build? [y/N]
```

Approval is **one operation only**: interactive mode prompts **after** the final
public sanitized `BuildPreview` is displayed (carrying the `buildPlanDigest` of
the canonical plan); global `--yes` pre-approves that single
operation non-interactively but the **same preview is still emitted and
journaled**. Approval is recorded in the journal
(`approval: {granted: true, buildPlanDigest, policyVersion, source, ts}` — see
plan 05 §5.4) and is **bound to the canonical `BuildPlan` digest and the
policy version** — if the plan changes, approval is invalid and `pkg` re-prompts (or, non-interactively,
exits `ACQUIRE_NEEDS_APPROVAL`). **`--yes` never overrides a hard refusal**
(`ACQUIRE_NO_BINARY`, §8/I5). V1 has **no** ambient, session, or persistent
build approval: there is no `PKG_YES_TO_BUILDS` env var, no same-session
skipping, and no `build.always_local_after_preview` config toggle.

## 8. Sandbox & resource limits

Applies to **local builds on Linux and macOS** (native system only; I5). Configured
in the generated
`nix.conf` (plan 07) and overridable per-call:

| Knob | Default | Source |
|------|---------|--------|
| `sandbox` | `true` (both platforms) | Nix conf `sandbox` [^sandbox] |
| `sandbox-fallback` | `false` (both platforms; fail closed, never build unsandboxed) | Nix conf `sandbox-fallback` |
| `build-users-group` | `nixbld` (both Linux and macOS; created by the installer) — build users are `nixbld*` (Linux) / `_nixbld*` (macOS) | multi-user [^multiuser] |
| `use-cgroups` | `true` on **Linux only** (requires experimental feature `cgroups` [^exp-features]); off on macOS (Linux-only setting) | conf `use-cgroups` — a **per-build cgroup** for **process grouping**, lingering-process cleanup, and CPU accounting/statistics; **not** a resource cap and **not** isolation (Nix 2.34.8 does not write `memory.max`/`cpu.max`/`pids.max`/IO limits) |
| `max-jobs` | `1` — bounds the number of **concurrent derivations** the daemon builds at once; **not** a CPU/mem/IO cap | conf `max-jobs` [^cores-jobs] |
| `cores` | `0` (use all) — only supplies the `NIX_BUILD_CORES` env var to each builder; a **cooperation hint**, **not** a hard CPU cap | conf `cores` [^cores-jobs] |
| `max-silent-time` | `3600` s — daemon kills a derivation that produces no output for this long | conf `max-silent-time` |
| `timeout` | `86400` s — daemon kills any single derivation after this long (**per derivation**) | conf `timeout` |
| `max-build-log-size` | `268435456` bytes (256 MiB) — daemon **kills** a derivation whose build log exceeds this (**never truncates**) | conf `max-build-log-size` |
| `system-features` | host features | conf `system-features` |
| `require-sigs` | `true` | conf `require-sigs` [^requiresigs] |
| `allow-import-from-derivation` | `false` (both platforms; **pure evaluation** — no realization/import-from-derivation before approval; a derivation requiring IFD is a hard refusal `ACQUIRE_NO_BINARY`) | conf `allow-import-from-derivation` |
| `substituters` | `https://cache.nixos.org` (channel-locked) | conf `substituters` |

These finite defaults are supplied by the root-owned generated `nix.conf` (plan 07
§5.2); per-call `timeout`/`max-silent-time` may only **tighten** them (never
relax). `timeout`/`max-silent-time`/`max-build-log-size` **terminate a builder**
when it stalls/runs too long/streams too much log — they are **not**
memory/CPU/IO caps. `max-jobs=1` bounds concurrent derivations **for one build
operation/request** and does **not** by itself serialize multiple users (each
user's acquire is a separate daemon request).

Preflight checks (each an estimate taken before build start, **not** an ongoing
cap/quota): `pkg` refuses to start a build if free disk at `/nix` <
`new_bytes * 1.2` (a preflight estimate of headroom, not an enforced quota) or
if `loadavg` exceeds a ceiling (`build.max_loadavg`, V1 product-managed
default **2 × logical CPU count**, checked before build start — a preflight
signal, not an ongoing cap).

**Resource boundary — what stock Nix 2.34.8 actually provides (I5).** The Rust `pkg` client and the `nix` CLI it spawns are **socket clients** of the long-lived `nix-daemon`; any RLIMIT or cgroup membership applied to those client processes does **not** constrain the builders the daemon spawns. Stock Nix provides **no** per-build memory/CPU/IO cap. What does hold: `max-jobs=1` bounds concurrent *derivations* (a concurrency bound, not a CPU/mem/IO cap); `cores` only supplies `NIX_BUILD_CORES` (a cooperation hint, not a hard CPU cap); `timeout`/`max-silent-time`/`max-build-log-size` are **daemon-enforced** time/output bounds (they terminate a builder, not cap memory/CPU/IO); and the preflight disk/load checks above.

**Linux sandbox & cgroups.** With `sandbox=true`, Nix implements its **own** Linux namespaces/chroot sandbox — it does **not** invoke bubblewrap [^nix-src-sandbox]. Regular input-addressed derivations are filesystem-sandboxed and network-denied; **fixed-output** derivations remain filesystem-sandboxed but are **intentionally network-enabled** (their output hash is the integrity boundary; on Linux they omit the private network namespace). `__noChroot` is **rejected** under `sandbox=true` (it only bypasses under `sandbox=relaxed`, which `pkg` never uses). V1 blocks impure derivations. Nix's `use-cgroups` (experimental feature `cgroups`, Linux-only) creates a per-build cgroup used for **per-build process grouping**, **cleanup of lingering processes**, and **CPU accounting/statistics** — it does **not** write `memory.max`/`cpu.max`/`pids.max`/IO limit knobs, so it is **not** a per-build resource cap and provides **no security isolation** [^nix-src-sandbox]. `pkg` uses cgroups (when present) **only** as Nix's per-build **process grouping, lingering-process cleanup, and CPU accounting/statistics** facility — it is **neither a per-build resource cap nor a security boundary, nor an aggregate daemon-subtree guardrail**. The aggregate daemon-subtree ceiling is the **distinct Pending systemd `MemoryMax`/`TasksMax`/`CPUQuota` service-cgroup ceiling** below, not `use-cgroups`. S5 (plan 00/11) must validate Linux cgroup v2 and service readiness before this is accepted.

**macOS sandbox.** Nix's Darwin sandbox is supported but uses different, generally narrower primitives than Linux's (D-11); the preview states `sandbox=on` honestly without claiming identical isolation. On macOS the sandbox profile permits network for non-sandboxed (e.g. fixed-output) derivation types.

**Service-manager ceilings — Pending defense-in-depth, not accepted enforcement (distinct semantics).** The two are **not** the same and must not be lumped together:

- **systemd (Linux):** `MemoryMax`/`TasksMax`/`CPUQuota` would be an **aggregate service-cgroup ceiling over the daemon plus all descendants** (daemon + every builder) — a coarse whole-unit limit, not a stable per-build control.
- **launchd (macOS):** `SoftResourceLimits`/`HardResourceLimits` are **inherited per-process RLIMIT ceilings** (`CPU`/`Data`/`FileSize`/`NumberOfFiles`/`NumberOfProcesses`/`ResidentSetSize`/`Stack`; there is no `AddressSpace` key in `launchd.plist`) — **not** an aggregate daemon-subtree ceiling, and several keys are advisory or alter system `sysctls` for system daemons (which can be dangerous system-wide).

Both remain **Pending** defense-in-depth pending real managed-host behavioral evidence (spike S5 / DR-005), and are **not** presented as accepted enforcement.

**Residual.** Because no hard per-build memory/CPU/IO guarantee exists in stock Nix 2.34.8, resource exhaustion during a local build (T-BUILD-2) remains a **disclosed residual** (RISK-07). Before any local build, `pkg` verifies `sandbox=true`/`sandbox-fallback=false` and that build users are ready, and **fails closed** if not.

## 9. Cancellation & restart recovery

- **Cancellation (Ctrl-C / SIGTERM):** the product traps the signal, sends
  `SIGTERM` to the Nix subprocess group, waits up to `cancel_grace_ms`
  (default 5000), then `SIGKILL`. It records `phase=acquire,
  status=cancelled` in the journal, removes the staging symlink if present,
  and leaves generation N active. Exit code `CANCELLED`.
- **Restart recovery:** on startup the product scans the journal tail
  (plan 05). If the last op has no `committed`/`aborted`/`cancelled` row,
  recovery **never** finishes Activate+Commit merely because a staging tree
  exists. The **atomic `current` swap is the linearization point**: a crash
  *before* it — including a complete **rooted** state — leaves generation N
  active and recovery **aborts and discards** the candidate (forest + record +
  per-output root set); a crash *at or after* it (`activated`) rolls forward
  (`manifest`/`lock` + `committed` row). The journal phase is only an
  **integrity-validated hint** — the journal is append-only and hash-chained
  (plan 05 §5.4/§5), and the chain detects corruption/tamper but does **not**
  authenticate the op against a same-uid writer; the decision is made from
  **four pieces of ground truth that must all agree** before any recovery
  action, and **only the `activated` state may finalize**:
  1. the **integrity-validated journal phase** for the op (stage/prepared/rooted/
     activated);
  2. the **immutable generation record** `generations/gen-<id>.json` exists,
     is fsynced, and parses (else the op is at most stage-only);
  3. the **`treeDigest` re-verifies** against the materialized forest — whether
     it still sits at `.staging/` (pre-promote) or has already been promoted to
     `activations/gen-<id>/` (the digest is recomputed either way and must
     match the record exactly); and
  4. the **complete fsynced per-output root set**
     `gcroots/pkg/users/<uid>/gen-<id>/` is present with one root per selected
     output recorded in the generation record (no missing/extra roots).
  - **activated** (`current` → `gen-<id>`, record ✓ + `treeDigest` ✓ + root set
    ✓ + candidate-view snapshots ✓) → proceed: **restore the mutable
    `manifest`/`lock` views from the durable prepared snapshots** (if they lag)
    and finalize the `committed` row (idempotent forward recovery). This is the
    **only** state that proceeds.
  - **stage-only / prepared / rooted** (record absent **or** `treeDigest` fails
    **or** the per-output root set is absent/incomplete **or** the root set is
    complete but `current` still points at the old generation) → all of these
    are strictly **pre-swap**. A complete **rooted** state is durable and safe
    (outputs GC-protected, record + `treeDigest` intact) but is **not
    user-visible authorization to complete after restart** — the user never
    observed the swap, so recovery must **not** promote or activate on their
    behalf. Recovery marks the op `aborted`, leaves generation N active, emits a
    `RecoveryNotice`, and **discards** the candidate: the `.staging/` or
    `gen-<id>/` forest, the generation record, **its candidate-view snapshots**
    (`gen-<id>.manifest.json`/`.lock.json` + `.sha256` sidecars), and the
    candidate per-output root set. Re-running the same op redoes stage→…→activate
    from scratch; the forest
    is deterministic and rebuildable, and the Nix daemon retains already-realised
    paths, so re-substitution is free.
- **Partial multi-target failure:** with `--keep-going`, some targets succeed
  and some fail. The product **does not stage** on any failure; it reports
  per-target results and exits `PARTIAL_FAILURE` with generation N unchanged.

## 10. Progress events, logs, exit codes

### 10.1 Progress event protocol

Progress is split into **two** event streams so the public surface never leaks
raw Nix implementation details:

- **Internal broker events** (PRIVATE; **broker-owned** — owned/writable
  **only** by the unprivileged `pkg-nix-broker` account, never under
  `<user-state>` (plan 07 §6.1/§7.4); held by the managed build
  engine/coordinator; bounded/redacted retention, §10.2). These are the parsed
  `--log-format internal-json` build-log events plus the adapter's own
  diagnostics, and they **may** carry drv path, store path, build `system`,
  internal build id, and argv-derived context — because they are never rendered
  to users or written under `<user-state>`:

```jsonc
// BROKER-PRIVATE — never on CLI/JSON/RPC, never under <user-state>
{ "type":"build_started","op_id":"op_...","drv":"/nix/store/...-.drv",
  "system":"x86_64-linux","name":"ffmpeg-6.1" }
{ "type":"build_progress","op_id":"op_...","drv":"/nix/store/...-.drv","pct":0.42 }
{ "type":"build_finished","op_id":"op_...","drv":"/nix/store/...-.drv","exit":0 }
{ "type":"download_started","op_id":"op_...","path":"/nix/store/...","bytes":1234567 }
{ "type":"download_progress","op_id":"op_...","path":"/nix/store/...","done":700000,"total":1234567 }
```

- **Public normalized events** (PRODUCT-OWNED; the **only** thing emitted to
  the `--jsonl` progress stream of plan 06 §5.3, to the CLI inline renderer of
  plan 06, and to the user-owned per-op log
  `<user-state>/logs/<opId>.ndjson`). They are **never** written to `--json`
  stdout, which carries only the single final result document (plan 06 §5.2).
  They carry **operation id, selector/package display names, phase, and byte
  counts only** — **no** drv path, **no** store path, **no** flake ref, **no**
  Nix argv, **no** trust/store-control knobs. A drv/path arriving on a broker
  event is mapped to the responsible `selector`/`packageName` via the
  operation's target map before it is emitted:

```jsonc
// PUBLIC — the only stream on the --jsonl stdout line and under <user-state>/logs/<opId>.ndjson
// (camelCase field names: opId, packageName, generationId; schemaVersion:1 on every line)
{ "schemaVersion":1, "type":"phase","opId":"op_...","phase":"acquire","status":"started" }
{ "schemaVersion":1, "type":"build_started","opId":"op_...","selector":"ffmpeg","packageName":"ffmpeg","version":"6.1" }
{ "schemaVersion":1, "type":"build_progress","opId":"op_...","selector":"ffmpeg","pct":0.42 }   // best-effort
{ "schemaVersion":1, "type":"download_started","opId":"op_...","selector":"ffmpeg","bytes":1234567 }
{ "schemaVersion":1, "type":"download_progress","opId":"op_...","selector":"ffmpeg","done":700000,"total":1234567 }
{ "schemaVersion":1, "type":"phase","opId":"op_...","phase":"verify","status":"started" }
{ "schemaVersion":1, "type":"phase","opId":"op_...","phase":"stage","status":"started" }
{ "schemaVersion":1, "type":"collision","opId":"op_...","file":"bin/x","selectors":["a","b"] }
{ "schemaVersion":1, "type":"phase","opId":"op_...","phase":"activate","status":"started" }
{ "schemaVersion":1, "type":"committed","opId":"op_...","generationId":"gen-42" }
```

Progress percentages are **best-effort** (Nix's internal-json does not
guarantee a percentage; the product derives one from download bytes and a
build heuristic). Public events are append-only to
`<user-state>/logs/<opId>.ndjson`, **additive, and idempotent to replay**.
The `--jsonl` stream (plan 06 §5.3) ends with exactly one terminal
`type:"result"` record carrying the same success/error/generation summary as
the `--json` final document (plan 06 §5.2), so a consumer detects completion
in-format; that final-result envelope is owned by plan 06.

### 10.2 Logs

- **User-owned logs are sanitized.** Structured NDJSON logs per operation at
  `<user-state>/logs/<opId>.ndjson` carry only the **public normalized** event
  stream (§10.1) — operation id, selector/package display names, phase, byte
  counts, best-effort percentages; **no** drv path, store path, flake ref, Nix
  argv, or trust/store-control knobs. (Raw broker diagnostics are a separate,
  broker-private artifact — last bullet — and never share this path.)
- A rotating `product.log` for non-operation events (startup, doctor, gc),
  likewise sanitized.
- **Raw adapter diagnostics are broker-private and broker-owned** — owned/writable
  **only** by the unprivileged `pkg-nix-broker` account (held in the broker-owned
  `/var/lib/pkg/log/broker` directory of plan 07 §6.1/§7.4, by the managed build
  engine/coordinator, never under `<user-state>`) — with **bounded, redacted
  retention**: drv/store paths are reduced to short
  fingerprints or dropped, and argv/flake refs are stripped; used only for
  `doctor`/parity/fault-injection. The bundled Nix's own logs are captured
  **there** (broker-private), not into user-owned state; a redacted excerpt may
  be surfaced to the user via `pkg doctor` on demand.
- No secrets are logged (channel descriptor keys are public keys; private keys
  never exist on the client — signing is server-side, plan 02/10).

### 10.3 Exit codes

| Code | Symbol | Meaning |
|------|--------|---------|
| 0 | `OK` | success |
| 2 | `USAGE` | bad CLI usage (plan 06) |
| 64 | `RESOLVE_*` | selector resolution failed (not found/ambiguous/broken/unsupported/insecure) |
| 65 | `PREFLIGHT_FAIL` | policy/disk/collision-preview blocked the op before mutation |
| 66 | `ACQUIRE_NETWORK` | substituter unreachable / download failed |
| 67 | `ACQUIRE_NO_BINARY` | no acceptable substitute **and** building is impossible or disallowed (unsupported package/system, sandbox/build-user unavailable, policy-blocked derivation) — not merely a Darwin cache miss |
| 68 | `ACQUIRE_NEEDS_APPROVAL` | build required, not approved (`--dry`/no `--yes`) |
| 69 | `BUILD_FAILED` | local build failed |
| 70 | `VERIFY_FAIL` | NAR/signature/identity mismatch |
| 71 | `STAGE_COLLISION` | symlink-forest collision (or path-escape / file-vs-dir conflict), policy=abort |
| 72 | `STATE_LOCKED` | another operation holds the lease (plan 05) |
| 73 | `STATE_CORRUPT` | state/journal corruption detected |
| 74 | `UNMANAGED_NIX` | unmanaged Nix detected, refusing (plan 07) |
| 75 | `CANCELLED` | user/system cancellation |
| 76 | `PARTIAL_FAILURE` | some targets failed with `--keep-going`; nothing staged |
| 77 | `PERMISSION` | needed privileges not available |
| 78 | `CONFIG` | misconfiguration (store path, channel, PATH) |
| 79 | `ENGINE_UNAVAILABLE` | cannot reach the managed private build engine/coordinator (e.g. daemon socket down, coordinator unreachable) — **distinct from 77 `PERMISSION`** (privileges were fine; the private engine itself could not be contacted) |
| 80 | `RECOVERED` | op recovered from a prior crash; informational (non-default, with `--strict`) |

Codes follow the `sysexits.h` spirit (`64–78` = the `EX__BASE` reserved band;
`79` `ENGINE_UNAVAILABLE` and `80` `RECOVERED` are product extensions beyond it)
and are defined once here; plan 06 maps each command's outcomes to them.

## 11. Failure matrix (selected)

| Phase | Failure | Detection | State after | Exit |
|-------|---------|-----------|-------------|------|
| resolve | attribute missing | `nix derivation show` empty/404 | unchanged | 64 `RESOLVE_NOT_FOUND` |
| resolve | meta.broken | eval of `meta.broken` | unchanged | 64 `RESOLVE_BROKEN` |
| preflight | disk < 1.2× new | statvfs | unchanged | 65 |
| preflight | insecure | meta.knownVulnerabilities | unchanged | 65 |
| acquire | substituter offline | narInfo GET error | unchanged; daemon may have partial paths (harmless) | 66 |
| acquire | build fail (Linux/macOS) | build exit≠0 | unchanged | 69 |
| acquire | kill -9 mid-build | journal tail | gen N active, op `aborted` on next start | 75/`RECOVERED` |
| verify | NAR mismatch | `nix store verify` | unchanged; quarantined path; SECURITY event | 70 |
| stage | collision / path-escape / file-vs-dir conflict | Rust forest walk | unchanged | 71 |
| stage | forest integrity | `treeDigest` mismatch / incomplete forest | unchanged | 73 |
| activate | rename fails (EIO) | errno | gen N active; op aborted | 73 |
| commit | crash during the transaction (§5.7) | journal tail + fs state | the `current` swap is the linearization point: **all pre-swap states** (stage-only/prepared/**rooted**, including a complete verified root set + durable snapshots) are **aborted and discarded** — generation N stays active; candidate forest + record + candidate-view snapshots + per-output root set cleaned (rooted is durable/safe but not authorization to complete after restart); **only `activated`** (`current` → gen-<id>, record ✓ + `treeDigest` ✓ + complete root set ✓ + snapshots ✓) **proceeds**, **restoring `manifest`/`lock` from the durable prepared snapshots** + finalizing `committed` (gen N+1 already rooted + documented) | `RECOVERED` |
| any | lease held by other pid | flock/lease (plan 05) | unchanged | 72 |

## 12. Package & binary collisions and multi-output

### 12.1 Multi-output selection

- Default outputs = `meta.outputsToInstall` from the resolved derivation
  (*confirmed nixpkgs convention* [^outputs-to-install]); fallback `["out"]`.
- `--with-outputs out,lib,dev` overrides per selector; persisted in
  desired-state selector.
- Only the selected outputs' store paths are walked into the forest (one leaf
  symlink per file); the full closure of those outputs is what gets rooted
  (one GC root per selected output).

### 12.2 Collision policy

- Authoritative detection: the **Rust symlink-forest walk** at Stage (§5.5), in
  **stable source order** (canonical selector id → output name → relative path).
- `--on-collision` — V1 policies are **only** these three; there is **no**
  `keep-all`/shadowing:
  - `abort` (default) → `STAGE_COLLISION` with the colliding relative path and
    the selector/output pairs that produce it.
  - `keep-first` → for each colliding relative path, the record **earliest** in
    stable source order wins; the loser's entry is omitted for that path. Every
    **non-conflicting** file from every output is still retained.
  - `keep-last` → the record **latest** in stable source order wins; otherwise
    identical to `keep-first`.
  - All winners **and** losers are recorded in the generation metadata
    (`collisionResolutions[]`) and the journal (§5.7; plan 05 §5.3).
- A **path-escape** (entry whose relative path leaves the forest root) and a
  **file-vs-directory conflict** (same relative path is a file in one output and
  a directory in another) are **hard failures** (exit 71), never governed by the
  collision policy.
- Preflight heuristic (§5.2) warns *before* approval so users don't waste a
  build on an obviously-colliding pair (e.g., two `python` interpreters).

## 13. Security considerations (detailed model in plan 08)

- **No arbitrary evaluation surface:** all `nix` invocations are constructed
  by the adapter from the channel descriptor; user selectors are validated
  against an allowlist grammar (attribute name regex + structured version
  pref) before becoming argv. No shell interpolation; argv arrays only.
- **Trust is channel-locked:** substituters and trusted keys come from the
  signed channel descriptor (plan 02) and are written to a root-owned
  `nix.conf`; the product ignores user env overrides (`NIX_SUBSTITUTERS`,
  `NIX_TRUSTED_PUBLIC_KEYS`) and `--override` of trust knobs.
- **Pure, IFD-free evaluation:** all resolution/`BuildPlan` evaluation is **pure** — locked flake inputs (rev+narHash from the signed descriptor), no `--impure`/`--override-input`, and the managed `nix.conf` pins `allow-import-from-derivation = false` (plan 07 §5.2). Nothing in the closure is realized before the user approves a deterministic `BuildPlan`; a derivation requiring import-from-derivation is a hard refusal (`ACQUIRE_NO_BINARY`), never overridden by approval.
- **Local build integrity (Linux and macOS):** `sandbox=true` + `sandbox-fallback=false`, build-user isolation (the `nixbld` group; `nixbld*` on Linux, `_nixbld*` on macOS),
  and `require-sigs` for substitutes. `pkg` fails closed if sandbox or build-user
  readiness cannot be verified; approval never overrides a hard policy refusal
  (unsupported/broken/impure derivation). Regular derivations are
  filesystem-sandboxed and network-denied; fixed-output derivations are
  network-enabled with their output hash as the integrity boundary. Stock Nix
  2.34.8 provides **no** per-build memory/CPU/IO cap (`max-jobs=1` bounds
  concurrency; `timeout`/`max-silent-time`/`max-build-log-size` are daemon bounds; Nix's
  Linux `use-cgroups` is process grouping/cleanup/statistics, not caps;
  disk/free-space/load preflight; service-manager ceilings are Pending
  defense-in-depth), so resource exhaustion is a disclosed residual (RISK-07).
  Locally-built paths are tagged `provenance: local-build` and never claimed to
  be cache-signed; on macOS they are **not** individually Apple-notarized by `pkg`.
- **Verify-before-activate:** the forest is built only from already-verified selected output store paths; Stage never activates an unverified/unrooted forest, and `current` is valid only if `treeDigest` verifies (plan 05 §11).
- **No network at activate/commit:** those phases are local-only.
- **Reproducibility audit:** the generation manifest records exact store paths
  + narHash + nixpkgs_rev per output, so any generation is independently
  reproducible/verifiable later (foundation for `repair`, plan 05).
- **Privilege boundary — broker vs root helper (ARCH-INV-05/06/07):** normal
  acquire/build/verify runs entirely in the **enforced singleton unprivileged
  broker** — the sole **general** daemon client / sole spawner of the bundled
  `nix` CLI for all normal operations (evaluate, cache/substitute, approved
  local build, `path-info`, read-only `nix store verify`, liveness-respecting
  GC), a daemon `allowed-user` **never** a Nix `trusted-user` (root is the only
  `trusted-user`) — which owns the `nix-driver`. The **root helper** is the
  privileged **root-set filesystem writer** (`/nix/var/nix/gcroots/pkg/…`),
  reached on a **closed validated request from the broker only**, and is
  additionally the **one narrow exception** to the broker's sole-general-client
  status: it runs the privileged **two-phase `nix store repair`** maintenance op
  as root (plan 01 §12.4 / plan 05 §10 / plan 07 / plan 08), only after the
  broker's own read-only `nix store verify` confirms corruption. The user CLI
  never calls either. The build-admission permit (§5.3.1) and the GC-inhibit
  permit (§5.3/plan 05 §8.5) are both **broker-internal**; neither is a
  root-helper operation. The install path performs **no root-mediated repair**
  and reaches the root helper **only** for GC-root filesystem publication
  (§5.7); actual repair (two-phase verify → cache-only/substitution → approved
  rebuild + re-root + forest rebuild) is the separate plan 05 §10 / plan 01
  §12.4 / plan 07 / plan 08 flow, never folded into install/acquire/commit, and
  its non-atomicity does not weaken this plan's atomic `current` swap (§5.6/§5.7).

## 14. Dependencies on other plans

- **plan 00** — product decisions & naming referenced here as authoritative.
- **plan 01** — layered architecture: where the Nix adapter, resolver, and
  pipeline live; the bundled-runtime boundary.
- **plan 02** — signed channel descriptor schema (substituters, keys,
  Nixpkgs rev, policy version) consumed by Resolve/Preflight/Verify.
- **plan 03** — disposable index used for `search`/`info`; Resolve itself
  re-evaluates the pinned Nixpkgs (index is not authoritative for identity).
- **plan 05** — state schema, generations, current-swap, GC roots, leases,
  journal, recovery — the storage substrate this pipeline writes to; **§8.4**
  (generation transaction / prepared candidate-view snapshots), **§8.5**
  (broker-internal machine-global GC admission gate + GC-inhibit permit), and
  **§12** (per-user state lease) are the authority for the three admission
  mechanisms named in §5.3/§5.3.1.
- **plan 01 §8/§11 + plan 07 §7.4** — the enforced singleton unprivileged
  broker boundary (sole **general** daemon client / `allowed-user`, never
  `trusted-user`; the one narrow exception — the root helper's two-phase
  `nix store repair` — is plan 01 §12.4 / plan 05 §10), the closed-request
  RPC surface to the root helper, and the **blocking**
  detailed-broker follow-on that specifies framed-RPC, operation-handle
  lifecycle, handle expiry/disconnect semantics, and child-containment
  supervision for realize/build/gc — this plan states the ordering invariants
  (permit-before-realize, release-after-rooted-or-abort, one-build-at-a-time)
  but not the wire formats.
- **plan 06** — CLI flags, prompts, human output, completion that drive this
  pipeline; exit-code mapping.
- **plan 07** — bundled Nix version, `nix.conf`, store prefix, daemon, the
  unmanaged-Nix refusal that gates all of the above.
- **plans 08–10** — threat model, fault-injection test lanes, release signing.

## 15. PR-shaped implementation checkpoints

> Sized for serious review; exact module paths finalized when implementation
> begins (see plan 11 PR DAG). Each checkpoint is independently mergeable and
> tested against the acceptance criteria.

- **PR-A — Nix adapter & JSON contracts (no pipeline).** Wraps the bundled
  `nix`; implements `derivation_show`, `eval`, `path_info` (`--json-format 2`),
  `build`, `store_verify` with typed JSON deserialization and the
  `internal-json` log parser. (GC roots are plain symlinks created by the
  root-helper, **not** a Nix subprocess — activation is Nix-free.) *Acceptance:*
  golden-file JSON round-trips for each call; CI lint that this is the only
  `nix*` caller.
- **PR-B — Resolver.** Selector grammar + validation; resolve against a
  fixture Nixpkgs; emits RealizedIdentity. *Acceptance:* unit tests for
  not-found/ambiguous/broken/unsupported/insecure; pinned-rev determinism.
- **PR-C — Preflight.** Closure preview, cache-hit/build classification,
  disk/policy checks, collision heuristic. *Acceptance:* PreflightReport
  schema tests; dry-run parity with real acquire.
- **PR-D — Acquire (substitute-only).** `--max-jobs 0` path on Linux+macOS;
  verify phase; progress events. *Acceptance:* cache-hit install of a small
  package end-to-end against a fake substituter; verify-fail → 70.
- **PR-E — Local build (cross-platform).** Build preview (with target system
  + sandbox status), approval gate, sandboxed native build, `BUILD_FAILED`
  mapping, and `ACQUIRE_NO_BINARY` for impossible/disallowed builds. *Acceptance:*
  hermetic build of a trivial derivation in the test lane on Linux **and** macOS
  (native sandboxed build under `nixbld`/`_nixbld`); a build that is disallowed
  (unsupported/impure derivation, or sandbox/build-user unavailable, or
  policy-deny) returns `ACQUIRE_NO_BINARY`; a required-but-unapproved build
  returns `ACQUIRE_NEEDS_APPROVAL`.
- **PR-F — Stage + collision policy (ZERO Nix).** Rust symlink-forest
  materializer: walk selected outputs (no symlink-follow), merge dirs + leaf
  symlinks to `/nix/store` targets, reject path-escape/file-vs-dir conflicts,
  collision policies (abort/keep-first/keep-last), `treeDigest`. *Acceptance:*
  collision + path-escape + file-vs-dir fixtures for each policy; `treeDigest`
  round-trips.
- **PR-G — Activate + Commit + Journal integration** (depends on plan 05 PRs
  for state primitives). *Acceptance:* kill -9 at each transaction state
  (stage/prepared/rooted/activated/committed, §5.7/§9) recovers correctly —
  **every pre-swap state (stage/prepared/rooted) is aborted and discarded**
  (generation N stays active; candidate forest + record + candidate-view
  snapshots + per-output root set cleaned), and **only `activated`** restores
  `manifest`/`lock` from the durable prepared snapshots + finalizes `committed`
  via re-verified `treeDigest` + complete root set; the GC root always exists
  before `current` switches; a failed stage leaves gen N active.
- **PR-H — Cancellation & resource limits.** Signal handling, per-call
  knobs, disk/load guards. *Acceptance:* SIGINT during build → 75, gen N
  intact.
- **PR-I — End-to-end wiring for `install`/`upgrade`/`remove`** (command
  shells live in plan 06). *Acceptance:* full acceptance criteria below.

## 16. Testable acceptance criteria

1. `pkg install ripgrep` (cache hit, Linux and macOS) results in: a new
   generation record listing the exact `storePath`/narHash/nixpkgsRev;
   `current/bin/rg` is a relative symlink whose target resolves to that store
   path (a forest leaf); the previous generation's outputs are still GC-rooted
   (per-output roots) and `rollback` restores them instantly.
2. Killing the product with `SIGKILL` during acquire or the generation
   transaction (§5.7), then re-running, either resumes and commits or aborts
   cleanly — **generation N is always still active and `current` is never a
   broken or unrooted symlink** (the per-output root set always precedes the
   swap, and `current` resolves to a `treeDigest`-verified forest). A pre-swap
   crash discards the candidate forest **and** its candidate-view snapshots; an
   `activated` crash restores the mutable `manifest`/`lock` views from the
   durable prepared snapshots (plan 05 §8.4/§11).
3. On macOS, a package that is `meta.broken`/unsupported on `aarch64-darwin`
   (or whose derivation requires forbidden impurity/unsandboxed execution, or
   when the sandbox/build users cannot be made ready) fails with exit 67
   `ACQUIRE_NO_BINARY` and a calm reason, even with approval; a buildable cache
   miss does **not** fail at 67.
4. On Linux and macOS, a buildable cache-miss install without approval exits
   68 `ACQUIRE_NEEDS_APPROVAL` and stages nothing; with `--yes` it builds under
   `sandbox=true`/`sandbox-fallback=false` (native system, `nixbld`/`_nixbld`)
   and commits; cancelling the preview leaves generation N active.
5. A collision between two selectors with default policy exits 71 and leaves
   the previous generation active; with `--on-collision=keep-first`/`keep-last`
   it commits, retains every non-conflicting file, and the generation record +
   journal record all winners/losers. There is no `keep-all` policy.
6. Every Nix subprocess emits structured output **where the subcommand supports
   it** (`--json`/`internal-json`), enforced by adapter unit tests + CI lint.
   `nix derivation show` is the unconditional-JSON exception (bundled Nix 2.34.8
   emits JSON and has no `--json` flag), and the enumerated **status-only**
   commands (the status-only **verify/gc** commands, e.g. `nix store verify`) are
   exempted because they have no JSON mode — they instead satisfy a checked
   **postcondition** (zero exit **plus** a follow-up integrity recheck via a
   JSON-capable command). **Activation (Stage/Activate) invokes zero Nix
   subprocesses** at all.
7. `pkg install --jsonl` emits a valid NDJSON stream (every line
   `schemaVersion:1`) including at least one event of each of: resolve,
   preflight, acquire/download, verify, stage, activate, commit, terminated by
   a single `type:"result"` record; `pkg install --json` emits **one** final
   document and no stream (plan 06 §5.2/§5.3).
8. After a forced verify failure (corrupted path), the product exits 70,
   writes a SECURITY event, and leaves generation N active.
9. Mixed-rev generation: install `a` at channel rev R1, then `update` to R2
   and `install b`; manifest shows `a.nixpkgs_rev=R1`, `b.nixpkgs_rev=R2`,
   and the tree activates without error.
10. `nix store verify --recursive` on every output in generation N+1 passes
    (NAR integrity) for cache-sourced paths.
11. Evaluation is pure and IFD-free: the managed `nix.conf` sets
    `allow-import-from-derivation = false`; no resolve/preflight call passes
    `--impure`; `nix derivation show --recursive` (the single canonical derivation
    route for the `BuildPlan`) returns the JSON v4 envelope and realizes **nothing**
    before approval. A derivation that requires import-from-derivation is a hard
    refusal (`ACQUIRE_NO_BINARY`, 67), even with approval.
12. The canonical `BuildPlan` (PRIVATE, held by the managed build engine; never
    serialized raw to `--json`/human preview/RPC) is deterministic: re-canonicalizing
    the same descriptor/system/operation-wide-targets/union-derivation-closure/cache-classification/
    readiness/resource snapshot yields a byte-identical object and
    `sha256:<lowercase hex>` digest (RFC 8785 JCS). The digest binds `nixRuntimeVersion`,
    `descriptorHash`, `policyVersion`, `channelSeq`, `nixpkgs.rev`+`nixpkgs.narHash`,
    `system`, the operation-wide `targets[]` (sorted by canonical selector id),
    `derivationClosure` (`closureDigest`/`derivationCount` over the UNION closure), the sorted `builds` (per-build
    `derivationDigest` + safe `fixedOutput`/`networkEnabled`), `cacheClassification`
    (`classificationDigest`/`hits`/`misses`/exact known-cache `downloadBytes`+
    `narBytes`), `readiness`, and the fixed `resources`/`admission`; it binds **no**
    per-cache-miss `narSizeBytes`, **no** `estimatedClosureBytes` (an unbuilt miss's
    size is unknowable pre-build), and **no** raw command-string/flake-ref source
    field; the cache identity is `classificationDigest`, not the counts. The approval
    receipt binds opId + that digest + `policyVersion`. The PUBLIC sanitized
    `BuildPreview` (`platform`, `policyVersion`, `buildPlanDigest` pointer, selector/
    package names, build count/names, fixed-output/network notice, exact known cache
    bytes, unknown-local-output count, heuristic size/time estimates outside the
    digest, and the product `readiness` summary with `sandboxed`/`buildIsolationReady`/
    `nativeBuild` and an honest `resourceBoundary`) is the only object rendered and
    **omits** all raw flake refs/drv paths/derivation expressions/Nix argv/trust/
    store-control knobs **and Nix system triples and builder-user/cgroup internals**
    (kept in the private `BuildPlan`).
13. The **private** `BuildPlan` `readiness` (§5.2.1.a) is a stable cross-platform
    schema: every `BuildPlan` carries `buildUsersGroup`, `buildUsersReady`,
    `useCgroupsEnabled`, `cgroupV2Ready` (and sandbox) explicitly — Linux reports
    the cgroup fields `true` when local builds are allowed; macOS reports them
    `false` (never absent). The **public** `BuildPreview` `readiness` (§5.2.1.b)
    exposes **only** `sandboxed`/`buildIsolationReady`/`nativeBuild` and the honest
    `resourceBoundary` — never the system triple, builder-user group, or cgroup
    internals.
14. Cache-only acquisition uses `nix build --max-jobs 0 --no-link --json` against
    **explicit derived-output paths** (`/nix/store/<x>.drv^out,man`), never a bare
    `.drv`; it never combines `--no-link` with `--out-link`, and no per-call
    `--substituters`/`--trusted-public-keys` flags are passed (trust comes from the
    managed root-owned `nix.conf`). An approved local build passes **no**
    `--substituters ""`/`--builders ""`; a binary appearing meanwhile may safely
    substitute and actual provenance is recorded.
15. Activation is Nix-free and integrity-checked: Stage/Activate run **zero** Nix
    subprocesses; the forest is a relative-symlink tree under
    `<user-state>/activations/gen-<id>/`, `current -> activations/gen-<id>` is a
    **relative** link, `treeDigest` (SHA-256 over canonical
    `{relativePath,storeTarget,sourceSelector,output}` records) round-trips, and
    `current` is rejected as invalid when the forest is missing/damaged/tampered
    (fails the `treeDigest` recheck). The generation record carries
    `kind:"pkg-symlink-forest"`, `treePath`, `treeDigest`, `entryCount`,
    `collisionPolicy`, sorted `outputRoots[]`, and collision resolutions — and
    **no** `activation.storePath`/`builder`/`buildenvInputs`.
16. **Machine-global build admission is a broker-internal permit, not a flock.**
    There is **no** `/var/lib/pkg/run/build-admission` backing file and **no**
    in-kernel `flock` for build admission; the gate lives inside the enforced
    singleton unprivileged broker, exactly one approved local-build operation
    holds the permit machine-wide at a time, a waiter is queued fairly and may
    cancel (exit 75), and a disconnect/cancel/error or broker crash releases the
    permit only **after** the broker contains its build children — a replacement
    broker completes its startup recovery barrier before admitting the first
    build. Pure-substitution acquire never takes the permit. The per-user state
    lease (plan 05 §12) remains a user-owned filesystem `flock` and is distinct.
17. **GC-inhibit permit spans realize→root.** Before the broker dispatches the
    substitute/build/realization, the op's handle acquires a **shared GC-inhibit
    permit** from the broker-internal machine-global GC admission gate (plan 05
    §8.5); it is retained across the CLI return, Rust forest staging, prepared
    snapshots, and broker-mediated root-helper publication, and released only once
    the per-output root set is durable (**rooted**) or the op aborts. A `gc`
    started for any uid takes the gate's exclusive side and waits for all shared
    permits to drain, so it never collects this op's realize→root outputs. The
    per-user lease is **never** claimed to protect against another uid's global GC.
18. **Prepared writes snapshots before the record.** At the **prepared** step the
    broker durably writes+fsyncs `gen-<id>.manifest.json` and `gen-<id>.lock.json`
    (+`.sha256` sidecars) **before** `gen-<id>.json`; post-`activated` recovery
    restores the mutable `manifest.json`/`lock.json` from those snapshots;
    discarding a prepared/rooted candidate deletes its snapshots too.

## 17. Unresolved questions / spikes

- **Q4.1 buildEnv vs Rust symlink farm (RESOLVED → Rust symlink forest).** The
  activation tree is a **Rust-materialized, user-owned symlink forest** outside the
  Nix store, built from already-verified selected output store paths. **Activation
  invokes zero Nix commands.** Nix owns downloads/builds/store; `pkg` owns the
  deterministic forest (merge dirs + leaf symlinks to absolute `/nix/store`
  targets), whose integrity is enforced by `treeDigest` and whose collisions are
  detected by the Rust walk (abort/keep-first/keep-last only). The buildEnv
  store-object approach was rejected: it couples activation to a Nix
  evaluation/build and makes the activation a store object the user cannot own or
  tamper-detect directly.
- **Q4.2 Build-time estimation.** Source of per-system build-time priors for
  the preview. *(Default: ship a coarse heuristic table; refine from
  opt-in telemetry in a later release — see plan 12.)*
- **Q4.3 `internal-json` stability.** The internal-json log format is
  nominally internal; confirm version-pinning of the bundled Nix (plan 07)
  makes it a stable contract for us. *(Required spike — see plan 07 §"bundled
  runtime pinning".)*
- **Q4.4 Multi-user authoritative state (RESOLVED → D-17).** Package environment
  state (manifest/lock/generations/activation/journal) is **per-user, keyed by uid**;
  only the runtime/channel/index/source/store service is root-owned and shared. This
  plan operates on `<user-state>` (doc 01 §9.3). Mixed Nixpkgs revisions remain
  per-selector within a user's lock.
- **Q4.5 Partial substitution races.** If a narInfo appears between preflight
  and acquire, a "build required" preview may turn into a cache hit (fine) or
  vice-versa (needs re-approval). *(Default: re-run preflight if the
  classification flips; never silently build.)*

## 18. Sources (current Nix behavior)

[^drv-show]: `nix derivation show` emits JSON **unconditionally** in the bundled
Nix 2.34.8 and does **not** accept a `--json` flag; the recursive form
`nix derivation show --recursive <installable>` returns the JSON v4 envelope
`{"version":4,"derivations":{...}}`. Nix Reference Manual, command
reference → https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3-derivation-show.html
[^path-info]: `nix path-info --json --json-format 2 --recursive` (the path-info v2
envelope includes `path`/`narHash`/`narSize`/`deriver`/`references`/`signatures`;
there is no `--deriver` flag),
https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3-path-info.html
[^outputs-to-install]: `meta.outputsToInstall`, Nixpkgs Reference Manual,
"Meta-attributes" → https://nixos.org/manual/nixpkgs/stable/#sec-meta
[^substituters]: `substituters`, Nix conf →
https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-substituters
[^trustedkeys]: `trusted-public-keys` / per-substituter `public-keys`, Nix conf
→ https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-trusted-public-keys
[^requiresigs]: `require-sigs`, Nix conf →
https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-require-sigs
[^store-verify]: `nix store verify`, Nix Reference Manual →
https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3-store-verify.html
[^add-root]: `--add-root` is an **option** on `nix-store` operations
(`--realise`/`--install`/…), **not** a standalone subcommand and **not** the mechanism
`pkg` uses to register an already-realized path. `pkg` creates GC roots by placing a
symlink directly in the daemon's scanned gcroots directory
(`/nix/var/nix/gcroots/pkg/users/<uid>/`); Nix treats any symlink in a scanned gcroots
directory as a root. → https://nixos.org/manual/nix/stable/command-ref/nix-store.html#opt-add-root ;
garbage-collection roots → https://nixos.org/manual/nix/stable/package-management/garbage-collection.html
[^log-format]: `--log-format internal-json`, Nix Reference Manual →
https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3.html#description
(common options, log-format)
[^sandbox]: `sandbox`, Nix conf →
https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-sandbox
[^multiuser]: Multi-user installation, Nix Reference Manual →
https://nixos.org/manual/nix/stable/installation/multi-user.html
[^cores-jobs]: `cores` vs `max-jobs`, Nix Reference Manual (advanced topics) →
https://releases.nixos.org/nix/nix-2.34.8/manual/advanced-topics/cores-vs-jobs.html
(`max-jobs` bounds concurrent derivations; `cores` only sets `NIX_BUILD_CORES`,
a cooperation hint — neither is a hard CPU/memory/IO cap).
[^exp-features]: Experimental features (`cgroups`), Nix Reference Manual →
https://releases.nixos.org/nix/nix-2.34.8/manual/development/experimental-features.html
[^nix-src-sandbox]: Nix 2.34.8 implements its own Linux namespaces/chroot
sandbox and does **not** invoke bubblewrap; `use-cgroups` is Linux-only and does
not write `memory.max`/`cpu.max`/`pids.max`/IO limits. Verified against
`src/libstore/include/nix/store/local-settings.hh`,
`src/libstore/unix/build/linux-derivation-builder.cc`,
`src/libstore/unix/build/derivation-builder.cc`, and
`src/libstore/unix/build/darwin-derivation-builder.cc` at tag `2.34.8` →
https://github.com/NixOS/nix/blob/2.34.8/src/libstore/unix/build/linux-derivation-builder.cc
