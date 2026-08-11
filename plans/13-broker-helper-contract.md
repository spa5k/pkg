# 13 — Broker/Helper Framing, Capability, and Lifecycle Contract

> **Status:** Accepted and reference-implemented 2026-08-09 (PR-39). This document closes the
> wire-design item left open by DR-017. Linux credential/socket bindings remain PR-27; macOS
> launchd/XPC bindings remain PR-28; Real-Nix execution remains PR-36.

## 1. Scope

This document is the normative V1 contract for:

- the product-owned CLI↔broker and broker↔helper frames;
- transport-derived peer authentication;
- opaque operation-handle lifecycle and cancellation;
- the privileged `MaintenanceAdapter` grammar;
- expiring, single-use repair capabilities;
- broker-internal build/GC admission;
- bundled-Nix child containment; and
- broker/helper restart handshakes.

It does not choose Linux/macOS socket APIs or service definitions, and it does not implement
the real Nix command adapter. Those bindings consume this contract without widening it.

## 2. Fixed frame envelope

Every request and response is exactly one length-delimited frame. Integers use network byte
order. The fixed 20-byte header is:

| Offset | Width | Field | Required value |
|---:|---:|---|---|
| 0 | 4 | magic | ASCII `PKG1` |
| 4 | 2 | protocol version | `1` |
| 6 | 1 | channel | `1` CLI↔broker; `2` broker↔helper |
| 7 | 1 | method | closed table below; zero/unknown refused |
| 8 | 8 | request id | nonzero correlation id; never authorization |
| 16 | 4 | payload length | exact remaining byte count, maximum 1 MiB |

The payload is one strict JSON object selected by `(channel, direction, method)`. It is not a
generic JSON-RPC envelope. Duplicate/unknown fields, trailing bytes, unknown enums, invalid
promoted strong types, wrong channel/version/method, zero request ids, length mismatch, and
oversized payloads fail before dispatch. The header version replaces a body `schemaVersion`;
there is exactly one version discriminator, not two independently drifting ones.

### 2.1 CLI↔broker methods

| Method | Request | Response |
|---:|---|---|
| 1 | `Begin { operation }` where operation is a closed V1 class | `Started { handle }` |
| 2 | `Poll { handle }` | `Status { running|completed|cancelled }` |
| 3 | `Cancel { handle }` | empty `Cancelled` acknowledgement |
| 10 | `Version { handle }` | validated `VersionInfo` from the pinned managed runtime |
| 11 | `EvaluateDerivation { handle, request }` | validated `DerivationPlanReport` |
| 12 | `PathInfo { handle, path }` | validated `PathInfoReport` |
| 13 | `Substitute { handle, path }` | validated `SubstituteReport` |
| 15 | `Verify { handle, request }` | validated `VerifyReport` |
| 16 | `Gc { handle }` | validated `GcReport` |

Every exposed adapter method may return the same method id with the alternative strict body
`{ "error": <closed NixAdapterErrorCode> }`. It carries no stdout, stderr, paths, argv, or free
text. A recognized adapter failure is a completed RPC and does not poison the connection; malformed,
unknown, or method-mismatched failures are protocol errors and do.

The lifecycle frame contains no generic command payload. Each later command integration owns a
new typed method/body or invokes the lifecycle API internally; it may not add `argv`, expression,
flake, option, environment, substituter, trust-key, arbitrary path, or arbitrary verb fields.

### 2.2 Broker↔helper methods

| Method | Request | Response |
|---:|---|---|
| 1 | validated complete `RootSet` | `RootSetReport` |
| 2 | `{ ownerUid, generation }`, no filesystem path | empty removal acknowledgement |
| 3 | validated server-side `VerifiedRepairScope` | opaque maintenance capability |
| 4 | opaque maintenance capability only | sanitized typed per-path outcomes |

Method 3 transports the broker's already verified scope to helper-private state. Method 4 never
repeats caller-selected paths or knobs: execution authority is recovered only from the helper's
server-side capability record.

## 3. Peer authentication is sideband, never payload

- CLI→broker: the broker obtains the real uid from the OS transport. Any payload/metadata uid
  claim is absent; the in-process reference has separate authenticated/claimed lanes solely to
  prove an impersonation mismatch is rejected.
- Broker→helper: the helper accepts only the configured unprivileged broker service uid or the
  platform-equivalent authorized-client identity. End-user CLI connections are impossible.
- Request ids, operation handles, and capability bytes do not authenticate a transport peer.
- PR-27 binds these facts to Linux `SO_PEERCRED` and systemd-owned endpoints. PR-28 binds them
  to the accepted launchd/XPC authorized-client mechanism. Neither transport may deserialize a
  request before peer authentication succeeds.

## 4. Operation lifecycle

An operation handle is an opaque `op_` plus 256-bit lowercase token minted from fresh
broker-private entropy, the broker epoch, a monotonic counter, caller uid, and operation class.
The raw handle is authorization-sensitive and has redacted `Debug` output.

- A handle is bound to one authenticated uid and one broker epoch.
- The fixed reference lifetime is 30 minutes; expiry releases every held admission gate.
- Poll/cancel by another uid returns the same closed invalid-handle failure as an unknown handle.
- Completion, cancellation, expiry, and CLI disconnect release the build lease, GC lease, and
  every shared GC-inhibit permit held by that operation.
- Broker restart rotates entropy and empties handles and admission. Old sessions return
  `SessionRestarted`; no operation silently resumes.
- The durable journal remains authoritative for later crash recovery. An in-memory handle is
  never evidence that a state mutation committed.

## 5. Privileged maintenance grammar

`MaintenanceAdapter` is object-safe, `Send + Sync`, separate from `NixAdapter`, and exposes
exactly three borrowed-input methods:

```rust
publish_root_set(&RootSet) -> RootSetReport
remove_root_set(&RemoveRootSetRequest) -> ()
repair_store_paths(&RepairStorePathsRequest) -> RepairStorePathsReport
```

`NixAdapter` has seven unprivileged methods and no repair or root-write operation.

- `RootSet` is nonempty, capped at 4096 entries, sorted by traversal-safe `RootName`, rejects
  duplicate names, and maps only to typed `StorePath` values.
- Removal carries only authenticated owner uid plus canonical `gen-<digits>` id. The helper
  derives the filesystem location.
- The repair execution request contains only an opaque helper-issued capability.
- There is no public helper input for raw filesystem paths, installables, derivations,
  expressions, flakes, argv, options, environment overrides, substituters/keys, output
  selection, or arbitrary verbs.

The in-process helper models atomic root-set publication in memory. PR-27/28 implement the
required staged temporary directory, complete symlink set, directory `fsync`, atomic `rename`,
and parent `fsync` on the real filesystem.

## 6. Maintenance capabilities

A capability is a lowercase opaque 256-bit token with redacted `Debug`. Its helper-private
record binds all of:

- authenticated caller uid;
- an existing pkg-owned rooted generation;
- the nonempty, sorted, de-duplicated full verified damage `StorePath` set (cap 4096);
- build-plan digest for `mode=build`, absent for `mode=cacheOnly`;
- nonzero policy version;
- exact repair mode; and
- helper epoch, broker epoch, and fixed five-minute expiry.

Issuance refuses an unrooted generation, cross-uid scope, empty/oversized path set, or mode/plan
mismatch. Redemption removes the record before execution. Reuse reports replay; expiry, unknown
token, removed generation, cross-uid use, helper restart, or broker restart fail closed. Neither
capabilities nor consumed-token memory survive restart. Durable root sets do survive restart.

Phase A/B semantics remain those in plans 05/09: cache-only executes with `max-jobs=0` and empty
builders; build mode requires the ordinary preview/approval and plan digest. PR-30 supplies the
real state resolution, re-derivation, journal, final verification, and backend execution.

## 7. Machine-global admission

The broker owns one in-memory admission controller:

- one exclusive FIFO local-build holder, available only to `Build` and `Repair` operations;
- one exclusive GC holder, available only to `Gc` operations; and
- shared GC-inhibit holders, available only to `Build`, `Activate`, and `Repair` operations.

GC cannot begin while any inhibitor exists; inhibitors cannot begin while GC is active. All
permits are operation-handle state and release on completion/cancel/disconnect/expiry/restart.
Each operation also owns a private cooperative-cancellation token. Terminal lifecycle transitions
signal that token before discarding private state. Build-admission waits consume it alongside any
local caller cancellation; broker-owned execution will consume the same authority when that path
lands. No cancellation authority crosses the IPC boundary.
Contended build/repair operations may join the in-memory FIFO and wait with cooperative
cancellation; lifecycle cancellation removes queued waiters as well as holders, and a nonblocking
repair probe cannot bypass an existing waiter. Admission registration is serialized with operation
validation so cancellation/restart cannot strand a just-validated waiter. A queued reservation
continues to block GC during the holder-to-waiter handoff. There is no backing-file `flock`. The
existing per-user state-mutation lease remains a separate filesystem lock.

## 8. Bundled-Nix child containment

The broker constructs child policy once from an exact absolute executable matching
`/opt/pkg/nix/<validated-version>/bin/nix`; callers cannot select it per operation. Launchers:

- call `env_clear` and install only the platform-fixed private broker `HOME` and its `TMPDIR`,
  the fixed managed `NIX_CONFIG`, `NIX_REMOTE=daemon`, `NIX_STATE_DIR=/nix/var/nix`,
  `NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket`, an explicitly empty
  `NIX_USER_CONF_FILES`, and `PATH=/usr/bin:/bin`; Linux uses
  `/var/lib/pkg/broker-home`, while macOS uses
  `/Library/Application Support/pkg/broker-home`;
- create a distinct process group;
- on cancellation send `SIGTERM` to the group, wait the fixed five-second grace, then `SIGKILL`;
- accept argv only from typed adapter methods, never from the framed request; and
- therefore cannot forward `--expr`, `--impure`, substituter/key, flake-registry, environment,
  or arbitrary option input.

Real spawning and signal code lands with the Real-Nix adapter. The PR-39 reference exposes only
the immutable policy, so it cannot accidentally become a raw-process API.

The privileged repair executor is a distinct fixed launcher. Nix 2.34.8 returns
`repairPath is not supported by store 'daemon'`, including for root, so each helper repair/verify
probe pins `--store local` against the exclusively managed `/nix/var/nix` store. The helper still
clears the environment, uses the root-owned managed config, and has a distinct root-owned private
home/tmp pair (`/var/lib/pkg/helper-home{,/tmp}` on Linux;
`/Library/Application Support/pkg/helper-home{,/tmp}` on macOS). It never reuses the
broker-owned home. The launcher bounds output/time and accepts only a
capability-resolved `VerifiedRepairScope`; there is no framed or public store URL, path, argv,
option, substituter, or key input. Cache-only pins `max-jobs=0` and empty builders, then verifies
the path and reports `CacheMiss` only when the repair command succeeded, the path remains damaged,
and the local store is still responsive. Build mode pins `max-jobs=1` and must verify clean.

The Linux production entry point consumes exactly one systemd-activated listener and validates its
pathname before initializing state. It requires effective uid 0 and resolves the dedicated broker
uid from the installed account database; uid 0 is never accepted as the broker. It safely creates
only `pkg/users` beneath an already trusted root-owned `/nix/var/nix/gcroots`, constructs the
`RootNixRepairExecutor`, and injects it into the helper capability engine. Peer authentication still
runs before frame decoding on every connection.

The Linux broker entry point separately requires the resolved non-root broker uid and the exact
systemd-activated `/run/pkg/broker.sock`. It authenticates every client from `SO_PEERCRED`, admits
at most 32 concurrent sessions, and applies finite monotonic whole-frame read and write deadlines;
partial traffic cannot reset them. Over-limit,
malformed, unauthenticated, and timed-out clients are connection-local failures. The entry point
  serves lifecycle begin/poll/cancel plus six typed adapter methods. The broker checks that the
  caller owns a live operation handle of an authorized class before invoking its fixed
  `RealNixAdapter`; `Gc` also acquires exclusive GC admission. Method 14 is the closed
  `ApproveBuild` pointer: it carries only the live handle, displayed plan digest, allowlisted approval
  source. The broker adds its own observed timestamp inside the authority boundary. The request
  carries no caller-created `BuildApprovalReceipt`, target,
  derivation, or Nix option, and therefore cannot create authority unless a trusted dispatcher has
  already retained the exact private plan under that caller-bound handle. The broker's in-memory build operation now retains at most one private `BuildPlan`,
  returns only its sanitized preview/digest, validates approval against that exact digest, journals a
  broker-derived private operation identity before retaining the private receipt, and permits only
  one approval. The state is bound by the existing authenticated uid, handle, epoch, status,
  and expiry; cancellation, disconnect, expiry, and restart make it unusable. This API is internal and
  adds no raw target/receipt input. Closed method 17 fetches only the strict `BuildPreview` from an
  already prepared caller-bound plan; nested unknown fields, invalid invariants, and private-plan
  extensions fail decoding. Cancellation, disconnect, expiry, and restart
  revoke even an approval whose journal write is in flight. The in-process broker now consumes the
  private receipt itself: it joins FIFO build admission, takes a GC inhibitor, replans and checks
  volatile resources under admission, and invokes only the typed adapter. A success retains both
  permits until authoritative rooting and operation completion; a refusal consumes the approval and
  releases both. Lifecycle cancellation during the synchronous adapter call is signalled immediately
  but defers permit release until that call returns, preventing GC from racing in-flight outputs.
  The production resource seam is fixed to the managed `/nix` filesystem and the host one-minute
  load average. It uses safe filesystem statistics plus bounded, fixed OS load sources and rejects
  unavailable, malformed, non-finite, negative, zero-capacity, or overflowing measurements.
  The broker now retains that trusted replanner capability with the private plan before approval;
  dispatcher-facing execution accepts only the handle, exact digest, a trusted in-process resource
  probe, and managed adapter, and invokes no caller-supplied closure. The heuristic disk estimate is
  fixed during trusted preparation, retained beside the private plan, and reused for both the public
  preview and admission; execution accepts no estimate. An unavailable estimate remains honest in
  the preview and makes execution fail closed before the adapter runs, while an explicit zero-byte
  estimate is invalid. The concrete replanner re-observes host
  facts and runs the authenticated source/evaluation/cache/plan pipeline on every call. The
  production observer is now available: construction binds it to the exact managed `nix.conf`
  rendered from the retained verified channel, and observation rechecks root-owned safe filesystem
  state, fixed non-root/non-login native build users through the configured system account-search
  view, host cores, and actual managed-daemon membership in the Linux cgroup-v2 service (with no
  Darwin cgroup claim). Production command-intent preparation is also connected internally: a
  non-serializable object accepts only a retained verified channel, typed selectors, optional authenticated
  index and the contained adapter; it derives the native target, produces the initial plan/replanner
  pair, and installs both through the authenticated caller's live build handle. The framed method
  is now closed method 18: it carries only the live build handle plus bounded, unresolved,
  unpinned `CurrentChannel` package selectors, invokes the injected authenticated authority, and
  returns only the sanitized preview. Channel, index, target system, derivations, store paths, and
  Nix controls are absent from the wire. Closed method 19 carries only the same live handle plus the
  displayed plan digest. The authority supplies the exact retained replanner, preparation-time disk
  estimate, fixed host resource probe, and contained managed adapter. Success returns the validated
  typed `BuildReport`; failure returns one of six stable redacted refusal codes without killing an
  otherwise healthy connection. No receipt, estimate, resource measurement, target, path, or Nix
  control crosses the execution request. The production service entry point still must bootstrap
  and inject the long-lived refresh owner before enabling this method on its public listener. The broker-owned
  in-memory authority now supplies a consistent verified-channel/authenticated-index snapshot,
  rejects rollback, policy downgrade and same-sequence descriptor reuse, drops a stale index on
  channel advance, and accepts a replacement index only when bound to the exact current descriptor.
  Its production prepare-and-install entry accepts only typed selectors plus the transport-derived
  caller and live handle, and releases its state lock before host/Nix work. The authenticated index is a non-forgeable Rust capability
  produced only after the downloaded compressed artifact matches the descriptor digest, bounded
  Brotli decoding produces a bounded document, and source-identity, strict-schema, invariant-rebuild and canonical-byte
  checks; a caller-created `IndexDocument` cannot enter production preparation. The fixed channel
  client obtains descriptor and host index from one authenticated TUF repository view, prechecks the
  signed target length before allocation, and releases bytes only after the complete target stream
  verifies; that TUF target capability is still unusable by build authority until the compressed
  index verifier promotes it. The client owns crash-durable accepted descriptor identity in its
  private datastore and automatically applies it to every refresh, so broker restart cannot reset
  product sequence/policy rollback checks; the strict record is atomically replaced and directory-
  fsynced, refresh transactions are async-serialized, and unsafe file types or modes fail closed. A
  durable first-run marker makes interrupted initialization retryable without treating missing
  established state as fresh; an explicit exact-seed migration handles the prior caller-owned state
  format and tightens its legacy lock mode. Indexed refresh advances rollback memory only after its
  required authenticated target succeeds. Semantic promotion runs through a typed callback while
  the serialized channel transaction remains held, so sequence commit cannot precede compressed
  index validation. A long-lived broker refresh owner derives the native system from the compiled
  target, bootstraps only from the promoted pair, and atomically replaces channel plus index in live
  authority; none enters from the command wire. Adapter failures cross only the closed error-code envelope;
  authorization, admission, framing, and transport failures still terminate the connection without
  disclosing private state. Product-command execution is still not fabricated before the dispatcher
  is connected.

Approval durability has two deliberately separate sinks. The invoking CLI appends the public,
sanitized approval phase to its uid-owned operation journal while it holds the existing exclusive
state lease. The broker independently appends the authority-side grant to
`log/broker/approvals.ndjson`, bound to the kernel-authenticated uid and private operation id, before
issuing a receipt. This service-private audit is hash-chained, sequence-contiguous, `0700`/`0600`,
append+fsync, replay-refusing, and fails closed on a symlink, unsafe owner/mode, interior corruption,
torn suffix, size overflow, or duplicate operation id. The broker never receives write access to a
user's state directory, and a CLI-only row never constitutes broker authority.
The production service opens this sink only in the approval-wire change and passes a journal bound
to the kernel-authenticated peer uid into `approve_build`; a receipt cannot be retained first.

The CLI-side lifecycle client connects only to the platform's compiled-in broker endpoint, bounds
connection establishment to five seconds, uses monotonic nonzero request ids, enforces the same
one-MiB frame ceiling before allocation, and applies one monotonic deadline across the complete
request-write/response-read transaction. It rejects a response-id or response-kind mismatch and permanently
refuses reuse of that connection after any framing, transport, or correlation failure. Caller uid
still never appears in the request. `BrokerNixAdapter` wraps this lifecycle transport for the six
exposed typed methods, using a fresh connection and operation handle per call. It converts an
authenticated adapter-failure envelope back into a redacted `NixAdapterError` with the same closed
code; transport and protocol failures remain generic operation failures. `build` returns
`PermissionDenied` locally without opening a connection. This adapter foundation does not replace
`UnavailableEngine`: that happens only after the product-command dispatcher and authenticated build
capability are connected.

## 9. Restart handshake

1. Supervisor restarts broker and/or helper.
2. Broker rotates its epoch/entropy and clears operations/admission.
3. Helper restart rotates helper epoch/entropy; broker restart notification rotates broker epoch.
   Either clears issued/consumed capabilities but preserves durable root sets.
4. Broker re-authenticates to helper using transport peer identity.
5. CLI re-authenticates to broker using transport uid and opens a new operation.
6. Recovery re-reads durable journal/state. Cache-only repair may resume only after a fresh
   Phase-0 verify; build repair requires fresh preview, approval, plan digest, and capability.

There is no resume-by-token path.

## 10. Contract evidence

The reference tests prove:

- exact request/response round trips on both channels;
- rejection of wrong magic/version/channel/method/length, extended JSON, raw option/path/flake
  payloads, forged uppercase tokens, and every truncated header length;
- uid impersonation and non-broker helper peers fail before dispatch;
- capability replay, expiry, cross-uid use, unrooted generation, helper restart, and broker
  restart fail closed;
- cancellation/disconnect/expiry/restart release build and GC admission;
- a FakeNix Phase-0 corruption result flows through a framed in-process capability repair; and
- child policy has a canonical bundled binary, scrubbed environment, process-group termination,
  and no public argv surface.

## 11. Downstream ownership

- PR-27: Linux authenticated transports, helper filesystem implementation, systemd units.
- PR-28: macOS authenticated transports, helper filesystem implementation, launchd/XPC.
- PR-22/26: integrate GC/build gates.
- PR-30: repair state resolution, capabilities, journaling, and real Phase A/B execution.
- PR-36: Real-Nix adapter and child launcher, consuming the immutable containment policy.
