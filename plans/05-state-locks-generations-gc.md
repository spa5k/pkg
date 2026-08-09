# 05 — State, Locks, Generations, GC

> Owner: execution track. **Planning only**; no Rust code. Cross-references by
> number; see [Dependencies](#15-dependencies-on-other-plans).

## 1. Purpose

Define the **authoritative state model** the product owns and operates on.
Because the product hides Nix, **Rust-owned state is the source of truth**.
Nix owns downloads/builds/the store only; the **activation is a Rust-owned
symlink forest outside `/nix/store`** (D-18/INV-11) and there is **no Nix
profile** — realized outputs live in `/nix/store` and are kept alive by the
product's per-output GC roots, while the activation forest is materialized and
rebuildable from this state. This document specifies:

- State directory layout and ownership.
- Exact schema (versioned) for: desired state, lock, generation manifest,
  channel cache, journal/leases.
- User-intent selector vs realized identity (canonical definition, referenced
  by plan 04).
- Mixed Nixpkgs revisions during selective upgrades.
- Generations, the atomic `current` swap, GC roots.
- Operation leases and the append-only journal that drive crash recovery.
- Migrations, corruption detection, and recovery procedures.

## 2. Scope / Non-scope

**In scope**

- All on-disk state the pipeline (plan 04) reads/writes.
- Schema, versioning, migrations, integrity (checksums/fsync).
- Concurrency model (per-user lease + machine-global GC admission, §8.5/§12),
  recovery model (journal) — including the **recovery barrier** (no command
  observes state until startup recovery reconciles it).
- GC root topology, the machine-global GC admission gate (§8.5), and how `gc`
  interacts with generations.

**Non-scope**

- *What* gets resolved/built and *how* (plan 04).
- *Where* the Nix store/daemon live and privilege (plan 07).
- CLI verbs that surface this state (`history`, `rollback`, `gc`, `list`)
  (plan 06), though their semantics are defined here.
- Signing of the channel descriptor (plan 02) and the search index (plan 03).

## 3. Invariants

| # | Invariant | How enforced |
|---|-----------|--------------|
| I1 | **Rust state is authoritative.** Nix profile contents are derived; the product never reads Nix profile state as ground truth. | Activation writes both; recovery rebuilds activation from manifest, never the reverse. |
| I2 | Every persisted file is **fsynced before the pointer to it is renamed**, and carries a checksum + schema version. | Write-temp → fsync → atomic rename; `sha256` footer / sidecar. |
| I3 | At most one **mutating** operation per user runs at a time (exclusive per-user lease, §12). Reads never block writers and never see a torn state: a reader either takes a **shared** per-user lease (`LOCK_SH`) for a consistent read of the mutable `manifest`/`lock` views, or reads **lock-free** from the immutable active-generation snapshots (§5.6). **No command reads or mutates state until startup recovery has completed** (§11; the recovery barrier, §12). | Per-user `flock(LOCK_EX)` on `run/lease` for mutators, `LOCK_SH` for leased readers; lock-free readers use `generations/gen-<active>.manifest.json`/`.lock.json`. |
| I4 | A committed generation is **immutable and content-addressed by its manifest hash**. | Manifest written once under `generations/<id>.json`; never edited. |
| I5 | Every selected output of an active generation has a live GC root for the lifetime of that generation — and the per-output root set is created and fsynced **before** `current` is switched to it. | The generation transaction (§8.4) creates the per-generation root set `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/<safe-id>` (one symlink → store path per selected output; §8.3) at the **rooted** step, *before* the atomic `current` swap (the **activated** step); each mutating op's handle holds a **shared GC-inhibit permit** (§8.5) — acquired **before** the broker dispatches the substitute/build/realization that can create unrooted outputs (plan 04 acquire) and held until the root set is durable (rooted) or the op aborts — which protects not-yet-`current` outputs from *any* user's broker `gc`, after which the root set protects every output's closure durably (so the swap never lands on an unrooted forest). Pruning removes the root set only when the generation is deleted, **last** (§9.1). |
| I6 | `current` is **always a valid relative symlink** (`current -> activations/gen-<id>`) to an existing retained, `treeDigest`-verified activation forest, or absent (pre-first-install). | Atomic temp-`symlink`+`rename`; never unlink-then-recreate; integrity re-checked via `treeDigest` (§11). |
| I7 | The journal is **append-only**, **self-describing**, and **hash-chained**; the longest valid prefix is recoverable and only a torn final suffix is discarded. | Each row carries `schemaVersion`, monotonic `seq`, `prevRowHash`, `rowHash` (SHA-256 of the RFC 8785/JCS canonical row excluding `rowHash`, chained to the prior accepted row), opId, phase, status, prev/next state hashes, ts (§5.4). |
| I8 | **Crash safety ≠ reconciled product state.** Immediately after a crash, `current` may already point at the *new* generation while the mutable `manifest.json`/`lock.json` views still reflect the *previous* one — this is *safe* (the new forest is rooted + documented + `treeDigest`-verified) but not yet *reconciled*. The fully-reconciled state is reached only after startup recovery restores the current views from the generation's durable snapshots (§5.6) and appends the `committed` row. | Recovery barrier (I3) + activated-state forward finalization (§8.4); views restored from `generations/gen-<current>.manifest.json`/`.lock.json`. |
| I9 | **Machine-global GC safety.** The product's broker is the sole GC mediator: every `nix store gc` is admitted only by the **broker-internal machine-global GC admission gate** (§8.5) — a fair counted read/write admission structure inside the enforced singleton broker — which grants GC the exclusive permit only after every in-flight op's shared **GC-inhibit permit** has drained. The V1 installer schedules **no** automatic GC (no timer/service/launchd job) and removes/disables only its own scheduler artifacts; all product GC is explicit and broker-mediated. A root set is durable before `current` switches (I5). | Broker-internal counted R/W admission gate (one shared GC-inhibit permit per op handle; one exclusive permit for GC); per-user leases do **not** gate the global collector, and there is **no** on-disk GC lock (§8.5/§9). |

## 4. Directory layout

> **Two roots (D-17/INV-10).** (a) A **root-owned, machine-global service root**
> `/var/lib/pkg/` holds the immutable runtime/channel/index/source/cache/log service
> (shared, read-only to users). (b) A **per-user authoritative root** `<user-state>`,
> keyed by OS uid, holds manifest/lock/generations/current/journal/cache/logs and is
> owned by that uid (mode `0700`):
>   Linux `$XDG_DATA_HOME/pkg/` (default `~/.local/share/pkg/`);
>   macOS `~/Library/Application Support/pkg/`;
>   root-owned fallback `/var/lib/pkg/users/<uid>/` for accounts without a usable HOME.
> Concrete OS paths/permissions are finalized in **plan 07**. Authoritative package
> state is **never** globally shared across users.

```
# (a) machine-global service root — root-owned, shared (D-17)
/var/lib/pkg/
  channel/
    tuf/{root,timestamp,snapshot,targets}*.json  # TUF metadata cache (plan 02)
    descriptor.json           # the currently-trusted channel descriptor; its
                              #   integrity is a TUF TARGET (plan 02 §7) — there
                              #   is NO separate .asc sidecar
    previous/                 # retained previous descriptor(s) for rollback
  index/<channelSeq>/         # disposable search index (plan 03); shared, read-only
  nixpkgs/<rev>/              # pinned catalog source (plan 03); shared
  cache/                      # service downloads (root-owned)
  log/                        # service logs (daemon/helper)
    broker/                   # broker-PRIVATE raw adapter/Nix subprocess logs — parent 0700, files 0600,
                              #   broker-owned, keyed by an INTERNAL id (NOT the user's opId verbatim); never
                              #   user-rendered and never copied verbatim into <user-state>. Raw Nix output may
                              #   contain store paths/derivers/cache URLs and must NOT live in per-user state;
                              #   only sanitized, schema-versioned NDJSON is COPIED OUT to
                              #   <user-state>/logs/<opId>.ndjson (§4/§6/§10).
  # NOTE: there is NO /var/lib/pkg/run/ backing file. Both the machine-global
  #   local-build admission permit (plan 04 §5.3.1) and the machine-global GC
  #   admission gate (plan 05 §8.5) are broker-internal, in-process structures
  #   inside the enforced singleton broker — NOT backing files, NOT kernel flocks,
  #   and with NO pid/boot-id record. The ONLY filesystem flock in the model is
  #   the per-user state mutation lease <user-state>/run/lease (plan 05 §12).

# (b) per-user authoritative root <user-state> — owned by <uid>, mode 0700 (D-17)
<user-state>/
  manifest.json               # what the user wants (selectors + pins) [I1]  (a.k.a. desired state)
  lock.json                   # exact realized identity per selector [I1]
  generations/
    gen-0001.json             # immutable generation METADATA record, content-addressed [I4]
    gen-0001.manifest.json    # immutable candidate MANIFEST snapshot for gen-0001 (§5.6)
    gen-0001.lock.json        # immutable candidate LOCK snapshot for gen-0001 (§5.6)
    gen-0002.json
    gen-0002.manifest.json
    gen-0002.lock.json
    ...                       # (each *.json has a *.sha256 sidecar; the activation forest lives under activations/, NOT here)
  activations/                # Rust-materialized activation symlink forests (ZERO Nix) [doc 04 §5.5]
    gen-<id>.staging/         #   transient staging forest (materialized, then renamed)
    gen-<id>/                 #   retained forest: merge dirs + leaf symlinks → /nix/store targets
  current                     # RELATIVE symlink -> activations/gen-<id> [I6]
  run/
    lease                     # flock + lease record (pid, nonce, opId, ts) [I3]
    pid
  journal/
    journal.ndjson            # append-only operation journal [I7]
  logs/
    product.log               # sanitized product events (the user-facing log surface)
    <opId>.ndjson             # sanitized, schema-versioned per-op product event stream (plan 06 §5.3) — the
                              #   ONLY log that crosses into <user-state>; COPIED from the broker-private raw
                              #   log (/var/lib/pkg/log/broker/<internal-id>, §4) with raw store paths/derivers/
                              #   cache URLs stripped. NO raw <opId>.nix.log lives in per-user state (§6/§10).
  cache/
    narinfo/                  # optional local narInfo cache (best-effort)

# user prefs ONLY (no trust/substituter keys; INV-03)
$XDG_CONFIG_HOME/pkg/config.toml
```

The **daemon-visible** GC roots live where Nix scans them. On a standard
multi-user install Nix scans `/nix/var/nix/gcroots` (*confirmed* [^gc-roots]).
The product therefore creates, **per user and per retained generation**, a root
**set** directory `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/` containing
**one symlink `<safe-id>` → store path per selected output** (each output root
protects its closure; §8.3). These are created by the authenticated root-helper
(the gcroots tree is root-owned; D-17/ARCH-INV-06); Nix treats any symlink in a
scanned gcroots directory as a root, so **no** `nix-store --add-root` operation
is used (see [^add-root]).

## 5. Schemas (versioned; current `schemaVersion = 1`)

> **Naming authority:** the *canonical shapes* of manifest/lock/generation live in
> **doc 01 §10**; this document owns **migrations, integrity, and the storage contract**
> (doc 00 §7 contract table). Field names are **camelCase** everywhere (matching the
> channel descriptor in doc 02 §7).

**Persisted-file integrity.** The manifest/lock/generation/snapshot JSON
objects each begin with `schemaVersion` and carry a sidecar `<file>.sha256`
containing the sha256 of the file body (excluding the sidecar); writers
compute the sidecar after fsync, readers verify and refuse on mismatch (plan
04 §verify applies to internal state too). The **append-only journal is an
explicit exception**: it has no single sidecar — instead every NDJSON row is
self-validating and hash-chained (§5.4). All of these bindings — sidecars,
`generationHash`/`manifestHash`/`lockHash`, and the journal chain — are
**corruption- and crash-detection** mechanisms, **not** same-uid
authentication: an attacker who can write a uid's `<user-state>` already owns
that uid's state, and cross-uid isolation is enforced by ownership/permissions
(plan 07/08), not by these hashes.

### 5.1 `manifest.json` — user intent (a.k.a. desired state)

```jsonc
{
  "schemaVersion": 1,
  "channelSeq": 42,
  "uid": 1001,                          // OS uid this manifest belongs to (D-17)
  "entries": [
    {
      "id": "sel_018f",
      "selector": "ripgrep",            // user intent (D-13)
      "attribute": "ripgrep",           // resolved Nixpkgs attribute (may equal selector)
      "versionPref": { "kind": "any" }, // any | exact | min | range
      "outputs": null,                  // null => meta.outputsToInstall
      "sourceRev": "channel:current",   // channel:current | channel:pinned:<id> | rev:<gitsha>
      "pinned": false,
      "pinnedTo": null,                 // realized.storePath or null
      "addedAt": "2024-08-02T23:14:00Z",
      "origin": "user:install"
    }
  ],
  "pins": [ "sel_018f" ]                // convenience index of pinned selectors
}
```

- `entries[].id` is **stable** for the lifetime of that selector in this
  file; `remove` deletes the entry; `upgrade` may rewrite `sourceRev` but
  keeps `id` (so the lock entry maps cleanly).
- `pinnedTo` non-null ⇒ the selector is pinned (set by `pin`); Resolve
  targets that exact `storePath` (plan 04 §5.1 rule 3).

### 5.2 `lock.json` — realized identity

Keyed by entry `id`. **One selector maps to one or more realized outputs.**
Identity = `storePath`, never `pname@version`.

```jsonc
{
  "schemaVersion": 1,
  "channelSeq": 42,                          // from descriptor (doc 02 §7)
  "system": "x86_64-linux",
  "uid": 1001,                               // OS uid this lock belongs to (D-17)
  "entries": {
    "sel_018f": {
      "attribute": "ripgrep",
      "nixpkgsRev": "abcd1234...deadbeef",          // may differ per entry (§7)
      "realized": {
        "storePath": "/nix/store/a1b2...-ripgrep-14.1.0",
        "deriver":  "/nix/store/9z8y...-ripgrep-14.1.0.drv",
        "outputs": { "out": "...", "man": "..." },
        "outputsToInstall": ["out","man"],
        "system": "x86_64-linux",
        "narHash": "sha256-...",
        "closureNarSize": 4821034,
        "pname": "ripgrep", "version": "14.1.0"
      },
      "lockedAt": "2024-08-02T23:14:05Z",
      "provenance": "cache:cache.nixos.org",         // or "local-build" (Linux/macOS native sandboxed)
      "sigsObserved": ["cache.nixos.org-1:..."]
    }
  }
}
```

**Mixed Nixpkgs revisions are normal.** `upgrade <one>` re-resolves only that
selector against `channelSeq` and updates its `nixpkgsRev`; every other
entry keeps its locked rev. A generation therefore frequently contains outputs
from several Nixpkgs commits — this is sound because every store path is
content-addressed and its closure is self-contained.

### 5.3 Generation manifest — `generations/gen-<id>.json`

Immutable snapshot of an activation. `<id>` is **reserved monotonically under
the per-user operation lease (§12) before staging** (`gen-%04d`), because the
staging forest path (`activations/gen-<id>.staging`), the immutable record
(`generations/gen-<id>.json`), the retained forest (`activations/gen-<id>/`),
the per-output root-set dir (`gcroots/pkg/users/<uid>/gen-<id>/`), and the
journal's `generationId` all need the id — so it is known by the **stage**
phase, **not** assigned at commit. Aborted or crashed operations may leave
gaps, and **ids are never reused** (the next op simply takes the next id). The
file is content-addressed: its `generationHash` is the sha256 of its canonical
JSON (excluding the `generationHash` field) and is recorded in the journal.

```jsonc
{
  "schemaVersion": 1,
  "uid": 1001,                               // OS uid this generation belongs to (D-17)
  "id": "gen-0042",
  "parent": "gen-0041",                       // previous generation (for history)
  "createdAt": "2024-08-02T23:14:06Z",
  "channelSeq": 42,
  "manifestHash": "sha256-...",              // sha256 of the candidate manifest BODY
                                         //   (== generations/gen-0042.manifest.json body == post-step-6 manifest.json body)
  "lockHash": "sha256-...",                   // sha256 of the candidate lock BODY
                                         //   (== generations/gen-0042.lock.json body == post-step-6 lock.json body)
  "manifestSnapshot": "generations/gen-0042.manifest.json", // RELATIVE to <user-state>; immutable candidate manifest snapshot (§5.6)
  "lockSnapshot":    "generations/gen-0042.lock.json",      // RELATIVE to <user-state>; immutable candidate lock snapshot (§5.6)
  "activation": {
    "kind": "pkg-symlink-forest",          // D-18: Rust-owned forest OUTSIDE /nix/store; ZERO Nix at activation
    "treePath": "activations/gen-0042",    // RELATIVE to <user-state>; the retained forest dir (§8)
    "treeDigest": "sha256-...",            // SHA-256 over RFC 8785 (JCS) canonical JSON of the SORTED
                                         //   records {relativePath,storeTarget,sourceSelector,output}
    "entryCount": 1287,                    // # leaf records covered by treeDigest
    "collisionPolicy": "abort",            // ONLY abort | keep-first | keep-last (D-18; NO keep-all/--force)
    "outputRoots": [                       // SORTED output store paths that got a per-output GC root (§8.3)
      "/nix/store/a1b2...-ripgrep-14.1.0",
      "/nix/store/c3d4...-fd-9.0.0"
    ],
    "collisionResolutions": [              // collision RESOLUTIONS for keep-first/keep-last (empty under abort).
                                         //   ONLY colliding relativePaths are logged — NO full path map is
                                         //   stored; treeDigest is the sole binding for the whole forest
      { "relativePath": "bin/rg",
        "winner": { "sourceSelector": "sel_018f", "output": "out" },
        "losers":   [ { "sourceSelector": "sel_0310", "output": "out" } ] }
    ]
  },
  "outputs": [
    { "id": "sel_018f", "attribute": "ripgrep",
      "nixpkgsRev": "abcd1234...deadbeef",
      "storePath": "/nix/store/a1b2...-ripgrep-14.1.0",
      "deriver":  "/nix/store/9z8y...-ripgrep-14.1.0.drv",
      "outputsToInstall": ["out","man"],
      "narHash": "sha256-...",
      "closureNarSize": 4821034,
      "provenance": "cache:cache.nixos.org",
      "pinned": false }
    /* ...one entry per installed selector... */
  ],
  "operation": { "opId": "op_2024-...-a1", "kind": "install",
                 "approval": { "build": "not_required" } },
  "generationHash": "sha256-..."             // content hash of THIS file (I4)
}
```

A generation is **reproducible and Nix-free at activation**: the forest is a
pure function of the verified, rooted outputs, so `pkg` (Rust) can
re-materialize `activations/gen-<id>/` at any time and re-verify it via
`activation.treeDigest` with **zero Nix commands** (D-18). Given
`outputs[].storePath`/`narHash` and `nixpkgsRev`, the product can also
re-verify (`nix store verify`) or re-acquire any missing path independently —
the basis for `repair` (§10) and for surviving a Nix store wipe. The
`treeDigest`/`outputRoots`/`outputs[]` triple is the complete, durable record
of the activation; **no huge path→target map is stored**.

### 5.4 `journal.ndjson` — append-only operation log

**Journal integrity (explicit exception to the sidecar rule, §5).** The
journal is the one append-only, actively-chained file; it has **no** single
sidecar. Every NDJSON row carries four common chain fields in addition to the
op/phase/status fields shown below:

- `schemaVersion` — journal-row schema (currently `1`);
- `seq` — a **strictly monotonic** per-file row sequence (`1, 2, 3, …`);
- `prevRowHash` — the `rowHash` of the prior **accepted** row (the genesis row
  carries a fixed sentinel, e.g. `sha256-Genesis`);
- `rowHash` — the SHA-256 of the RFC 8785 (JCS) canonical JSON of **that row
  with the `rowHash` field removed**, rendered as `sha256-<lowercase-hex>`.

Rows are chained: each accepted row's `prevRowHash` equals the previous
accepted row's `rowHash`. **Recovery** scans from the head and accepts the
**longest prefix** whose rows are all: newline-terminated, JSON-valid,
schema-valid, **sequence-contiguous** (`seq` increments by exactly 1), and
**hash-valid** (each `rowHash` recomputes from the canonical row body and each
`prevRowHash` matches the prior accepted `rowHash`). Only a **torn final
suffix** — a partial trailing line, a bad-JSON tail, a `seq` gap, or a hash
break at the very end — is discarded or quarantined; any corruption, reorder,
or deletion **inside** the accepted history is detected and **fails closed**
(refuse mutating ops; require `repair`). `prevStateHash`/`nextStateHash` are
sha256 of the post-commit `manifest.json`+`lock.json` bodies and let recovery
detect partial current-view writes; like all state hashes here, they are
corruption/crash-detection bindings, **not** same-uid authentication (§5/§14).
The common chain fields are **omitted from the illustrative rows below for
readability**; the first row shows them representatively.

```jsonc
{ "opId":"op_...","schemaVersion":1,"seq":1,
  "prevRowHash":"sha256-Genesis","rowHash":"sha256-...",
  "ts":"...","kind":"install",
  "phase":"resolve","status":"started","prevStateHash":"sha256-...",
  "channelSeq":42 }                  // every row also carries schemaVersion/seq/prevRowHash/rowHash (omitted below)
{ "opId":"op_...","seq":2,"phase":"preflight","status":"ok",
  "reportHash":"sha256-..." }
{ "opId":"op_...","seq":3,"phase":"acquire","status":"ok" }
{ "opId":"op_...","seq":4,"phase":"stage","status":"ok",               // Rust symlink forest materialized (ZERO Nix)
  "stagingPath":"activations/gen-0042.staging","treeDigest":"sha256-...",
  "generationId":"gen-0042" }
{ "opId":"op_...","seq":5,"phase":"commit","status":"prepared",        // candidate snapshots + gen-0042.json fsynced (prepared = BOTH durable)
  "generationId":"gen-0042","generationHash":"sha256-...",
  "manifestSnapshot":"generations/gen-0042.manifest.json","manifestHash":"sha256-...",
  "lockSnapshot":"generations/gen-0042.lock.json","lockHash":"sha256-..." }
{ "opId":"op_...","seq":6,"phase":"commit","status":"rooted",          // per-output root SET created + dir fsynced
  "generationId":"gen-0042",
  "gcrootSet":"/nix/var/nix/gcroots/pkg/users/1001/gen-0042" }        // one <safe-id> -> output per selected output
{ "opId":"op_...","seq":7,"phase":"activate","status":"activated",     // atomic relative current swap (forest at gen-0042)
  "generationId":"gen-0042" }
{ "opId":"op_...","seq":8,"phase":"commit","status":"committed",       // manifest/lock written + committed row
  "nextStateHash":"sha256-...","generationId":"gen-0042" }
```

The `prepared`→`rooted`→`activated`→`committed` statuses are the four recovery
distinguishable transaction states (§8.4); recovery scans the tail to identify
the op's state and the filesystem to confirm which durable side-effects landed.

**Build-required op — preflight + approval rows (Linux/macOS, native).** When a
preflight reports `build_required`, the journal carries the canonical
`BuildPlan` digest and the policy version. A **granted** approval row records
`source` ∈ `interactive` | `yes` (`interactive` = the user answered yes at the
prompt; `yes` = invoked with `--yes`, which pre-approves the **single** displayed
`BuildPlan`). A **decline/refusal** row records a stable `reason` + `resultCode`
and **never** carries a `source`/`approvalSource`. An approval is **one operation
only** and never persists across operations; a decline/refusal leaves the desired
state and the active generation unchanged:

```jsonc
{ "opId":"op_b","seq":1,"ts":"...","kind":"install",
  "phase":"resolve","status":"started","prevStateHash":"sha256-...",
  "channelSeq":42 }
{ "opId":"op_b","seq":2,"phase":"preflight","status":"ok",
  "reportHash":"sha256-...",
  "buildPlanDigest":"sha256:1f2e...","policyVersion":7,
  "approvalRequired":true }                       // preview emitted (plan 04 §5.2.1 / §7)
{ "opId":"op_b","seq":3,"phase":"approval","status":"granted",
  "buildPlanDigest":"sha256:1f2e...","policyVersion":7,
  "source":"interactive" }                         // interactive: user said yes at the prompt
// With --yes the SAME granted row is written, but source is "yes":
// { "opId":"op_b","seq":3,"phase":"approval","status":"granted",
//   "buildPlanDigest":"sha256:1f2e...","policyVersion":7,
//   "source":"yes" }                              // --yes pre-approves the single displayed BuildPlan
{ "opId":"op_b","seq":4,"phase":"acquire","status":"ok",
  "buildPlanDigest":"sha256:1f2e...","policyVersion":7 }  // re-checked: digest matches the approved plan
/* …stage/activate/commit as above… */

// Refusals — NO approval row is ever written as "granted", and a refusal row
// carries a stable `reason` + `resultCode` and NEVER a `source`/`approvalSource`.
// A refusal changes no desired state and no generation.
// (a) interactive decline — user said no at the prompt:
{ "opId":"op_c","seq":3,"phase":"approval","status":"declined",
  "buildPlanDigest":"sha256:1f2e...","policyVersion":7,
  "reason":"interactive_declined","resultCode":68 }  // gen N unchanged
// (b) non-TTY, build required, WITHOUT --yes — cannot prompt; safe refusal:
{ "opId":"op_d","seq":3,"phase":"approval","status":"refused",
  "buildPlanDigest":"sha256:1f2e...","policyVersion":7,
  "reason":"no_tty_without_yes","resultCode":68 }    // distinct from --yes (which grants, source:"yes")
// A hard policy refusal (ACQUIRE_NO_BINARY, 67) is recorded at preflight/acquire
// (status:"failed", reason:"acquire_no_binary", resultCode:67) and is never
// overridden by approval or --yes.
```

`buildPlanDigest` is the digest of the canonical `BuildPlan`
(plan 04 §5.2.1, RFC 8785 canonical JSON, `sha256:<lowercase hex>`); it is the
stable approval subject. Immediately before a local build, under the
machine-global build admission permit (broker-internal, plan 04 §5.3.1), `pkg` re-derives the exact
derivation/readiness `BuildPlan` and compares the digest, then re-measures
disk/free-space/load **outside** the digest; on digest mismatch it re-prompts
(interactive) or exits `ACQUIRE_NEEDS_APPROVAL` (non-interactive), and on a
dynamic check failure it exits `PREFLIGHT_FAIL`. No approval persists beyond the
one operation that recorded it, and a granted approval's `source` is always
`interactive` or `yes`.

`prevStateHash`/`nextStateHash` are sha256 of the *post-commit*
`manifest.json`+`lock.json` bodies, letting recovery detect partial
writes.

### 5.5 Lease record — `run/lease`

```jsonc
{ "opId":"op_...","pid":12345,"nonce":"...","started":"...",
  "host":"...","tty":"..." }
```

Held by an exclusive `flock(LOCK_EX)` on `run/lease` for the duration of a
mutating op (or exclusively by `gc` while it prunes that user's generations,
§9). The full read model — leased `LOCK_SH` reads of the mutable views vs.
lock-free reads of the immutable generation snapshots, plus the **recovery
barrier** (no command observes state until startup recovery reconciles it) —
is in §12; the three machine-global/per-user coordination mechanisms are
compared at the top of §12. There is deliberately **no** racy "`LOCK_SH` or
none" middle ground: a reader either leases the mutable views or reads the
immutable snapshot (both yield a consistent view; never a half-written one).

### 5.6 Generation-scoped view snapshots — `generations/gen-<id>.manifest.json` / `gen-<id>.lock.json`

Each generation durably owns **two small immutable candidate-view snapshots**
alongside its metadata record:

- `generations/gen-<id>.manifest.json` — the exact manifest body this
  generation was built from (the *post-commit* `manifest.json` body for that
  generation);
- `generations/gen-<id>.lock.json` — the exact lock body for that generation.

These are **generation-scoped, independent files** (one pair per generation),
**not** a shared-content / by-hash store — deliberately, to avoid
reference-counting/GC complexity across generations. Each carries the standard
`schemaVersion` + `.sha256` sidecar (§5). The generation record references them
by relative path (`manifestSnapshot`/`lockSnapshot`) and records their body
hashes (`manifestHash`/`lockHash`). They are written and fsynced at the
**prepared** step (§8.4), **before** `gen-<id>.json` — so the record only ever
points at already-durable snapshots. They are the **sole source** from which
`activated`/`committed` recovery restores the mutable `manifest.json`/
`lock.json` current views (§8.4), the lock-free read source for read-only
commands (§12), and the per-generation point-in-time "backup" (§11.3).
Cleanup is uniform: discarding a generation (prepared/rooted recovery discard,
pruning, or `history --delete`) deletes exactly that generation's record,
forest, root-set dir, and its **two snapshot files** + sidecars — touching no
other generation's snapshots.

## 6. Identity: selector vs realized (canonical)

- **Selector** = intent (attribute + version pref + outputs + source rev +
  optional pin). Lives in `manifest.json` as **desired state only**.
- **Realized identity** = exact artifact (`storePath` + deriver + outputs +
  narHash + system + nixpkgsRev). It is **authoritative only in `lock.json`
  and the immutable generation record** (`generations/gen-<id>.json`). The
  manifest carries the desired selector and **at most an explicit pin
  intent/reference** (`pinnedTo` = a single `storePath` when the selector is
  pinned) — **never general realization**: a normal manifest entry has no
  `storePath`/deriver/narHash.
- `pname@version` is **display metadata**. The product will render it, search
  by it (plan 03), and use it for `versionPref` checks — but it is **never**
  a unique key. Two attributes can share `pname`; one attribute can ship the
  same `pname@version` from different commits with different hashes.
- `storePath` is unique and content-addressed; it is the join key across
  **lock ↔ immutable generation ↔ per-output GC root ↔ Nix store** — **not**
  across every manifest entry. The manifest's desired selectors join to
  realization only via the lock/generation, or via the single `pinnedTo`
  reference on a pinned entry.
- **Internal durable state, not public CLI JSON.** `manifest.json`/`lock.json`,
  `generations/*`, `journal/*`, and `run/*` are **internal durable state**:
  machine-owned recovery/operating data, **not** a public CLI output format.
  Raw store identities (`storePath`, deriver, narHash, drv paths, raw Nix
  system triples) remain **internal** and **must not be rendered** by plan 06,
  which exposes only sanitized product events, friendly platform names, and
  generation/operation ids. **Raw adapter/Nix output may contain store paths,
  derivers, and cache URLs and must not live in per-user state**: it is kept in
  the broker-private service directory
  (`/var/lib/pkg/log/broker/<internal-id>`, parent `0700`, files `0600`, §4);
  only sanitized, schema-versioned NDJSON is copied to
  `<user-state>/logs/<opId>.ndjson`.

## 7. Selective upgrades with mixed revisions

```mermaid
sequenceDiagram
    autonumber
    participant U as user
    participant P as product
    participant L as lock.json
    participant N as Nixpkgs (pinned)
    Note over L: a@revR1, b@revR1 (after initial install)
    U->>P: update (channel R1 -> R2; plan 02)
    U->>P: upgrade b
    P->>N: resolve b @ channelSeq(R2)
    P->>L: entries[b].nixpkgsRev = R2; realized = new storePath
    Note over L: a@revR1, b@revR2  (mixed, legal)
    U->>P: upgrade --all
    P->>N: resolve a,b @ R2
    P->>L: entries[*].nixpkgsRev = R2
```

Rules:

1. `update` changes `channelSeq` only; it touches no lock entry.
2. `upgrade <sel>` re-resolves exactly that selector at `channelSeq`.
3. `upgrade --all` re-resolves every non-pinned selector at `channelSeq`.
4. Pinned selectors are never touched by `upgrade`/`upgrade --all` unless
   `--bump-pinned` is given (off by default; flagged in plan 12).
5. `outdated` (plan 06) compares each lock entry's realized `storePath`
   against a fresh resolution at `channelSeq` and reports per-selector.

## 8. Generations, `current`, activation

### 8.1 Generation lifecycle

```mermaid
stateDiagram-v2
    [*] --> Staging: stage materializes symlink forest (ZERO Nix; D-18)
    Staging --> Active: rooted + rename + atomic current swap
    Active --> Retired: superseded by newer gen
    Retired --> [*]: pruned by `gc`/`history --delete`
    Active --> [*]: never (until superseded)
```

- Creating a generation: pipeline commit (plan 04 §5.7); the activation is a
  **Rust symlink forest outside `/nix/store`**, not a Nix store object (D-18).
- `current` is a **relative** symlink (`current -> activations/gen-<id>`) to the
  active generation's retained forest, and is valid **only** while that forest
  exists and its `treeDigest` recomputes (§11).
- `history` lists generations with timestamps and operation kind.
- `rollback` (no arg) repoints `current` to the parent of the active generation;
  `rollback <id>` repoints to that generation. Because the activation is a
  deterministic forest whose identity is bound by the `treePath` + gen-id +
  per-output root-set topology (§8.3/§8.4), rollback **always creates a fresh
  monotonic generation record** (`generations/gen-<new-id>.json`), a **fresh
  deterministic retained forest** (`activations/gen-<new-id>/`, re-materialized
  from the target's verified rooted outputs with a freshly computed
  `treeDigest`), and a **fresh per-output root-set directory**
  (`gcroots/pkg/users/<uid>/gen-<new-id>/`) — then swaps `current` to it. It
  does **not** reuse/relink the target's old forest or root set: that
  optimization conflicts with the `treePath`/gen-id/root-set topology and would
  break the per-generation isolation that pruning (§9) and GC rely on (a pruned
  generation must take exactly its own forest + record + root set with it).
  History stays linear and monotonic (mirroring Nix *generation*-based rollback,
  *confirmed* [^profile-rollback], but in Rust-owned state with **no Nix
  profile**). Rollback uses the same generation transaction (§8.4): the fresh
  per-output root set is created + fsynced **before** `current` is repointed.

### 8.2 Atomic `current` swap (the **activated** step)

This swap is the linearization point that makes the new generation live. It is
performed **only after** the generation record is fsynced (**prepared**, §8.4),
the per-output root set is created + fsynced (**rooted**, §8.3/§8.4), and the
staging forest has been renamed to its retained path `activations/gen-<id>/`
(§8.4) — so the swap always lands on a durably-rooted, fully-documented,
`treeDigest`-verified forest. The link target is a **relative** path
(`activations/gen-<id>`), so `<user-state>` is relocatable. POSIX recipe (no
unlink gap):

1. `symlink("activations/gen-<id>", "<user-state>/current.tmp.<nonce>")`
2. `fsync` the directory `<user-state>`
3. `rename("<user-state>/current.tmp.<nonce>", "<user-state>/current")`
4. `fsync` the directory again.

`rename(2)` over an existing symlink is atomic on Linux/macOS, so `current` is
never missing mid-swap — but a crash during the swap may land on **either**
side of the rename, and recovery must **classify by what actually happened**,
not assume the previous target. After cleaning any `current.tmp.*` detritus,
recovery reads the **actual relative `current` target** and cross-checks it
against the ground truths: does the target's `generations/gen-<id>.json` exist
and verify? does its forest recompute `treeDigest`? is its per-output root set
present? If `current` still resolves to the **old** generation, this is a
**pre-swap** crash → recovery discards the new (unreachable) staged forest,
record, snapshots, and root set, and the old generation stays active (§8.4
**prepared**/**rooted**). If `current` resolves to the **new** generation, this
is a **post-swap (activated)** crash → recovery restores `manifest`/`lock`
from the generation's durable snapshots (§5.6) and appends the `committed`
row (§8.4 **activated**). Recovery never blindly asserts
"previous `current` intact"; it follows the actual link target plus the
record/`treeDigest`/root-set ground truths.

### 8.3 GC roots — one per-output root set per generation (D-18/INV-05/INV-11)

- **One root per selected output, not one per generation.** For each retained
  generation the product creates a root-set directory
  `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/` containing **one symlink
  `<safe-id>` → absolute, validated output store path per selected output** —
  exactly the sorted `activation.outputRoots[]` recorded in `gen-<id>.json`.
  Each root independently pins its output's closure, so the set as a whole
  protects every installed output. There is deliberately **no single
  root-per-generation**: D-18 rejects the buildEnv single-store-path model, so
  no single root ever has to stand in for a hand-built tree.
- **`<safe-id>` definition.** Each symlink name in a root set is a **validated
  deterministic non-user path component** derived from the canonical selected
  output `StorePath` (e.g. `out-` + the first 128 bits of
  `sha256(storePath)`). This makes the complete name set reconstructible from
  the persisted, sorted `activation.outputRoots[]` during crash recovery and
  remains collision-resistant; two distinct selected outputs never alias.
  It is computed by the product and is **never a caller-supplied raw path
  component** — no user selector text, attribute, or version string is used
  verbatim as a symlink name — and the helper rejects any name not in the
  computed set or carrying `/`, `..`, absolute, or otherwise escaping bytes.
  This makes the root-set directory a controlled, traversable namespace rather
  than an injection surface.
- **Atomic root-set publication (root-helper / private broker).** The gcroots
  tree is root-owned (D-17/ARCH-INV-06); the authenticated helper/broker is the
  only writer and **never** uses `nix-store --add-root` (see [^add-root]). To
  publish a generation's root set it: (1) creates a staged dir
  `gen-<id>.tmp.<nonce>/` under `gcroots/pkg/users/<uid>/`; (2) **validates
  and populates every `<safe-id>` symlink** → absolute output store path from
  `activation.outputRoots[]`, rejecting any out-of-set/escaping name; (3)
  `fsync`s the staged dir tree; (4) `rename(gen-<id>.tmp.<nonce> → gen-<id>)`
  so the whole set appears atomically; (5) `fsync`s the per-`<uid>` parent. A
  crash before the rename leaves only `gen-<id>.tmp.*` detritus (no `gen-<id>/`
  exists, so Nix never scans a half-populated set); after the rename the full
  set is durable. Nix treats any symlink in a scanned gcroots directory as a
  root.
- The root set is created at the **rooted** step of the generation transaction
  (§8.4), **before** the `current` swap, so a crash after the swap can never
  leave `current` resolving to a forest with an unrooted output. Because each
  root targets a `/nix/store` output path (not the forest), rooting is
  independent of whether the forest has been renamed to `gen-<id>/` yet.
- **Empty generations.** Removing the final selector produces an empty,
  retained activation forest with `outputRoots: []`. The rooted landmark is
  then vacuously satisfied without calling the helper: its closed grammar
  deliberately rejects empty root-publication requests. The record, empty
  forest, atomic `current` switch, snapshots, and journal rows are otherwise
  identical to a nonempty generation, and recovery remains idempotent.
- Pruning a generation (§9) removes its entire root-set directory; the next
  `nix store gc` reclaims any output closures no longer rooted by any retained
  generation.

### 8.4 Generation transaction: ordering, crash invariant, recovery states

This section is the **canonical crash-consistency contract** for activating a
generation (referenced by plan 01 §12.2, plan 04 §5.5–§5.7, plan 09 §6.5). The
whole transaction runs under the per-user operation lease (§12), which
serializes this user's own mutating ops and this user's `gc`/pruning.
**Cross-user GC safety does not rest on the per-user lease** — a per-user lease
cannot block another user's global `nix store gc`: it rests on the
**broker-internal machine-global GC admission gate** (§8.5), whose shared
**GC-inhibit permit** (held by this op's handle from before it dispatches the
substitute/build/realization that can create unrooted outputs, until its
per-output root set is durable or the op aborts) holds this op's
realized-but-not-yet-rooted outputs safe from any user's broker GC. The V1
sole-manager assumption (Q5.1) means the product's broker is the only GC
mediator; the installer schedules no automatic GC (§8.5).

**Crash invariant (two layers).** Distinguish *immediate post-crash safety*
from the *fully-reconciled product state* reached only after startup recovery:

- **Immediate post-crash safety** (the property that holds at every instant
  observable after any crash or forced kill, *before any recovery runs*):
  `current` is either absent (pre-first-install) or is a **relative** symlink
  to an existing retained forest `activations/gen-<id>/` whose recomputed
  `treeDigest` equals `gen-<id>.activation.treeDigest`, where (a) **every**
  output in `gen-<id>.activation.outputRoots` is protected by a durable
  per-output GC root, (b) the immutable record `generations/gen-<id>.json`
  exists, is fsynced, and records the forest (`kind/treePath/treeDigest/
  entryCount/collisionPolicy/outputRoots`) with its per-output `outputs[]` and
  the `manifestSnapshot`/`lockSnapshot` references, and (c) the durable
  candidate-view snapshots `generations/gen-<id>.manifest.json` /
  `.lock.json` exist and hash to `manifestHash`/`lockHash`. **The mutable
  `manifest.json`/`lock.json` current views are NOT required to be consistent
  with `gen-<id>` at this instant** — after an *activated* crash they
  legitimately *lag* `current` (still reflecting the previous generation).
  This lag is safe (the new forest is rooted + documented +
  `treeDigest`-verified) but not yet reconciled (I8).
- **Reconciled product state** (reached only after startup recovery, §11, under
  the recovery barrier of §12): recovery restores the current `manifest.json`/
  `lock.json` views by copying the durable snapshots for the generation
  `current` actually resolves to, and appends the `committed` row. Only then
  do commands observe a fully-consistent state.

Equivalently: **`current` is never observed pointing at an unrooted,
undocumented, or `treeDigest`-mismatched forest — recovery never publishes
`current` before the roots, and no command observes state before recovery
restores the current views from the snapshots.** A crash *before* the
`current` swap leaves the previous generation active and the new
forest/root-set/record/snapshots unreachable from `current` (deletable); a
crash *after* the swap always leaves a fully-rooted, fully-documented,
`treeDigest`-verified active generation whose current views lag at most until
recovery restores them from the snapshots and appends the `committed` marker.

**Transaction ordering** (phase names from plan 04 are illustrative; the
ordering and the four recovery states are normative). Each filesystem step is
followed by the fsyncs needed to make it durable. **Activation stages a
deterministic symlink forest and invokes ZERO Nix commands (D-18)** — Nix has
already realized/substituted the selected outputs before this transaction:

1. **stage** — Rust materializes the symlink forest at
   `activations/gen-<id>.staging/`: walk each selected output **without
   following symlinks**, create merge dirs + leaf symlinks to absolute,
   validated `/nix/store` targets, **reject** any path-escape (`.`/`..`/absolute
   /traversal) and any file-vs-directory conflict (hard `STAGE_***`, not a
   collision-policy case), apply `collisionPolicy` (only `abort`/`keep-first`/
   `keep-last`; `keep-first`/`keep-last` pick the per-file winner and **retain
   every non-conflicting file**), then compute `treeDigest` + `entryCount` and
   verify they recompute; `fsync` the dir tree as it is built. `current`
   unchanged. **No Nix.** The outputs are not yet rooted; this op's handle
   already holds its **shared GC-inhibit permit** (§8.5) — taken on the handle
   in acquire (plan 04) **before** the broker dispatched the
   substitute/build/realization that can create unrooted outputs — which
   protects them from any user's broker `gc` until the root set is durable
   (rooted) or this op aborts.
2. **prepared** — first durably write the **generation-scoped immutable
   candidate snapshots** `generations/gen-<id>.manifest.json` and
   `generations/gen-<id>.lock.json` (the exact byte bodies the post-commit
   `manifest.json`/`lock.json` must equal), each with its `.sha256` sidecar;
   `fsync` each file. Then write the immutable record
   `generations/gen-<id>.json` with the D-18 activation record
   (`kind:"pkg-symlink-forest"`, relative `treePath:"activations/gen-<id>"`,
   `treeDigest`, `entryCount`, `collisionPolicy`, sorted `outputRoots[]`,
   `collisionResolutions[]`) plus per-output `outputs[]`, `manifestHash`/
   `lockHash` (the snapshot body hashes), the relative `manifestSnapshot`/
   `lockSnapshot` paths (§5.6), and `generationHash`; `fsync` the file;
   `fsync` the `generations/` directory (so the snapshots are durable *before*
   the record that references them — I2). `current` unchanged. **`prepared`
   means BOTH the two candidate-view snapshots AND `gen-<id>.json` are
   durable.** Journal: `phase=commit,status=prepared` (carries the snapshot
   paths + body hashes).
3. **rooted** — the authenticated root-helper **atomically publishes** the
   per-output root set `gcroots/pkg/users/<uid>/gen-<id>/` (one `<safe-id>`
   symlink → each output store path in `outputRoots[]`) via the staged-tmp +
   rename protocol of §8.3 (create `gen-<id>.tmp.<nonce>`, validate/populate
   every `<safe-id>`, `fsync` the staged dir, `rename` → `gen-<id>`, `fsync` the
   `<uid>` parent); **no** `nix-store --add-root` is used. Every selected
   output's closure is now durably protected by its per-output root; this op's
   handle **releases its shared GC-inhibit permit** (§8.5) once this step's
   rename + parent-fsync are durable. `current` unchanged. Journal:
   `phase=commit,status=rooted`.
4. **rename** (idempotent, "if not yet") — `rename(activations/gen-<id>.staging
   → activations/gen-<id>)`; `fsync(<user-state>/activations/)`. The forest is
   now visible at its retained path. This is a **distinct normative step**
   after **rooted** and before **activated** (the ordering is
   `stage .staging → prepared → rooted → rename to retained → activated`); it
   is **never folded into stage**. Rooting targets `/nix/store` outputs (not
   the forest), so the rename is safe after **rooted**. `current` unchanged.
5. **activated** — atomic `current` swap → the relative target
   `activations/gen-<id>` (§8.2): `symlink(current.tmp.<nonce>,
   "activations/gen-<id>")` → `fsync(<user-state>/)` → `rename(..., current)`
   → `fsync(<user-state>/)`. `current` now resolves to a rooted, documented,
   `treeDigest`-verified forest. Journal: `phase=activate,status=activated`.
6. write `manifest.json` and `lock.json` (temp → `fsync` file → `rename` →
   `fsync(<user-state>/)` dir) as byte-identical copies of the durable
   candidate snapshots (`generations/gen-<id>.manifest.json` / `.lock.json`);
   assert each hashes to `manifestHash`/`lockHash` recorded in `gen-<id>.json`
   (else hand off to recovery). The snapshots are the source of truth for
   these views.
7. **committed** — append `phase=commit,status=committed` (with
   `nextStateHash`) to `journal/journal.ndjson`; `fsync` the journal file and
   the `journal/` directory.

> The `current` swap (step 5) is the linearization point. Steps 1–4 only
> prepare durable, `current`-invisible state (including the candidate-view
> snapshots at **prepared**); steps 6–7 only finalize bookkeeping once the
> activation is already correct. The per-output root set precedes the swap so
> the swap can never land on a forest with an unrooted output — this is
> strictly safer than root-after-swap, which would rely on a per-user lease to
> bridge an activate→root gap; but **a per-user lease cannot block another
> user's global `nix store gc`** (§8.5), so root-after-swap would break
> `current` if any collector ever ran in that window. No verified Nix semantic forbids this
> ordering: Nix keeps a realized path alive purely via gcroots independent of
> any profile, so rooting each output before swapping `current` is consistent
> with Nix semantics (and uses **no** Nix profile — I1/D-18).

**How `manifest`/`lock` (current views) relate to immutable generation copies.**
`generations/gen-<id>.json` is the immutable, content-addressed, authoritative
snapshot of a generation (its realized `outputs[]`, the activation forest record
`treeDigest`/`outputRoots`, and the `manifestHash`/`lockHash` of the views it
was built from; the matching manifest/lock bodies are snapshotted as the
explicit, generation-scoped files `generations/gen-<id>.manifest.json` /
`.lock.json`, referenced by `manifestSnapshot`/`lockSnapshot` — §5.6).
`manifest.json`/`lock.json` are the mutable *current views* — rewritten each
commit (step 6) to match the now-active generation. They are a **projection**
of the active generation, not independent state: recovery always makes them
consistent with the generation that `current` points to, by copying the
durable snapshots for that generation (§5.6). They are written *after* the
swap (not before) so that a pre-swap crash never leaves the views ahead of
`current`; the only inconsistency a crash can expose is the views lagging
`current` (I8), which is forward-recoverable because `current` is already
rooted and documented and the snapshots are durable from **prepared** onward.

**Directory-fsync discipline.** POSIX does not make a directory entry durable
until its parent directory is `fsync`'d after the create/rename/unlink/symlink.
The transaction therefore `fsync`s: each staging forest dir as it is built; each
new file's fd before renaming it; **each candidate-view snapshot file
(`generations/gen-<id>.manifest.json`/`.lock.json`) and its `.sha256` sidecar
*before* `gen-<id>.json` is written (so the record only references durable
snapshots)**; the `generations/` dir after writing `gen-<id>.json`; the per-uid gcroots dir **tree** after creating every root in
the set (inside the root-helper, before it returns); the
`<user-state>/activations/` dir after the staging→`gen-<id>` forest rename; the
`<user-state>/` dir after the `current` symlink and again after the `rename`;
and the `journal/` dir after appending the committed row. File-data durability
without the directory fsync is insufficient — a crash can lose the name binding
while the bytes persist.

**Recovery states** — detected by scanning the journal tail and the filesystem
(is `gen-<id>.json` present? is the per-output root set present? does `current`
→ `activations/gen-<id>` and does `treeDigest` recompute? is there a
`committed` row for this op?). The staging→`gen-<id>` forest rename (step 4) is
**not** a distinct recovery state: it is subsumed under **rooted** (the forest
may sit at `.staging` or at `gen-<id>/`; recovery deletes whichever exists):

| State | `gen-<id>.json` | root set | `current` | committed row | Recovery action |
|-------|:--:|:--:|:--:|:--:|---|
| **prepared** | ✓ | ✗ | old forest | ✗ | delete `gen-<id>.json` **and its two candidate-view snapshots** `gen-<id>.manifest.json`/`gen-<id>.lock.json` (+ `.sha256` sidecars) **and** the staging forest (`.staging` or `gen-<id>/`) if present; previous generation stays active |
| **rooted** | ✓ | ✓ | old forest | ✗ | remove the per-output root set (`gen-<id>/`, plus any `gen-<id>.tmp.*` detritus from a mid-publication crash); delete `gen-<id>.json` **and its two snapshots** (+ sidecars); delete the forest (`.staging` or `gen-<id>/`); previous generation stays active (outputs become unrooted, reclaimed by a later `gc`) |
| **activated** | ✓ | ✓ | → `gen-<id>` forest | ✗ | new generation is already rooted + documented + `treeDigest`-verified; **restore the current `manifest`/`lock` views by copying the durable snapshots** `gen-<id>.manifest.json`/`.lock.json` (verifying they hash to `manifestHash`/`lockHash`), then append the `committed` row (idempotent forward recovery) |
| **committed** | ✓ | ✓ | → `gen-<id>` forest | ✓ | no-op; optionally re-restore `manifest`/`lock` from the snapshots if stale |

In the **prepared** and **rooted** states the staged forest is unreachable from
`current`, so deleting its record, snapshots, root set, and forest is safe and
loses no live activation (the forest is rebuildable from the record + rooted
outputs). In the **activated**/**committed** states `current` already resolves
to a rooted, documented, `treeDigest`-verified forest, so recovery only
restores the current views from the snapshots and appends the marker. (In every
row, `gen-<id>.json` ✓ implies its two snapshots are also present: **prepared**
= snapshots + record both durable, §8.4 step 2.) There is no state in which
`current` points at an unrooted, undocumented, or `treeDigest`-mismatched
forest, and no command observes the lagging mutable views before recovery
restores them (I3/I8).

### 8.5 Machine-global GC admission gate and the GC-inhibit permit

`nix store gc` is **global to the store** ([^nix-gc]): a collector started for
one user reclaims any unrooted path in `/nix/store`, including outputs another
user's in-flight op has just realized but not yet rooted. A **per-user lease
(§12) cannot block that** — it serializes only one user's own ops and that
user's own `gc`. Cross-user GC safety therefore rests on a **machine-global**
mechanism inside the enforced singleton broker, not on any per-user lease and
not on any on-disk lock file.

**The broker is the sole GC mediator; the installer schedules no automatic GC.**
All product `nix store gc` runs go through `pkg`'s **private broker**
(ARCH-INV-05 / I7-plan-07), the daemon's sole client. The V1 installer installs
and schedules **no** automatic GC timer/service/launchd job and, on uninstall,
removes or disables **only its own** scheduler artifacts; all product GC is
**explicit and broker-mediated** (there is deliberately **no** `nix.conf`
automatic-GC key being set or relied upon — Nix automatic GC is simply never
scheduled by the product). This makes the two rules below enforceable.

**The gate is broker-internal; there is no on-disk GC lock.** The GC admission
gate is a **fair counted read/write admission structure** living **inside the
enforced singleton broker process** (plan 07 §7.4). It is **not** a `flock` on
a backing file and **not** a `run/gc-admission` pid/boot-id record: a single
long-lived broker cannot represent many independent logical shared holders via
`flock` portably (macOS file locks are file/process-oriented, not per-handle),
so the gate is an in-memory counted structure keyed by **opaque operation
handle**, with fairness so neither the shared holders nor the exclusive writer
can starve. Ordinary users **never** touch the gate: they hand the broker an
opaque operation handle and request `gc` over the UID-authenticated broker
socket (plan 07 §7.4).

**Shared GC-inhibit permit (one per in-flight op handle).** Each mutating op's
opaque handle owns **exactly one shared GC-inhibit permit**. It is acquired
**before** the broker dispatches any substitute/build/realization step that can
create unrooted store outputs (the acquire/realize boundary, plan 04 §5.3) —
**not** after realization returns — and is **held until the op's final
per-output root publication is durable** (the §8.4 **rooted** step's `rename`
+ parent-`fsync`) **or the op aborts**. Many handles (across users) may hold
shared permits at once; while any is held, the realized-but-not-yet-rooted
outputs behind it are uncollectable by a broker GC. (After **rooted** the
per-output root set itself protects every output's closure, so the permit is
released.)

**Exclusive GC permit (one at a time).** Every broker `nix store gc` obtains
the **exclusive permit** and **waits for all active shared GC-inhibit permits
to drain** before running the collector — fairly: once an exclusive waiter is
queued, no **new** shared permit is granted, so the drain is bounded by the
in-flight realize→root windows, not by fresh arrivals. Only then does GC
proceed; on completion or failure the exclusive permit is released and any
queued shared permits are granted.

**Lifecycle, disconnect, crash, and recovery.**

- **Per-op release.** A handle releases its shared permit on every exit path:
the **rooted** durability point, an abort/decline/refusal, a build/preflight
failure, or a cancellation.
- **CLI disconnect / cancel** does **not** leak a permit: the broker **owns**
the operation handle, so a disconnect/cancel triggers broker-owned
cancellation + cleanup (stop any spawned realize/build, discard staging) and
then permit release; the user process going away never strands a shared
permit.
- **Broker crash** fails **all** in-flight handles at once — their
realize/build subprocesses are children the broker supervises, so the crash
tears down the whole set — and no orphaned shared permit survives; a
replacement broker starts with an empty gate.
- **Replacement-broker recovery barrier.** A fresh broker completes its
**startup recovery barrier** (§11/§12 — no command observes state until
recovery reconciles it) and classifies every journal tail **before it admits
the first `gc`**: any in-flight op whose permit never drained is recovered by
transaction state (§8.4), so GC is never admitted against an unreconciled
realize→root window.
- **Supervision / child-containment detail is a blocking cross-doc item.** The
exact process model by which the singleton broker supervises and contains its
realize/build/gc children (cgroups / launchd / ptrace-equivalent, OOM policy,
signal handling) is owned by the **detailed-broker design** (plan 07 §7.4
follow-on); this document depends on, but does not specify, it.

**Three distinct mechanisms — do not conflate.**

1. **Per-user state lease** (§12) — a real advisory `flock` on per-user
   `run/lease` (held in the user's own process, separate processes) that
   serializes one user's mutating ops and that user's own `gc`/pruning. This is
   the **only** one of the three that is a filesystem `flock`.
2. **Broker-internal machine-global local-build admission permit** (plan 04
   §5.3.1) — a fair in-process mutex/queue living **inside the enforced
   singleton broker** that serializes approved native local builds across all
   users. It is **not** a backing file, **not** an in-kernel `flock`, and has
   **no** pid/boot-id record; the permit is owned by the broker-minted opaque
   operation handle, released on **every** exit **after child containment**, and
   a replacement broker starts with an empty gate and completes its startup
   recovery barrier (§11/§12) before admitting the first build. (Per-client
   `max-jobs` discussion lives in plan 04, not here; this section is specifically
   **global GC safety**.)
3. **Broker-internal machine-global GC admission gate** (this section) — a fair
   in-process counted R/W admission structure that makes the global collector
   safe across users during the realize→root window. (Both #2 and #3 are
   in-broker, in-memory structures keyed by opaque operation handle — **neither**
   is a backing-file `flock` or a pid/boot-id record, because a single
   long-lived broker cannot represent many independent logical holders via
   `flock` portably, especially on macOS where locks are file/process-oriented
   rather than per-handle. They are nonetheless **distinct mechanisms**: #2 is a
   single-holder exclusive build mutex; #3 is a counted R/W gate with many shared
   GC-inhibit holders plus one exclusive GC writer.)

**A root set before `current` remains required (I5).** The gate does **not**
replace root-before-swap: it covers the realize→root window for outputs not yet
behind `current`, and root-before-swap guarantees a crash never lands
`current` on an unrooted forest. Together: a broker GC can never collect a
path the active generation (or an in-flight op about to activate one) depends
on. **Disclosed residual:** the gate can only restrain collectors the broker
starts; an *external* `nix store gc` run by root outside `pkg` is not gated
(§13) — v1 assumes sole-management (Q5.1).

## 9. `gc` semantics

`gc` runs the Nix collector **scoped to the product's roots only is not
possible** — `nix store gc` is global to the store [^nix-gc]. Implications:

- The product's `gc` invokes `nix store gc` **through the broker** under the
  broker-internal GC admission gate (§8.5). The broker is the sole GC mediator
  (ARCH-INV-05 / Q5.1) and the installer schedules no automatic GC (§8.5), so
  there are no other managers' roots in v1; **per-user** roots under
  `/nix/var/nix/gcroots/pkg/users/<uid>/` are all the product roots (D-17).
- **Generations must be pruned before GC** for their paths to become
  collectable. `gc` therefore:
  1. Determines the protected set = active generation + generations within the
     retention window (`gc.keep_generations`, default 10) and
     `gc.max_age_days` (default 30). **The active generation is never
     eligible** (I5/I6); only **retired, non-current** generations may be
     pruned.
  2. For each eligible generation, prune it using the **crash-safe ordering**
     of §9.1 (journal a prune intent, delete the user-owned forest + record +
     candidate snapshots first while its per-output root set still keeps the
     outputs alive, remove the privileged root-set directory last, then append
     `pruned`).
  3. Runs `nix store gc` once (under the broker's exclusive GC permit, §8.5).
- `gc --dry-run` prints what would be pruned and the estimated reclaimed bytes
  without removing anything.
- `gc` acquires the **per-user lease** (§12) exclusively while pruning that
  user's generations (a same-user concurrent install is refused with
  `STATE_LOCKED`, exit 72) **and** obtains the broker's **exclusive GC permit**
  (§8.5) for `nix store gc`: it waits for any other user's realize→root
  GC-inhibit permit to drain before running the collector.
- Nix's own `nix-collect-garbage -d` also removes *profile* generations — but
  the product does **not** use Nix profiles as state (I1), so this is a no-op
  for us; we never call it. Cite: profile-based GC is Nix-specific and not
  used here [^profile-gc].

### 9.1 Crash-safe pruning and `history --delete`

Both `gc` (retention pruning) and `history --delete <id>` retire generations,
and both run **exactly the same crash-safe transaction** for each id. The
`history --delete <id>` verb is simply the manual, non-retention entry point to
the code path that `gc` runs for each retention-eligible generation in
ascending id order.

**Eligibility (re-checked under the per-user lease).** Only a **retired,
non-current** generation may be pruned — the active generation (the one
`current` resolves to) is **never** eligible. Eligibility is checked at the
start and again immediately before the privileged root-set removal (step 4):
if `current` moved onto the target meanwhile (e.g. a concurrent `rollback`),
the prune is abandoned (`phase=prune,status=aborted`) and **nothing** is
removed.

**Root-last ordering (why).** The user-owned metadata (forest + record +
candidate snapshots) is deleted **while the per-output root set still keeps the
outputs alive**, and the privileged root-set directory is removed **last**. A
crash therefore never leaves a generation record/forest pointing at outputs a
collector may have reclaimed: at every crash point the state is either (a)
intact metadata + still-live outputs (roots not yet removed) or (b) no metadata
at all (already deleted) — never a dangling record over collected paths.

**Transaction steps** (each filesystem step followed by the fsyncs needed to
make it durable):

1. **Journal a prune intent.** Append `phase=prune,status=intended` carrying
   the **exact** generation id and the planned `outputRoots[]` (copied from
   `gen-<id>.json`); `fsync` the journal file and the `journal/` dir. Recovery
   (§11) resumes from this row.
2. **Delete the user-owned forest, while the root set still keeps outputs
   alive.** `rm -rf activations/gen-<id>/` (or `activations/gen-<id>.staging/`
   if that is what exists); `fsync(<user-state>/activations/)`. The selected
   outputs stay live in `/nix/store` because their per-output roots still
   exist, so this delete alone cannot make any active generation's closure
   unreachable.
3. **Delete the user-owned record + candidate snapshots.** Remove
   `generations/gen-<id>.json`, `generations/gen-<id>.manifest.json`,
   `generations/gen-<id>.lock.json`, and their `.sha256` sidecars;
   `fsync(generations/)`. Touches no other generation's snapshots (§5.6/§11.3).
4. **Remove the privileged per-output root-set directory LAST.** The
   authenticated root-helper/broker deletes
   `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/`; `fsync` the `<uid>`
   parent. (Re-check eligibility here — see above.) Only now do the output
   closures become eligible for a future `nix store gc`.
5. **Append `pruned`.** `phase=prune,status=pruned` for the id; `fsync` the
   journal file and the `journal/` dir. Idempotent: a duplicate `pruned` is a
   no-op.

Representative prune journal rows (common chain fields per §5.4, omitted for
readability):

```jsonc
{ "opId":"op_...","seq":N,"kind":"gc","phase":"prune","status":"intended",
  "generationId":"gen-0007","outputRoots":["/nix/store/...","/nix/store/..."] }
// … crash here ⇒ recovery resumes: finish user-owned deletes, remove root set, append pruned …
{ "opId":"op_...","seq":N+1,"kind":"gc","phase":"prune","status":"pruned",
  "generationId":"gen-0007" }
```

**Crash semantics.**

- **Crash before step 4 (root removal)** — the user-owned forest/record/
  snapshots are already gone but the privileged root set still exists, so the
  outputs are still alive. This can **leak roots/space** (reclaimed by a later
  `gc` once recovery finishes the prune) but **cannot lose a live activation**:
  `current` never pointed at a pruned generation (only retired generations are
  pruned), and the still-present roots keep every active generation's closure
  reachable. On restart, recovery **resumes the exact prune intent before any
  command runs** (the recovery barrier, §12): it re-reads the journal, finishes
  steps 3–4–5 for each `intended` id that is not yet `pruned` (idempotent
  re-delete of already-gone files), and only then admits commands.
- **Crash after step 4 (root removal)** — the user metadata is already gone and
  the roots are gone; recovery sees the `intended` row, confirms the root set
  is absent, and **finalizes the journal** by appending `pruned`. No live
  activation is affected.
- **Active generation is never eligible** — if `current` resolves to the prune
  target at step 1 or step 4, the prune is abandoned (`status=aborted`) with
  nothing removed.

The "backup" role of the candidate snapshots (§11.3) ends at `pruned`: once a
generation is pruned, its snapshots are gone and only a channel re-resolution
can reconstruct it.

## 10. `repair` semantics

`repair` brings the active generation (or a named one) back to a verified,
complete state. **V1 ships the full privileged mutating repair**: a broker
read-only verification (**Phase 0**) that is **not** one of the two mutating
repair phases, followed by the **two mutating phases** — **Phase A**
(cache-only helper repair) and **Phase B** (the approved rebuild fallback, split
into **B.1** preview/approval and **B.2** execution). This structure enforces
two verified Nix 2.34.8 facts: (a) `nix store verify` and `nix store repair`
are **separate** modern commands — `verify` is **read-only**, `repair`
**mutates** (there is **no** modern `nix store verify --repair`; [^store-verify]
[^store-repair]); and (b) `Store::repairPath` **first tries a Repair-mode
substitution**, and only if that fails **and** the output has a valid deriver
does it **rebuild ALL outputs of that deriver** (`bmRepair`) ([^store-repair]).
Because repair mutates and the Nix daemon **rejects repair for untrusted
clients** (the broker is an unprivileged `allowed-user`, never a `trusted-user`;
plan 01 ARCH-INV-01/05; verified Nix 2.34.8), **every repair mutation runs as
the one fixed maintenance operation of the root helper** — the broker never
mutates the store itself. Read-only verify stays **broker-mediated** (an
`allowed-user` may verify). The phases are:

- **Phase 0 — broker read-only verify (mutates nothing; not a repair phase).**
  §10.1. Computes the **damage set**.
- **Phase A — helper cache-only repair (the first mutating phase).** §10.3.
  Per-path `nix store repair` with `max-jobs=0`/`builders` empty; **auto on a
  signed cache hit** and **must stop before any build** on a cache miss.
- **Phase B — approved rebuild fallback (the second mutating phase).** Reached
  only when Phase A leaves an unrepaired path that has a valid deriver. **B.1**
  (§10.4) is the ordinary build preview + single-operation approval, which
  **must stop before any build**; **B.2** (§10.5) is the approved rebuild
  **execution, only if useful** (a valid deriver exists **and** approval was
  granted), serialized and held under the shared GC-inhibit permit.

**Targets: the full computed closure, never merely `activation.outputRoots`.**
The damage set and the repair capability bind to the **exact typed corrupt or
missing registered-or-expected store paths within the FULL computed closure
reachable from the target generation's selected output roots** — i.e. every
path `nix store verify --recursive` can reach, including **missing-on-disk but
registered/expected closure targets** — **not** merely the generation's
top-level `activation.outputRoots[]`. (The per-output GC roots of §8.3 — **one root
per selected output** — are a **rooting topology** that keeps each selected
output's closure alive; they are **not** the repair authorization set. Repair is
authorized only against the typed corrupt/missing closure targets, drawn from
broker-held generation state, never from public/user input — plan 01
ARCH-INV-01/§12.4.)

**Mark affected closure unknown/unhealthy; block dependent mutations.** The
moment Phase 0 computes a **nonempty** damage set, the product marks the
affected generation's closure **unknown/unhealthy** — the Phase-0 verify row's
nonempty `damageSet` is itself the unhealthy signal (§10.7) — and **blocks
dependent state mutations** — a new `install`/`upgrade`/`rollback` that would
build on or switch onto a closure it cannot trust is refused until the closure
is verified healthy again. Each target path is journaled through
`intended`→`in_progress`→`post_verify` (§10.7). Health is restored only by a
**final read-only Phase-0 `nix store verify`** that confirms **every** target
clean (§10.7), which is what governs success; if repair cannot complete, the
closure stays unhealthy and dependent mutations stay blocked, and the op exits
non-zero. This health state is part of the journal and is honored across the
recovery barrier (§11.1/§12).

**Store repair is not atomic (verified Nix 2.34.8).** Both mutating paths can
leave a registered store path transiently — or, after a crash/power loss,
durably — absent: **cache repair** (`LocalStore::addToStore` in `Repair` mode,
used by `Store::repairPath` on a cache hit) **deletes the live real path before
restoring and re-validating its NAR**, so for the deletion→restore window the
path is **absent or only partially restored on disk while the store DB
validity record may still say the path is valid** — a naive "is it registered
valid?" check is wrong here, which is exactly why success is governed by a fresh
read-only `nix store verify`, not by a validity lookup; and a **local repair
build** (`replaceValidPath`, used when `Store::repairPath` rebuilds via
`bmRepair`) **moves the original aside before moving the replacement into
place**, with a best-effort rollback a crash can interrupt. §10.9 makes this a
normative limitation/residual and fixes the consequences every phase below
assumes: `pkg repair` is explicitly user-initiated and **warns before mutation
that affected commands may be temporarily unavailable**; each target path is
journaled through `intended`→`in_progress`→`post_verify`; repair is **never**
claimed atomic; the shared GC-inhibit permit (§8.5) spans the whole repair so
root safety cannot change concurrently; crash recovery re-verifies each target,
**auto-retries only Phase A** after a fresh read-only verify, and **requires
fresh preview/approval/capability before repeating any Phase B rebuild** (a
stale capability stays invalid after restart). And **store repair does not
touch the generation/forest/root-set/`current`** (§10.6): a repaired path keeps
its content-addressed store path, so the existing generation/root intent is
unchanged. **Store repair never creates or swaps a generation**; recovery of a
damaged activation forest is a **separate, Rust-only, zero-Nix** path (§10.6).

### 10.1 Phase 0 — broker read-only verify (mutates nothing; not a repair phase)

The broker runs `nix store verify --recursive` (**no** `--repair`) over the
**full computed closure reachable from the target generation's selected output
roots** [^store-verify]. It first re-verifies the active forest's `treeDigest`
against `activation.treeDigest` (a forest mismatch is the separate Rust-only
recovery path of §10.6, not a store-path repair). The outcome is the **damage
set** — the sorted set of **corrupt or missing registered-or-expected store
paths within that full closure**, including any **missing-on-disk but
registered/expected closure targets** (a path the closure reaches that is no
longer on disk but is still registered/expected). This phase is safe to repeat
any time, mutates nothing, and **never marks anything repaired**. A **nonempty**
damage set **marks the affected generation's closure unknown/unhealthy** and
**blocks dependent state mutations** until a clean final read-only verify
restores health (§10 intro/§10.7). (Trust/JSON-mode specifics for the pinned
runtime are pinned for the chosen managed Nix runtime and validated by the
Fake↔Real parity job — doc 09 §4.3; doc 01 §11 / doc 00 §11 SPK-02; for
status-less `verify`, exit status **plus** an independently validated
postcondition is checked, never human-output scraping — doc 04 I2 / doc 09
§4.1.)

### 10.2 Repair capability (helper-issued, opaque, single-use)

Every repair mutation is delegated to the root helper as a **single fixed
operation against a broker-chosen, validated StorePath set drawn from
broker-held generation state** (plan 01 ARCH-INV-01/05) — specifically the
**exact typed corrupt/missing registered-or-expected targets within the FULL
computed closure reachable from the target generation's selected output roots
(§10.1), never merely `activation.outputRoots`**: those per-output roots are a
**rooting topology** (one GC root per selected output, §8.3), not the repair
authorization set. The helper accepts **no public/raw StorePath, installable,
derivation, expression, flake ref, argv, option, substituter/key, environment
override, output selection, or arbitrary verb**, and returns only **sanitized
per-path outcome**.

The helper **issues an opaque, expiring, single-use capability** bound
**server-side** to: (1) caller uid; (2) an **existing pkg-owned, rooted**
generation; (3) the **exact typed corrupt/missing registered-or-expected
StorePath set within the FULL computed closure reachable from that
generation's selected output roots — not merely its top-level
`activation.outputRoots[]`, and including any missing-on-disk but
registered/expected closure targets** (the closure is computed by Phase 0's
`verify --recursive`; §10.1); (4) a **`RepairBuildPlan` digest** (§10.4); (5)
`policyVersion`; (6) `mode` ∈ {`cache-only`, `build`}. The capability is held in
helper/broker server-side state; the public CLI→broker channel and the
broker→helper execute channel carry **only opaque identifiers/digests**
(generation id, op id, capability id, plan digest). The CLI **never** sees a raw
StorePath. A presented capability that is stale, expired, replayed (already
used), or whose uid/generation/set/digest/policyVersion/mode does not re-match
the request **fails closed** (helper refuses; logged SECURITY); capabilities
are **invalidated on helper/broker restart**.

> **Next broker milestone — not specified here.** The exact helper RPC/framing
> (capability token format, transport, wire fields, server-side store) is the
> same pending detailed-broker/wire design flagged in plan 01/07 ("Detailed
> wire/capability design is the next milestone"). This section fixes only the
> **invariants** above.

### 10.3 Phase A — helper cache-only repair (auto on a cache hit; the first mutating phase)

For each path in the damage set, the helper runs the **mutative**
`nix store repair` [^store-repair] **one path at a time**, with **fixed,
non-user-extensible settings**: `max-jobs = 0`, `builders` **empty** (no remote
builders), the managed pinned substituters/keys (from the root-owned `nix.conf`,
never per-call flags), recursive, against exactly the one validated path named
by the capability. **Never accept a public raw StorePath or argv.**

**Why this is cache-only.** `max-jobs = 0` disables local builds but still
permits substitution; an **empty `builders`** setting is additionally required
to prevent remote builds [^max-jobs-builders]. So `Store::repairPath` can only
succeed via a **Repair-mode substitution (cache hit)** — it cannot rebuild. A
cache hit repairs the path **automatically, with no user approval**. A **cache
miss** (no substitutable repair) is detected as repair-not-possible for that
path; the helper returns a sanitized per-path outcome and **does not proceed to
any build**. This is the mandatory stop point.

Per-path execution makes the phase **idempotent and resumable**: a path already
repaired is a no-op on re-run, and a crash mid-phase loses no work (§10.8). The
repair op's handle holds a **shared GC-inhibit permit** (§8.5) across Phase A —
spanning the non-atomic delete-then-restore of each target path (§10.9) — so a
broker `gc` cannot change root safety or observe a transiently-absent target.
Normal cache repair leaves the generation, activation forest, per-output root
set, and `current` unchanged (§10.6).

### 10.4 Phase B.1 — approved rebuild: preview + approval (must stop before any build)

Phase B is the approved rebuild fallback and runs **only** when Phase A leaves a
path unrepaired (cache miss) **and** that path has a valid deriver. **B.1** is
the preview/approval gate: because an unconstrained `Store::repairPath` *would*
rebuild in that case, `pkg` **must not let it** — it **stops before any local or
remote build** and instead produces the **same ordinary public build preview and
explicit single-operation approval flow used elsewhere** (plan 04 §5.2.1/§7).

Because `Store::repairPath` with a build slot rebuilds **ALL outputs of the
deriver** (`bmRepair`), the internal **`RepairBuildPlan` and its digest MUST
cover every output the repair may rebuild — not only the damaged output.** It
enumerates every output of every deriver of every still-damaged path with the
same derivation/readiness/system/sandbox fields as a normal `BuildPlan`
(plan 04 §5.2.1.a); its RFC 8785-canonical-JSON digest is the approval subject
(mirroring `buildPlanDigest`).

Approval is **single-operation** (plan 04 §7): `--yes` pre-approves the one
displayed `RepairBuildPlan`; interactive = yes-at-prompt; non-TTY without
`--yes` is refused. A granted approval binds `RepairBuildPlan digest +
policyVersion` and **never** persists beyond this op. On approval, a new
capability with `mode = build` is issued; the public request still carries only
opaque ids/digest.

### 10.5 Phase B.2 — approved rebuild: execution (helper, serialized; only if useful)

**B.2** is the approved rebuild **execution** and runs **only if useful** — i.e.
Phase A left an unrepaired path with a valid deriver **and** B.1 approval was
granted. An approved repair build is **serialized by the broker's machine-global
local-build admission permit** (plan 04 §5.3.1), **holds the shared GC-inhibit
permit across the non-atomic local repair build** (§8.5/§10.9 — the original is
moved aside before the replacement lands, so the permit keeps root safety from
changing while the registered path is transiently absent), uses **no remote
builders** (`builders` empty), and is executed by the root helper with a
**small nonzero
`max-jobs`** (so `Store::repairPath` may rebuild via `bmRepair`; same fixed
managed substituters/keys). Immediately before execution, `pkg` **re-derives the
`RepairBuildPlan` and compares its digest** to the approved one (fail closed on
mismatch — interactive re-prompts; non-interactive exits `ACQUIRE_NEEDS_APPROVAL`,
68), re-measures disk/free-space/load **outside** the digest, and **re-validates
the capability** (uid/generation/set/digest/policyVersion/mode). There are
**never** remote builders for repair (or any v1 build): `builders` is empty in
the managed config; `max-jobs` only bounds local build slots [^max-jobs-builders].

### 10.6 Store repair leaves the generation/forest/roots/`current` unchanged; activation-forest damage is separate

**Normal store repair is not a generation operation.** Store paths are
content-addressed: a cache repair (Phase A) and a deriver rebuild (Phase B.2)
restore content **at the same registered store path** — they do **not** mint a
new path, do **not** change which outputs are selected, and so do **not**
create a new generation, rebuild the Rust activation forest, publish a new
per-output root set, or swap `current`. The existing generation's
`outputs[]`/`activation.outputRoots[]`, the per-output root-set directory
`gcroots/.../gen-<id>/`, the activation symlink forest `activations/gen-<id>/`,
and `current` are all **unchanged** by normal store repair. The shared
**GC-inhibit permit** (§8.5) spans the repair precisely so that root safety
cannot be concurrently changed by a broker `gc` while a target path is
transiently absent (§10.9); the forest itself never invokes Nix (D-18). The
repair phases therefore stop at "every target store path verifies clean"
(§10.7): they stage no generation, touch no forest, re-root nothing, and swap
no `current`.

**Activation-forest / `current` metadata damage is a separate, Rust-only
recovery path with ZERO Nix.** If the forest or `current` metadata is itself
damaged or missing (e.g. the user deleted `activations/gen-<id>/`, `current` is
a broken link, or a generation record's `treeDigest` no longer recomputes),
that is **not** store repair: it is the §11.1/§11.2 startup-recovery flow —
re-materialize the forest from the generation's durable, verified, rooted
outputs, recompute `treeDigest`, and (if `current` is broken) repoint it to the
most recent intact generation. It is driven entirely by the immutable
generation snapshots (§5.6), substitutes/builds nothing, and is described
**distinctly** from the store-repair phases. `pkg repair` may *detect* such
forest damage during its Phase-0 `treeDigest` check and then hand off to this
recovery path, but the hand-off never conflates the two: store repair fixes
`/nix/store` content at unchanged paths; forest recovery rebuilds Rust state
from unchanged store paths.

### 10.7 Completion — never "repaired" until read-only verify

A repair op is **never marked `repaired` in state until a fresh Phase-0
read-only `nix store verify` confirms every target path verifies clean** — and
that same clean verify is what **clears the affected closure's
unknown/unhealthy marker** (§10 intro/§10.1) and unblocks dependent state
mutations. The damage set, the unhealthy/healthy markers, the issued
capability, **each target path's
`intended`→`in_progress`→`post_verify` status**, the per-path sanitized
outcome, and the final verifying read-only verify are all journaled (§5.4);
the `repaired` marker follows the clean verify. Representative journal rows
(chain fields per §5.4 omitted):

```jsonc
{ "opId":"op_r","seq":1,"kind":"repair","phase":"verify","status":"ok",
  "generationId":"gen-0042","damageSet":["/nix/store/..."] }   // Phase 0 read-only verify
{ "opId":"op_r","seq":2,"phase":"capability","status":"issued","mode":"cache-only",
  "capabilityId":"cap_...","repairPlanDigest":"sha256:...","policyVersion":7 }
// per-path cache-only repair rows (one intended→in_progress→post_verify triple per path):
{ "opId":"op_r","seq":3,"phase":"repair","status":"intended","path":"/nix/store/...","mode":"cache-only" }
{ "opId":"op_r","seq":4,"phase":"repair","status":"in_progress","path":"/nix/store/..." }   // non-atomic delete→restore underway (§10.9)
{ "opId":"op_r","seq":5,"phase":"repair","status":"post_verify","path":"/nix/store/...","result":"ok" }
// cache miss on a damaged path ⇒ jump to Phase B (B.1 approval → B.2 build) for that path (build-mode rows then):
{ "opId":"op_r","seq":6,"phase":"approval","status":"granted",          // ONLY on deriver fallback
  "repairPlanDigest":"sha256:...","policyVersion":7,"source":"interactive" }
{ "opId":"op_r","seq":7,"phase":"repair","status":"intended","path":"/nix/store/...","mode":"build" }
{ "opId":"op_r","seq":8,"phase":"repair","status":"in_progress","path":"/nix/store/..." }   // non-atomic replaceValidPath underway (§10.9)
{ "opId":"op_r","seq":9,"phase":"repair","status":"post_verify","path":"/nix/store/...","result":"ok" }
{ "opId":"op_r","seq":10,"phase":"verify","status":"ok" }                // final read-only verify gates …
{ "opId":"op_r","seq":11,"phase":"repair","status":"repaired","generationId":"gen-0042" }
```

`repair` never deletes user state; worst case it reports paths it cannot
re-acquire (e.g., removed upstream) and exits non-zero.

### 10.8 Crash / restart recovery for repair

The recovery barrier (§12) applies: no command observes state until repair
recovery reconciles it, and an **unhealthy** closure marker (§10 intro) stays
in force — so dependent state mutations stay blocked — across the whole resume.
Recovery is shaped by the **non-atomic** store-repair limitation (§10.9): a
crash can leave a registered path missing, so recovery never trusts pre-crash
progress — it **re-verifies each target** and splits the resume by phase.

- **Phase A (cache-only), partially completed.** The per-path journal rows
  (§10.7/§10.9) show which targets were `intended`/`in_progress`/`post_verify`;
  an `in_progress` row with no `post_verify` is exactly a path that may have
  been mid-mutation (deleted-but-not-restored, or moved-aside-but-not-replaced)
  at crash time. On restart, recovery re-runs Phase 0 (read-only verify) to
  recompute the **actual** damage set, then **automatically retries only
  Phase A per-path** (after that fresh verification) for still-damaged,
  still-cache-repairable paths (Phase A is approval-free, so it may
  auto-resume). Paths that need a build are **not** auto-built.
- **Phase B.2 (approved rebuild), partially completed.** The approved
  `mode = build` capability is **single-use and invalidated on helper/broker
  restart by design**, so a partially-completed repair build **does not silently
  resume**; recovery re-verifies each target, marks the op as **needing
  re-approval**, and surfaces the partial state (a registered path may be
  missing because `replaceValidPath` was interrupted — §10.9). The user must
  obtain a **fresh preview, explicit single-operation approval, and a fresh
  capability** before any Phase B rebuild is repeated; a fresh
  `RepairBuildPlan` digest + capability are then issued.
- **In every case**, no path is marked `repaired` and the closure is not marked
  healthy until its own Phase-0 read-only `post_verify` succeeds; a torn repair
  tail is reconciled forward exactly like any other unfinished op
  (§8.4/§11.2), never by silently mutating state.

### 10.9 Non-atomic store repair — verified Nix 2.34.8 limitation (residual)

**Store repair is not atomic.** Verified against Nix 2.34.8, both repair
mutation paths can leave a registered store path transiently — and, after a
crash or power loss, durably — absent [^store-repair]:

- **Cache repair** (`LocalStore::addToStore` in `Repair` mode, used by
  `Store::repairPath` on a cache hit) **deletes the live real path *before*
  substituting/restoring and re-validating its NAR.** For the
  deletion→restore window the registered path is **absent or only partially
  restored on disk while the store DB validity record may still say the path is
  valid** — even though the generation's per-output GC-root symlink and the
  activation forest still reference it. This is exactly why success is governed
  by a fresh read-only `nix store verify`, not by a validity lookup (§10.7).
- **Local repair build** (`replaceValidPath`, used when `Store::repairPath`
  rebuilds via `bmRepair`) **moves the original aside before moving the
  replacement into place**; a caught exception attempts to roll back, but a
  crash or power loss between the aside-move and the completion of the
  replacement/rollback **can leave a registered path missing**.

This is a **normative limitation and disclosed residual**, not a property `pkg`
can make atomic. The consequences every phase assumes:

- **User-initiated, warned mutation.** `pkg repair` is explicitly
  user-initiated (never automatic, never scheduled). Before any Phase A/Phase
  B.2 mutation it **warns that commands whose `PATH` resolves through the
  affected activation forest may be temporarily unavailable** while a target
  path is absent (cache repair) or moved aside (local build), and it **marks
  the affected closure unknown/unhealthy and blocks dependent state mutations**
  (§10 intro) for the duration.
- **GC-inhibit permit spans the whole repair.** The op's handle holds its shared
  **GC-inhibit permit** (§8.5) from before the first Phase A/Phase B.2 mutation
  until the final Phase-0 re-verify (§10.7), so a broker `gc` cannot change
  root safety — and cannot observe+collect a transiently-absent target — while
  the repair is in flight. The existing per-output root set is **never
  rewritten** by normal store repair (§10.6).
- **Per-path journaling.** Each target path is journaled through an explicit
  `intended` → `in_progress` → `post_verify` status (one row each, carrying the
  path, `mode`, and the verifying outcome; §5.4/§10.7). Recovery uses these
  rows to know exactly which paths were mid-mutation.
- **Never claim atomicity.** No `pkg` documentation, journal status, or message
  asserts that store repair is atomic; the durable contract is only
  "verified-clean-or-reported" (§10.7).
- **Crash recovery re-verifies and splits the resume.** After a crash/restart,
  recovery (under the §12 barrier) re-runs Phase-0 read-only `nix store verify`
  for every target to recompute the true damage set, then (a) **automatically
  retries only Phase A** for still-damaged, still-cache-repairable paths
  (approval-free, after that fresh verification); (b) **requires a fresh
  preview, explicit single-operation approval, and a fresh capability before
  repeating any Phase B rebuild** — the prior `mode = build` capability is stale
  and stays invalid after restart (§10.2); (c) marks no path `repaired` and the
  closure not healthy until its own `post_verify` read-only verify succeeds.
- **Stale helper capability stays invalid after restart.** Capabilities are
  invalidated on helper/broker restart by design (§10.2); a crash leaves any
  issued capability unusable, which is exactly what forces the fresh
  preview/approval in (b).

A path that cannot be re-verified (e.g. removed upstream, no substitutable
repair, and no approved build) is reported and the op exits non-zero; `pkg`
never fabricates a "repaired" state. This residual is sole-manager scoped: an
*external* `nix store gc`/repair run by root outside `pkg` is out of scope
(Q5.1).

## 11. Corruption detection & recovery

### 11.1 Detection

- On every startup and before every mutating op: verify all sidecar checksums;
  verify `current` is a **relative** symlink to an existing retained forest
  `activations/gen-<id>/` whose recomputed `treeDigest` equals
  `gen-<id>.activation.treeDigest`, and that every entry in
  `gen-<id>.activation.outputRoots` has a live per-output GC root; verify the
  active generation's `generationHash` recomputes; verify its candidate-view
  snapshots `generations/gen-<id>.manifest.json`/`.lock.json` exist and hash
  to `manifestHash`/`lockHash`; **verify the journal chain** (§5.4: every row
  seq-contiguous and hash-valid, accepting the longest valid prefix and
  quarantining only a torn suffix); verify the journal tail's `nextStateHash`
  matches current `manifest`+`lock` (if a `committed` row is the tail); and
  **resume any unfinished `prune intended` row** (§9.1) before admitting
  commands.
- If `current` resolves to a missing/damaged forest (e.g., user deleted it), its
  `treeDigest` no longer recomputes, or any selected output is unrooted: the
  product refuses mutating ops and points the user to `pkg repair` or
  `pkg rollback`.
- **Repair health across restart.** A generation whose closure is recorded
  **unknown/unhealthy** by a repair verify row whose `damageSet` is nonempty
  (§10 intro/§10.7), with no subsequent clean verify, stays unhealthy across
  restart: startup recovery honors the signal, **blocks dependent state
  mutations**, and points the user to `pkg repair` (which re-runs Phase 0 and
  resumes per §10.8). It is cleared only by a clean final read-only verify
  (§10.7).

### 11.2 Recovery flows

| Symptom | Detection | Recovery |
|---------|-----------|----------|
| Truncated `manifest.json` (no valid checksum) | sidecar mismatch | restore by copying the active generation's durable snapshot `generations/gen-<active>.manifest.json` (§5.6), verifying it hashes to `manifestHash`; restore `lock.json` the same way from `gen-<active>.lock.json`. If the snapshot is itself gone/corrupt, refuse with manual instructions (`repair --from-lock`). |
| `current` broken / forest missing or `treeDigest` mismatch / output unrooted | startup check | pick the most recent *intact* generation (forest present + `treeDigest` verifies + all `outputRoots` rooted); (re)create its per-output root set if needed, then atomically repoint `current`; log `RecoveryNotice`. |
| Unfinished op in journal tail (no `committed`/`aborted`) | journal scan | recover by transaction state (§8.4): **prepared** → delete `gen-<id>.json` + its two snapshots + staging forest; **rooted** → remove per-output root set + delete forest + delete `gen-<id>.json` + its two snapshots; **activated** → restore `manifest`/`lock` from the durable snapshots + append `committed` row (forward). Previous gen stays active in the first two; the new (rooted, documented, `treeDigest`-verified) gen stays active in the third. |
| Unfinished `repair` in journal tail (Phase A cache-only partial, or Phase B.2 build partial) | journal scan (§10.8) | the closure's **unknown/unhealthy** marker stays in force (dependent mutations blocked); re-run Phase-0 read-only `nix store verify` to recompute the damage set, then **automatically retry only Phase A cache-only repair per-path** (after that fresh verification, approval-free); a partially-completed **Phase B.2 build does NOT silently resume** — its single-use `mode=build` capability is invalidated on restart, so recovery marks the op needing fresh preview/approval/capability and surfaces partial state. **Never** mark `repaired` or healthy until a fresh read-only verify confirms every target path clean. |
| Manifest missing for the active generation | startup check | refuse; require `repair --from-lock` (rebuild manifest from lock + store reality). |
| Checksum mismatch on a manifest | sidecar | quarantine file; refuse ops; `repair`. |
| Unfinished `prune` in journal tail (`intended` without `pruned`) | journal scan (§9.1) | **resume the exact prune intent before any command** (§12 barrier): idempotently finish deleting the user-owned forest/record/snapshots if still present, remove the privileged root-set dir if still present, then append `pruned`; never touches the active generation (re-checked at step 4). |
| Interior journal corruption / reorder / deletion / `seq` gap / `rowHash` break (not just a torn suffix) | journal chain verification (§5.4) | **fail closed**: refuse mutating ops; require `repair` (the longest valid prefix is kept, the offending tail is quarantined). |
| Two ops raced (shouldn't, due to lease) | lease nonce check | second op exits 72. |

### 11.3 Generation-scoped view snapshots (and "backups")

- Each generation durably owns its **candidate-view snapshots**
  `generations/gen-<id>.manifest.json` and `generations/gen-<id>.lock.json`
  (§5.6), written at the **prepared** step and referenced by
  `manifestSnapshot`/`lockSnapshot` + `manifestHash`/`lockHash` in
  `gen-<id>.json`. These are explicit, generation-scoped, immutable files —
  **not** a shared-content/by-hash store — so each generation's snapshots are
  independently deletable with no reference counting.
- They serve three roles: (1) the source from which **activated/committed**
  recovery restores the current `manifest.json`/`lock.json` views (§8.4); (2)
  point-in-time "backups" of the user's intent + realized identity per
  generation (recoverable via `rollback`/`repair --from-lock`); (3) the
  lock-free read source for read-only commands that do not take the per-user
  lease (§12).
- Cleanup is uniform: discarding a generation — whether by **prepared/rooted**
  recovery discard (§8.4), by **pruning** (§9/§9.1), or by `history --delete`
  (§9.1) — deletes exactly that generation's record `gen-<id>.json`, its
  activation forest, its per-output root-set directory (if created), and its
  **two snapshot files** (+ `.sha256` sidecars), in the crash-safe root-last
  order of §9.1. No other generation's snapshots are touched.
- The product does **not** auto-copy state off-host (deferred; plan 12).

## 12. Concurrency & leases

`pkg` coordinates concurrency with **three distinct mechanisms** — do not
conflate them:

1. **Per-user state lease** (this section) — a real advisory `flock` on
   per-user `run/lease` (held in the user's own process) that serializes one
   user's mutating ops and that user's own `gc`/pruning. This is the **only**
   filesystem `flock` in the model.
2. **Broker-internal machine-global local-build admission permit** (plan 04
   §5.3.1) — a fair in-process mutex/queue inside the singleton broker that
   serializes approved native local builds across all users; **not** a backing
   file, **not** a `flock`, **no** pid/boot-id record (the permit is owned by
   the broker-minted opaque handle, released after child containment, and a
   replacement broker runs its startup recovery barrier before admitting the
   first build).
3. **Broker-internal GC admission gate** (§8.5) — a counted R/W admission
   structure inside the singleton broker (one shared GC-inhibit permit per op
   handle; one exclusive permit for GC) that makes the global `nix store gc`
   safe across users during the realize→root window. It is **not** a `flock`
   and has **no** on-disk backing file.

**Recovery barrier.** No command — mutating or read-only — reads or mutates
state until startup recovery (§11) has run to completion and published a
reconciled active generation. Immediately after a crash `current` may already
be new while the mutable `manifest.json`/`lock.json` views lag (I8); the
barrier guarantees every command sees a reconciled state, never the lagging
post-crash views.

**Per-user state lease.**

- Mutating ops acquire an exclusive lease (`flock LOCK_EX` on `run/lease`).
  Lease record includes pid+nonce+opId; on start the holder writes its pidfile
  `run/pid`.
- **Two read classes, both consistent (no torn reads):**
  - *Leased consistent read* of the mutable `manifest`/`lock` views — take
    `LOCK_SH` on `run/lease` across the whole read. Used by ops that must read
    the live mutable views atomically with respect to a concurrent writer
    (e.g. computing a diff before mutating). `LOCK_SH` and `LOCK_EX` are
    mutually exclusive, so the reader sees a clean pre-op or post-op state,
    never a half-renamed one.
  - *Lock-free read* — do **not** take the lease; instead read the immutable
    active-generation snapshots
    `generations/gen-<active>.manifest.json`/`.lock.json` (§5.6), which are
    durable and self-consistent by construction. Used by `list`/`info`/
    `history`/`outdated`-style commands so they never block on a long install.
  There is deliberately **no** racy "`LOCK_SH` or none" middle ground: a
  reader either leases the mutable views or reads the immutable snapshot.
- Stale lease: if the holder pid is not alive (`kill(pid,0)`), the lease is
  considered abandoned; the next op rewrites it (after verifying no live
  Nix subprocess for the dead op via the journal). To be safe, the product
  also refuses to steal a lease younger than `lease_min_age` (default 60s).
- `doctor` (plan 06) reports lease state and can `--force-release` (requires
  confirmation; logs a SECURITY event).

## 13. Failure matrix (selected)

| Scenario | Outcome |
|----------|---------|
| Crash during the generation transaction (§8.4) | Resume on next start (**no command runs before recovery** — §12); recovery acts by state — **prepared**/**rooted** discard the unreachable staged forest + root set + record + snapshots (prev gen active); **activated** restores `manifest`/`lock` from the durable snapshots + appends the `committed` row (new gen already rooted + documented + `treeDigest`-verified; views may have lagged until this restore — I8). |
| Crash during `current` rename | `current.tmp.*` cleaned; recovery classifies by the **actual** relative `current` target + record/`treeDigest`/root-set/snapshot ground truths — old target ⇒ pre-swap discard (prev gen active); new target ⇒ **activated** restore-from-snapshots (§8.4). Never blindly assumes the previous `current` is intact. |
| Disk full mid-write | Temp file discarded; previous state intact; exit 65. |
| Concurrent `install` + `gc` (same user) | `gc` takes the per-user lease (§12) → same-user install refused `STATE_LOCKED` (72). |
| Concurrent `install` (user B) + `gc` (user A) | A per-user lease cannot block B; `gc`'s `nix store gc` instead waits on the broker's exclusive GC permit (§8.5) for B's realize→root GC-inhibit permit to drain — B's in-flight closure is never collected. |
| `nix store gc` ran externally (root) | The broker is the sole GC mediator and the installer schedules no automatic GC (§8.5), so an external collector is unexpected; `doctor` warns. Product-rooted paths survive, but realize→root outputs not yet behind a root set are **not** protected from an external collector (disclosed residual — the §8.5 gate restrains only broker-started collectors; v1 assumes sole-management, Q5.1). |
| Crash during `gc`/`history --delete` pruning (§9.1) | Recovery resumes the exact `prune intended` row before any command (§12 barrier): finishes the user-owned deletes + removes the root-set dir + appends `pruned` (idempotent). A crash before root removal may leak roots/space (later `gc` reclaims) but **cannot lose a live activation**; the active generation is never eligible. |
| Crash mid-`repair` (Phase A cache-only partial, or Phase B.2 build partial) (§10.8/§10.9) | Store repair is **non-atomic** (verified Nix 2.34.8): `addToStore(Repair)` deletes the live path before restoring (the store DB may still say it is valid); `replaceValidPath` moves the original aside before the replacement lands — a crash/power loss can leave a registered path missing. The affected closure stays **unknown/unhealthy** (dependent mutations blocked). Recovery re-runs read-only `nix store verify` **per target**, **automatically retries only Phase A per-path** (after that fresh verification, approval-free) for still-cache-repairable paths, and **requires fresh preview/approval/capability before repeating any Phase B rebuild** (the single-use capability is invalidated on restart → op marked needing re-approval). No path is marked `repaired` or healthy until its own read-only `post_verify` confirms it clean. |
| `pkg repair` mutation warning + residual (§10.9) | `repair` is explicitly user-initiated; before any Phase A/Phase B.2 mutation it marks the affected closure **unknown/unhealthy** (blocking dependent state mutations), warns that affected commands may be temporarily unavailable while a target path is deleted-then-restored (cache) or moved aside (local build), and journals per path. The shared GC-inhibit permit spans the repair so a broker `gc` cannot change root safety mid-mutation; normal store repair does not create a generation, rebuild the forest, re-root, or swap `current` (§10.6). |
| Capability replay / stale / mismatched / expired (§10.2) | A presented repair capability not in current server-side state, or whose uid/generation/typed StorePath set/`RepairBuildPlan` digest/policyVersion/mode does not re-match the request, **fails closed** (helper refuses; SECURITY logged). Capabilities do not survive helper/broker restart. |
| CLI disconnect mid-realize (user B) | The broker owns B's operation handle: disconnect/cancel triggers broker-owned cleanup (stop spawned realize/build, discard staging) and releases B's shared GC-inhibit permit (§8.5); no permit is stranded, no unrooted output is left behind. |
| Corrupt `lock.json` sidecar | Refuse mutating ops; `repair --from-manifest <gen-id>`. |
| Pinned path was GC'd by external action | `install`/`upgrade` of that selector reports it; suggests `unpin` then resolve. |

## 14. Security considerations (full model: plan 08)

- State dir owned by an admin/product user with `0750`; files `0640`; the
  lease/journal writable only by the product. No world-writable paths.
- **No privilege via state:** the product never `setuid`s; elevation is via
  the daemon/helper in plan 07.
- **Tamper-evidence / crash-detection (not same-uid auth):** sidecar sha256 +
  `generationHash`/`manifestHash`/`lockHash` detect edits/crashes in the
  manifest/lock/generation/snapshot JSON objects; the journal is append-only and
  **explicitly hash-chained** (every row carries `schemaVersion`, monotonic
  `seq`, `prevRowHash`, `rowHash`; §5.4). These bindings are corruption/
  crash-detection, **not** same-uid authentication — an attacker who can write
  a uid's `<user-state>` already owns that uid's state; cross-uid isolation is
  ownership/permissions (plan 07/08), not cryptography.
- **Rollback attack surface:** rolling back to an old generation with known
  vulnerabilities is a user choice; `outdated`/`doctor` flag vulnerable pinned
  outputs. Channel rollback protection is in plan 02.
- **Logs:** no secrets (only public keys); op logs may contain attribute names
  (considered user data).

## 15. Dependencies on other plans

- **plan 00** — product decisions (multi-user authoritative state per D-17; retention defaults).
- **plan 01** — where the state module sits in the layering; the **privilege
  split** the repair phases depend on (ARCH-INV-01/05/07/10: the unprivileged
  broker runs read-only `nix store verify` (Phase 0); the root helper is the one
  exceptional maintenance client that runs the fixed mutative `nix store repair`
  in the two mutating phases A and B). Plan 01 and this document use the same
  model: `verify` is read-only, `repair` is the separate mutating command (there
  is **no** `nix store verify --repair`; verified Nix 2.34.8), the capability and
  damage set bind to the **exact typed corrupt/missing registered-or-expected
  targets within the full computed closure reachable from the selected output
  roots (not merely `activation.outputRoots`; ARCH-INV-01/§12.4), and restart
  semantics auto-retry only Phase A and require fresh preview/approval/capability
  for Phase B (ARCH-INV-10). No reconciliation between the two docs is pending.
- **plan 02** — channel descriptor consumed as `channelSeq`; signing.
- **plan 03** — disposable index lives under `/var/lib/pkg/index/<channelSeq>/`
  (root-owned, shared, read-only; doc 03 §7), **not** under per-user `<user-state>`.
- **plan 04** — the pipeline that writes generations/lock/journal; §5–10 here
  are its storage contract. The **shared GC-inhibit permit** (§8.5) is taken on
  the op handle in plan 04's acquire phase **before** the broker dispatches the
  substitute/build/realization that can create unrooted outputs, and released
  at §8.4 rooted; plan 04's §5.7 `prepared`/`committed` steps must reflect the
  candidate-snapshot writes / current-view restores defined here (plan 04
  remains consistent — it already asserts the post-step-6 hash — but does not
  yet name the snapshots or the broker-internal permit). **Repair build reuse:**
  the approved repair build (§10.5) reuses plan 04's **machine-global local-build
  admission permit** (§5.3.1), the **single-operation approval flow** (§7), and
  the `BuildPlan` digest discipline (§5.2.1) — mirrored as the
  `RepairBuildPlan`/`repairPlanDigest` covering **every deriver output** the
  repair may rebuild (§10.4); `max-jobs=0`/`builders`-empty for cache-only and a
  small nonzero `max-jobs` for the approved build mirror plan 04's substitute
  vs build settings.
- **plan 06** — `list`/`history`/`rollback`/`gc`/`repair`/`outdated` CLI that
  surface this state. The only per-op log under `<user-state>/logs/` is the
  **sanitized, schema-versioned `<opId>.ndjson`**; **raw** adapter/Nix output
  stays broker-private under `/var/lib/pkg/log/broker/<internal-id>` and must
  not be copied verbatim into user state (§4/§6/§10). Plan 06's earlier
  `<opId>.nix.log` per-user-log guidance is superseded by this.
- **plan 07** — concrete paths, ownership, daemon, GC root topology, **the
  enforced singleton broker (§7.4) that owns the broker-internal GC admission
  gate (§8.5)**, the **broker-private raw-log directory**
  (`/var/lib/pkg/log/broker/`, 0700/0600, §4), and the **installer rule that no
  automatic GC timer/service/launchd job is scheduled** (only the product's own
  scheduler artifacts are removed/disabled). **Repair helper invariants are a
  hard dependency:** the root helper accepts only a closed, helper-issued opaque
  single-use capability and runs the fixed mutative repair with
  `max-jobs=0`/`builders`-empty (cache-only) or a small nonzero
  `max-jobs`/`builders`-empty (approved build), never a public/raw StorePath or
  argv (§10.2/§10.3/§10.5). The detailed **helper RPC/framing + broker
  supervision/child-containment** design is a **blocking cross-doc dependency**
  for §8.5's and §10's crash/lifecycle/capability claims; this document defines
  only the invariants.
- **plans 08–10** — corruption fault-injection tests, release ops, retention
  policy governance. **Plan 08 (security):** the repair capability model
  (§10.2) — no public/raw StorePath or argv ever accepted; the CLI carries only
  opaque ids/digests; replay/stale/mismatch/expiry fail closed; raw Nix output
  isolated from per-user state — is the threat-model boundary for repair and
  must be reflected in plan 08's HELPER/broker trust surface. **Plan 09
  (testing):** the §6.5 fault rows and PR-S6/AC items below must cover
  corrupt→detect→cache-only-repair, cache-miss→stop-before-build→approval, and
  crash-mid-repair recovery; the Fake↔Real parity job (§4.3) must pin the
  `verify` (read-only) / `repair` (mutative) split for the chosen managed Nix.
- **plan 11** — PR roadmap. **PR-30** (Repair flow + corruption recovery) must
  deliver the Phase-0 verify plus the two mutating phases (A cache-only helper
  repair; B.1 deriver-fallback preview/approval → B.2 approved helper build),
  the `RepairBuildPlan`/digest covering all deriver outputs, the helper
  capability invariants, the closure-unknown/unhealthy marking + dependent-
  mutation blocking, and the crash/restart recovery behavior of §10.8; its
  dependency set gains the detailed-broker/helper-RPC milestone (plan 01/07) and
  the build-admission/approval reuse from plan 04.

## 16. PR-shaped implementation checkpoints

- **PR-S1 — State module skeleton + checksummed read/write.** Atomic
  temp+fsync+rename, sidecar sha256, schemaVersion checks. *Acceptance:*
  kill -9 mid-write leaves prior file intact; sidecar mismatch → clear error.
- **PR-S2 — Desired-state & lock schemas + migrations.** v1 schemas, migration
  registry, `from_v0`/`to_v1` hooks. *Acceptance:* golden round-trip tests;
  migration test from a v0 fixture.
- **PR-S3 — Generation manifests + symlink-forest activation + `current` swap +
  transaction ordering.** Immutable manifests carrying the D-18 activation
  record (`kind:"pkg-symlink-forest"`, relative `treePath`, `treeDigest`,
  `entryCount`, `collisionPolicy` abort/keep-first/keep-last, sorted
  `outputRoots[]`, `collisionResolutions[]`, `manifestSnapshot`/`lockSnapshot`),
  monotonic ids, the relative-symlink atomic swap, the **generation-scoped
  candidate-view snapshots** (§5.6), and the §8.4 generation-transaction
  ordering (stage→prepared(= snapshots + record both durable)→rooted→rename→
  activated→committed; per-output roots before swap, committed marker after).
  **Activation invokes zero Nix.** *Acceptance:* `current` is always a
  `treeDigest`-verified relative link under crash injection at each of the four
  recovery states; the full per-output root set is created + fsynced before
  `current` switches to a new forest; **`prepared` is not reached until both
  candidate snapshots and `gen-<id>.json` are fsynced**; **activated** recovery
  restores `manifest`/`lock` byte-identically from the snapshots.
- **PR-S4 — Lease + journal + read model.** flock lease, append-only
  **hash-chained** NDJSON (§5.4: `schemaVersion`/`seq`/`prevRowHash`/
  `rowHash`; recovery accepts the longest sequence-contiguous hash-valid
  prefix, quarantines only a torn suffix, fails closed on interior
  corruption), recovery scan, the **recovery barrier** (§12), and the two read
  classes (leased `LOCK_SH` vs. lock-free immutable-snapshot read). *Acceptance:*
  two concurrent installs → second gets 72; crash injection at each transaction
  state (§8.4) → correct resume/abort; **no command observes state before
  recovery completes**; a lock-free reader during a long install sees a
  consistent snapshot (never a torn/half-written view); a deliberately torn
  journal tail is truncated to the last valid row, while an interior
  reorder/corruption fails closed.
- **PR-S5 — GC roots + `gc`/`gc --dry-run` + broker-internal GC admission
  gate + crash-safe pruning.** Per-output root-set topology (one symlink per
  selected output under `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/`),
  **crash-safe root-last pruning** (§9.1: prune intent → user-owned
  forest/record/snapshots → privileged root-set dir → `pruned`), and
  `nix store gc` invocation under the broker's **exclusive GC permit** (§8.5).
  *Acceptance:* dry-run byte estimate within tolerance; no protected
  generation's output is ever collected; **a second user's broker GC waits for
  an in-flight realize→root window and never collects an unrooted realized
  output** (two-UID, like AC-S19 for builds); CLI disconnect mid-realize does
  not strand a permit; **a crash at any point in §9.1 is resumed idempotently
  by recovery and never loses a live activation**.
- **PR-S6 — `repair` (Phase 0 verify + two mutating phases A/B + capability +
  recovery).** Read-only `nix store verify --recursive` over the **full computed
  closure reachable from the selected output roots** (Phase 0, broker-mediated)
  computes the damage set and **marks the affected closure unknown/unhealthy,
  blocking dependent state mutations**; **Phase A cache-only helper repair**
  (`nix store repair`, `max-jobs=0`, `builders` empty, managed
  substituters/keys, per-path) auto-repairs cache hits; **Phase B** (reached
  only on a Phase A cache miss with a valid deriver) splits into **B.1**, which
  stops before build and produces the ordinary build preview + single-op
  approval over a `RepairBuildPlan`/digest covering **every deriver output** the
  repair may rebuild, and **B.2**, the approved helper build, serialized by the
  broker build-admission permit, holding the GC-inhibit permit to
  verification/root safety, using no remote builders, run at small nonzero
  `max-jobs`; the **capability** (§10.2) is helper-issued, opaque, single-use,
  bound to the exact typed corrupt/missing closure targets (not merely
  `activation.outputRoots`), uid/generation/digest/policyVersion/mode, fails
  closed on replay/stale/mismatch, invalidated on restart; store repair leaves
  the generation/activation-forest/per-output-root-set/`current` unchanged and
  the shared GC-inhibit permit spans the non-atomic repair (§10.6/§10.9);
  **never marked `repaired` or healthy until a fresh read-only verify succeeds**. *Acceptance:*
  delete one output's store path → cache hit → `repair` restores it with no
  approval and no generation/forest/root/`current` change; corrupt a path whose
  deriver is not in any cache → `repair` stops (Phase B.1), shows the build
  preview, and only proceeds on explicit approval to a Phase B.2 rebuild;
  replay/stale/mismatched capability is refused; crash mid-Phase A cache-only
  repair resumes idempotently per-path after a fresh read-only re-verify (the
  closure staying unknown/unhealthy throughout); crash mid-Phase B.2 build marks
  the op needing fresh preview/approval/capability (single-use capability
  invalidated on restart; no silent resume); damage the *activation forest* (not
  a store path) → `repair` hands off to the separate Rust-only forest-recovery
  path (§10.6), re-materializes it with ZERO Nix, and `treeDigest` recomputes;
  `pkg repair` warns before mutation that affected commands may be temporarily
  unavailable; no raw StorePath/argv is ever accepted on the public channel.
- **PR-S7 — Corruption detection & recovery.** startup checks, recovery
  flows table implemented. *Acceptance:* each row of §11.2 has a test.

## 17. Testable acceptance criteria

1. After any kill -9 during a mutating op, on next start either the op
   completes or is marked aborted, and `current` is a relative symlink to a
   retained, `treeDigest`-verified forest of an intact generation whose every
   selected output is GC-rooted.
2. `pkg history` lists generations in chronological order with the active one
  marked; `pkg rollback` makes the previous one active by creating a new
  monotonic generation row (history stays linear).
3. `pkg gc --dry-run` does not remove any path; `pkg gc` prunes only retired,
  non-current generations outside the retention window (crash-safe root-last
  ordering, §9.1) and never touches the active generation's closure.
4. Mixed-rev lock: after the §7 scenario, `lock.json` shows different
   `nixpkgsRev` per entry and the active generation activates and verifies.
5. Deleting the store path of one installed output, then `pkg repair`,
   restores it from the substituter and leaves the active generation verified.
6. Sidecar checksum tampering of any state file (and any interior journal-row
   corruption/reorder/deletion/hash break) is detected on next startup and
   reported with the offending file/row; a merely torn journal suffix is
   truncated to the last valid row.
7. A concurrent second mutating op exits 72 and does not modify state.
8. `current` is never observed missing or dangling during a 1000-iteration
   random crash-injection test (acceptance gate; fault lane in plan 09).
9. Rolling back to a generation whose outputs include a path with known
   vulnerabilities triggers a `doctor`/`outdated` warning (integration with
   plan 03/06).
10. In the generation transaction (§8.4), a crash *after* the `current` swap
    always leaves `current` pointing at a forest whose every selected output is
    GC-rooted and whose `gen-<id>.json` is present and fsynced with a verifying
    `treeDigest`; a crash *before* the swap leaves the previous generation
    active with the staged forest/root-set/record unreachable from `current`
    and deletable by recovery. (Drives the doc 09 §6.5 fault rows and AC-T9.)
11. **Activation invokes zero Nix** (D-18/INV-11): instrumented tests confirm no
    `nix` subprocess is spawned during stage/rename/activate; the forest is
    built purely from already-verified `/nix/store` output paths.
12. **Per-output roots (INV-05):** every entry of
    `gen-<id>.activation.outputRoots` has a live symlink under
    `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>/`; all roots for a
    generation are created + fsynced before `current` switches to its forest.
13. **`treeDigest` binding (D-18):** tampering with any leaf in
    `activations/gen-<id>/` (repointing a symlink, swapping a target) is
    detected on next startup via `treeDigest` mismatch and refused.
14. **Collision policy (D-18):** under `keep-first`/`keep-last` the colliding
    path resolves to the deterministic per-file winner while **every
    non-conflicting file from the losing package remains visible** in the
    forest; `abort` (default) fails with `STAGE_COLLISION` (71). There is **no**
    `keep-all`/`--force` in V1.
15. **Generation-scoped snapshots (§5.6):** at the **prepared** state both
    `generations/gen-<id>.manifest.json` and `generations/gen-<id>.lock.json`
    exist and are fsynced with verifying `.sha256` sidecars **before**
    `gen-<id>.json` is written; their body hashes equal `manifestHash`/
    `lockHash`. Discarding a generation (prepared/rooted recovery, pruning, or
    `history --delete`) deletes exactly its record + forest + root set + the
    two snapshots, touching no other generation's snapshots, in the crash-safe
    root-last order of §9.1.
16. **Recovery barrier + activated restore (I3/I8):** no command observes
    state before startup recovery completes; after an **activated** crash
    (where the mutable views legitimately lag `current`), recovery copies the
    durable snapshots to `manifest.json`/`lock.json` and the views become
    consistent with the generation `current` actually resolves to.
17. **Lock-free read consistency (§12):** a `list`/`info`/`history`/`outdated`-
    style read that takes **no** lease reads the immutable active-generation
    snapshots and observes a consistent state even while a concurrent install
    is mid-commit (never a torn/half-written view).
18. **Broker-internal GC admission, two-UID (I9/§8.5):** user B's install, on
    its opaque handle, holds a shared GC-inhibit permit across its
    realize→root window; user A's `gc` blocks on the broker's exclusive permit
    until B roots (or aborts); an unrooted realized output is **never**
    collected by a broker gc. CLI disconnect of B mid-realize does **not**
    strand a permit (broker-owned cleanup releases it). The test must
    demonstrate cross-user safety — a per-user lease alone does **not** suffice
    (it cannot block another user's global collector), and there is **no**
    on-disk `run/gc-admission` lock.
19. **Crash-safe pruning / `history --delete` (§9.1):** under crash injection
    at each of the five prune steps, recovery resumes the exact `prune
    intended` row before any command, finishes the user-owned deletes, removes
    the privileged root-set dir, and appends `pruned` idempotently; a crash
    before root removal may leak roots/space but **never** loses a live
    activation; the active generation is never eligible (a concurrent rollback
    onto a prune target aborts the prune with nothing removed).
20. **Repair: Phase 0 verify + two mutating phases A/B (§10):** `pkg repair`
    first runs a read-only `nix store verify --recursive` **over the full
    computed closure reachable from the selected output roots** (broker-mediated,
    Phase 0); a **nonempty** damage set marks the affected closure
    **unknown/unhealthy** and **blocks dependent state mutations**; a cache hit
    repairs the damaged path **automatically via the root helper as Phase A**
    (`nix store repair`, `max-jobs=0`, `builders` empty) with **no approval**;
    a **cache miss** with a valid deriver **stops before any build** (Phase B.1),
    shows the ordinary build preview, and proceeds only on explicit
    single-operation approval to a Phase B.2 rebuild that runs via the root
    helper at small nonzero `max-jobs` under the broker build-admission permit
    with **no remote builders**; health is restored only by a clean final
    read-only verify (§10.7).
21. **`RepairBuildPlan` covers all deriver outputs (§10.4):** because Nix's
    `Store::repairPath` rebuilds **all** outputs of a deriver on a build-slot
    miss, the approved `RepairBuildPlan`/digest enumerates **every output** of
    every involved deriver — not only the damaged output; a digest mismatch at
    execution fails closed.
22. **Repair capability fails closed (§10.2):** the public CLI↔broker and
    broker↔helper channels carry **only opaque ids/digests** (no raw StorePath,
    no argv); a stale/expired/replayed/mismatched (uid/generation/set/digest/
    policyVersion/mode) capability is refused by the helper and logged;
    capabilities do not survive helper/broker restart.
23. **Never "repaired" until read-only verify (§10.7):** no repair op is marked
    `repaired` in state until a fresh read-only `nix store verify` confirms
    every target path clean, across both the cache-only and approved-build
    paths.
24. **Repair crash/restart recovery (§10.8):** a crash mid-Phase A cache-only
    repair is resumed idempotently per-path (approval-free, after a fresh
    read-only re-verify) while the closure stays unknown/unhealthy; a crash
    mid-Phase B.2 build marks the op needing fresh preview/approval/capability
    (single-use capability invalidated on restart) and **never** silently
    resumes the privileged build.
25. **Raw repair/verify logs are not in user state (§4/§6):** raw Nix/adapter
    output (store paths/derivers/cache URLs) lives only in the broker-private
    `/var/lib/pkg/log/broker/<internal-id>` (0700/0600); only sanitized,
    schema-versioned NDJSON is copied to `<user-state>/logs/<opId>.ndjson`; no
    `<opId>.nix.log` exists under `<user-state>`.
26. **Non-atomic store repair is treated as a residual (§10.9):** `pkg repair`
    is explicitly user-initiated and warns before any mutation that commands
    resolving through the affected forest may be temporarily unavailable; it
    marks the affected closure unknown/unhealthy and blocks dependent state
    mutations; each target path is journaled `intended`→`in_progress`→
    `post_verify`; the shared GC-inhibit permit spans the whole repair so a
    broker `gc` cannot change root safety mid-mutation; no status/message claims
    repair is atomic. Under crash injection mid-Phase A cache-only repair,
    recovery auto-retries only Phase A per-path after a fresh read-only
    re-verify; under crash injection mid-Phase B.2 build, recovery requires
    fresh preview/approval/capability before repeating the local build (stale
    capability invalid after restart). Normal store repair does not create a
    generation, rebuild the forest, re-root, or swap `current` (§10.6); the DB
    may still say a path is valid while it is absent or partially restored, so
    success is governed by read-only verify, not a validity lookup.

## 18. Unresolved questions / spikes

- **Q5.1 GC scope.** Confirm v1 is sole-manager so `nix store gc` global is
  acceptable; else design a root-quarantine scheme (move unmanaged roots into
  a protected subdir). *(Default: sole-manager in v1; per-user roots under
  `/nix/var/nix/gcroots/pkg/users/<uid>/` are all product roots per D-17.
  Cross-user collector safety is the **broker-internal GC admission gate**
  (§8.5), not the per-user lease — the latter serializes only one user's own
  ops/gc. An external `nix store gc` run by root outside `pkg` remains an
  out-of-scope sole-manager residual.)*
- **Q5.2 Journal chain (RESOLVED → normative).** The journal is explicitly
  hash-chained: every row carries `schemaVersion`, monotonic `seq`,
  `prevRowHash`, and `rowHash` (SHA-256 of the RFC 8785/JCS canonical row
  excluding `rowHash`, chained to the prior accepted row; §5.4). Recovery
  accepts the longest sequence-contiguous, hash-valid prefix and fails closed
  on interior corruption/reorder/deletion. State hashes are corruption/crash
  detection, **not** same-uid authentication.
- **Q5.3 Per-user vs shared profile (RESOLVED → D-17).** Authoritative package state is per-user, keyed by uid (`<user-state>`); the runtime/channel/index/store service is root-owned and shared. The schema carries a `uid` dimension; each user has an independent `current`/generations tree.
- **Q5.4 Retention defaults.** `keep_generations=10`, `max_age_days=30` are
  guesses; confirm with UX (plan 06) and disk-budget reality.
- **Q5.5 `repair --from-lock`** rebuild manifest from lock+store when the
  manifest is gone — define exact trust rules (do we trust the lock file alone
  if its sidecar is also gone?). *(Default: require at least the active
  generation's manifest hash from the journal; else refuse.)*

## 19. Sources (current Nix behavior)

[^gc-roots]: Garbage collector roots, Nix Reference Manual, "Garbage
Collector" → https://nixos.org/manual/nix/stable/package-management/garbage-collection.html
[^nix-gc]: `nix store gc` (new CLI; `nix-store --gc` legacy equivalent), Nix Reference Manual →
https://nixos.org/manual/nix/stable/command-ref/nix-store.html#operation---gc
[^add-root]: `--add-root` is an **option** on `nix-store` operations
(`--realise`/`--install`/…), **not** a standalone subcommand and **not** the
mechanism `pkg` uses to register an already-realized path. `pkg` creates GC
roots by placing a symlink directly in the daemon's scanned gcroots directory
(`/nix/var/nix/gcroots/pkg/users/<uid>/`); Nix treats any symlink in a scanned
gcroots directory as a root. → https://nixos.org/manual/nix/stable/command-ref/nix-store.html#opt-add-root ;
garbage-collection roots → https://nixos.org/manual/nix/stable/package-management/garbage-collection.html
[^profile-rollback]: Profile generations / rollback, Nix Reference Manual →
https://nixos.org/manual/nix/stable/command-ref/files/profiles.html
[^profile-gc]: `nix-collect-garbage`, Nix Reference Manual →
https://nixos.org/manual/nix/stable/command-ref/nix-collect-garbage.html
[^store-verify]: `nix store verify` — the **read-only** modern command (NAR /
  signature verification; **no mutation**; an untrusted `allowed-user` may run
  it). Nix Reference Manual →
  https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3-store-verify.html
[^store-repair]: `nix store repair` — the **separate mutative** modern command
  (distinct from read-only `verify`; the daemon **rejects repair for untrusted
  clients**, so it is run only by the root helper). Verified against Nix 2.34.8
  source: `Store::repairPath` **first tries a Repair-mode substitution**; only
  if that fails **and** the output has a valid deriver does it **rebuild ALL
  outputs of that deriver** (`bmRepair`). **Repair is not atomic (§10.9):** on a
  cache hit the repair runs via `LocalStore::addToStore` in `Repair` mode, which
  **deletes the live real path before substituting/restoring and re-validating
  its NAR — during which the path may be absent or only partially restored on
  disk while the store DB validity record may still say it is valid**; on a
  rebuild it runs via `replaceValidPath`, which **moves the
  original aside before moving the replacement into place** (caught exceptions
  attempt rollback, but a crash/power loss can leave a registered path missing).
  → new-store CLI command reference,
  https://nixos.org/manual/nix/stable/command-ref/new-cli/ (and
  `Store::repairPath` / `bmRepair` / `addToStore(Repair)` / `replaceValidPath`
  in the Nix 2.34.8 `src/libstore/` tree).
[^max-jobs-builders]: `max-jobs = 0` **disables local builds but still permits
  substitutions**; an **empty `builders`** setting is additionally required to
  prevent **remote** builds. So `nix store repair` under `max-jobs = 0` +
  `builders =` can only repair via a cache hit, never by building. Nix Reference
  Manual (conf-file: `max-jobs`, `builders`) →
  https://nixos.org/manual/nix/stable/command-ref/conf-file.html
