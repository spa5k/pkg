# 07 — Platform Installation & Runtime

> Owner: execution track. **Planning only**; no Rust code.

## 1. Purpose

Define **how the product is installed, how it bundles and operates a managed
Nix runtime, and how it behaves across Linux and macOS** — including the
daemon, the store, privilege boundaries, architecture detection, shell PATH
integration, uninstall, and the V1 rule that the product **detects any
existing unmanaged Nix and refuses** (with manual remediation) and **never
auto-removes it**.

A central, explicitly-required **spike** is called out: the Nix store prefix
is **not relocatable** in stock Nix; the product must not assume otherwise.

## 2. Scope / Non-scope

**In scope**

- Linux and macOS installer/daemon/store/privilege layouts.
- Bundled, pinned Nix runtime and its `nix.conf` (trust-locked).
- Architecture / `system` detection and mapping.
- Shell PATH integration and unattended/scripted install.
- Unmanaged-Nix detection and refusal policy; uninstall boundaries.
- The store-prefix spike and multi-user vs single-user decision.

**Non-scope**

- Channel signing & key ops (plan 02); index (plan 03); pipeline/state
  internals (plans 04/05); CLI verbs (plan 06); threat model & release ops
  (plans 08/10). This plan defines the *substrate* those run on.

## 3. Invariants

| # | Invariant |
|---|-----------|
| I1 | The product **exclusively manages** its Nix installation in V1. If any unmanaged Nix is detected, the product refuses to operate beyond `doctor`/help and **never deletes** the unmanaged install. |
| I2 | The Nix store prefix is **`/nix/store`** (stock Nix). The product makes no assumption of a relocatable store; this is a **required spike** (§9). |
| I3 | The bundled Nix runtime version is **pinned by the signed channel descriptor** and is not user-overridable. |
| I4 | Trust knobs (substituters, trusted keys, `require-sigs`, sandbox) are **root-owned and channel-locked**; user env overrides (`NIX_*`, `FLAKE_*`) are ignored and flagged by `doctor`. |
| I5 | The **product binary is not setuid**. The privileged surface is a single **non-setuid root helper/service** (`pkg-root-helper`), reachable **only** from the **unprivileged singleton broker** over a private closed channel; the CLI never invokes the helper directly. The broker (`pkg-nix-broker`) is the sole general Nix client/mediator and bundled-CLI spawner. |
| I6 | Uninstall is **explicit and total only** (removes product + managed Nix + state). There is no partial uninstall that leaves a half-managed Nix. |
| I7 | The raw Nix daemon socket is **not exposed to ordinary users**. The **unprivileged singleton broker** (`pkg-nix-broker`) is the sole client of the managed daemon and the single mediator for substitution, build admission, `verify`, liveness-respecting `gc`, and per-output GC-root creation (D-18). The broker is in `allowed-users` but **never** `trusted-users` (`trusted-users = root` only); in Nix 2.34.8 trusted-users are effectively root-equivalent, so the daemon rejects the broker's mutating repair. Nix 2.34.8 also rejects `repairPath` over the daemon protocol even for root, so the root helper does **not** connect to the daemon: for the capability-gated repair operation only, it opens the exclusively managed store explicitly as `--store local` and accepts no caller-selected store URL, path, argv, or option (§7.4). The framed public broker RPC/peer-auth/capability schemas are the next required milestone — the broker/helper boundary itself is **accepted**. |

## 4. Architecture & `system` detection

The product maps host to a Nix `system` triple (*confirmed* [^system]):

| Host | `system` | Build mode |
|------|----------|------------|
| x86_64 Linux | `x86_64-linux` | substitute first; native local build allowed (D-11) |
| aarch64 Linux | `aarch64-linux` | substitute first; native local build allowed (D-11) |
| x86_64 macOS (Intel) | `x86_64-darwin` | substitute first; native local build allowed (D-11) |
| aarch64 macOS (Apple Silicon) | `aarch64-darwin` | substitute first; native local build allowed (D-11); **Rosetta not used** for x86_64-darwin paths |

Detection: `uname -m` + `uname -s`, plus `sysctl -n hw.optional.arm64` on
macOS to distinguish Apple Silicon (report `aarch64-darwin`, never fall back
to Rosetta `x86_64-darwin` binaries in v1). Linux: parse `/proc/sys/kernel`
and glibc/musl where relevant; v1 targets glibc `*-linux` (musl is a spike,
see Q7.3).

Unsupported arch → installer refuses with a clear message and links to the
support matrix.

## 5. Bundled Nix runtime

### 5.1 What is bundled

A specific **Nix release** (e.g. 2.x stable, exact version pinned per channel
descriptor fields `nixRuntime.version` and per-system
`nixRuntime.perSystem.<system>.{url,sha256}` — doc 02 §7). Bundled as a prebuilt,
self-contained Nix distribution under a product-owned prefix (e.g.
`/opt/pkg/nix` on Linux, `/opt/pkg/nix` or `/Library/pkg/nix` on macOS) —
**not** `/nix` itself, which belongs to the store layout (§6).

> **Store-prefix spike (required, §9):** stock Nix is compiled with the store
> path baked in (`/nix/store`). Confirm the bundled Nix build we ship uses
> `/nix/store` and that the rest of our files (binary, state) can live under a
> different prefix without rebuilding Nix. Do **not** assume relocatable store.

### 5.2 The generated `nix.conf` (trust-locked)

Owned by root (or the daemon user), mode `0644`, path
`<daemon-state>/nix.conf` referenced by the daemon. The file is **rendered per
platform** from the signed channel descriptor as one self-contained block. The two
blocks differ in exactly two places: (a) the `cgroups` token in
`experimental-features` (Linux only), and (b) the `use-cgroups` setting (Linux
only; omitted entirely on macOS, never set to `false`). Every other line is
identical. The installer/runtime renders the **complete** block for the target
platform — there is no shared "common" file that a platform patches.

```ini
# LINUX — <daemon-state>/nix.conf  (root-owned, mode 0644, rendered from the
# signed channel descriptor; only this exact key set is emitted).
build-users-group = nixbld            # group `nixbld` (created by installer); build users `nixbld1..N`
trusted-users = root                  # ONLY root is trusted. In Nix 2.34.8 trusted-users are
                                     #   effectively root-equivalent, so the broker is NEVER trusted.
allowed-users = pkg-nix-broker        # the unprivileged dedicated broker is the ONLY permitted
                                     #   client; ordinary users are not listed and never reach the socket.
experimental-features = nix-command flakes cgroups   # `cgroups` appended on LINUX ONLY
sandbox = true                       # builds run sandboxed
sandbox-fallback = false             # fail closed, never build unsandboxed
allow-import-from-derivation = false # pure evaluation: no realization / import-from-derivation before approval (BOTH platforms)
use-cgroups = true                   # LINUX-ONLY compiled setting. Per-build process grouping,
                                     #   lingering-process cleanup, and CPU accounting/statistics.
                                     #   NOT security isolation and NOT a resource cap
                                     #   (Nix 2.34.8 writes no memory.max/cpu.max/pids.max/IO knobs).
require-sigs = true
builders =                           # EMPTY on BOTH platforms: NO remote/distributed builders in v1
                                     #   (D-11/INV-08); all builds are local on the managed daemon only.
substituters = https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59...  # from descriptor (plan 02 §6.5/§7)
connect-timeout = 10
max-substitution-jobs = 4
max-jobs = 1                         # bounds concurrent derivations for ONE client/connection
                                     #   (NOT a CPU/mem/IO cap). The broker adds a FAIR IN-PROCESS
                                     #   build-admission mutex/queue on top of this (§5.4).
cores = 0                            # only supplies the NIX_BUILD_CORES cooperation hint
max-silent-time = 3600               # daemon-enforced; terminate a builder; per-call may only
timeout = 86400                      #   TIGHTEN these. NOT memory/CPU/IO caps. Per derivation.
max-build-log-size = 268435456       # 256 MiB log bound: KILLS the builder and NEVER truncates
```

```ini
# macOS — <daemon-state>/nix.conf  (root-owned, mode 0644, rendered from the
# signed channel descriptor; only this exact key set is emitted).
build-users-group = nixbld            # group `nixbld` (created by installer); build users `_nixbld1..N`
trusted-users = root                  # ONLY root is trusted. In Nix 2.34.8 trusted-users are
                                     #   effectively root-equivalent, so the broker is NEVER trusted.
allowed-users = pkg-nix-broker        # the unprivileged dedicated broker is the ONLY permitted
                                     #   client; ordinary users are not listed and never reach the socket.
experimental-features = nix-command flakes   # `cgroups` OMITTED on macOS
sandbox = true                       # builds run sandboxed (Nix Darwin sandbox)
sandbox-fallback = false             # fail closed, never build unsandboxed
allow-import-from-derivation = false # pure evaluation: no realization / import-from-derivation before approval (BOTH platforms)
                                     # (use-cgroups is a LINUX-ONLY setting; OMITTED here, never `false`)
require-sigs = true
builders =                           # EMPTY on BOTH platforms: NO remote/distributed builders in v1
                                     #   (D-11/INV-08); all builds are local on the managed daemon only.
substituters = https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59...  # from descriptor (plan 02 §6.5/§7)
connect-timeout = 10
max-substitution-jobs = 4
max-jobs = 1                         # bounds concurrent derivations for ONE client/connection
                                     #   (NOT a CPU/mem/IO cap). The broker adds a FAIR IN-PROCESS
                                     #   build-admission mutex/queue on top of this (§5.4).
cores = 0                            # only supplies the NIX_BUILD_CORES cooperation hint
max-silent-time = 3600               # daemon-enforced; terminate a builder; per-call may only
timeout = 86400                      #   TIGHTEN these. NOT memory/CPU/IO caps. Per derivation.
max-build-log-size = 268435456       # 256 MiB log bound: KILLS the builder and NEVER truncates
```

S5 (plan 00/11) must validate Linux **cgroup v2** and service readiness before
`use-cgroups` is accepted as build grouping/cleanup/accounting. On macOS there
is no cgroup equivalent.

**Evaluation purity & IFD.** Both blocks set `allow-import-from-derivation = false`:
all resolution/`BuildPlan` evaluation is **pure** (locked flake inputs whose
rev/`narHash` come from the signed descriptor; no `--impure`, no `--override-input`),
so `nix derivation show --recursive` realizes **nothing** before the user approves a
deterministic `BuildPlan` (plan 04 §5.1/§5.2.1). A derivation that requires
import-from-derivation is a hard refusal (`ACQUIRE_NO_BINARY`), never overridden by
approval.

The product **rewrites** this file atomically whenever the channel descriptor
changes (plan 02). Any user attempt to edit it is detected by `doctor`
(checksum); **ordinary broker operations stay blocked (exit 78 `CONFIG`) until
integrity is restored** by an **explicitly signed installer/runtime-maintenance
`nix.conf` reconciliation command** (a rerun of the installer's
render-from-descriptor step, gated by the signed channel descriptor) — **not**
the bare store-path `pkg repair` (§7.4, plan 05 §10), which fixes corrupt NAR
store paths and never touches `nix.conf`. This is the trust enforcement point (plan 02/08).

The following are deliberately **absent / not user-settable** on both platforms:
`extra-substituters`, `trusted-substituters`, `builders-use-substitutes`. (`builders` is rendered **explicitly empty** on both platforms — no remote/distributed builders in v1, D-11/INV-08.)

### 5.3 Daemon

The bundled Nix runs as a **daemon** (`nix-daemon`):

- **Linux:** a systemd service `pkg-nix-daemon.service` running as **`root`**
  (the only `trusted-users` entry), listening on the canonical Unix socket
  `/nix/var/nix/daemon-socket/socket` (plan 01 §9.1; configurable via
  `NIX_DAEMON_SOCKET_PATH`). Because the installer refuses any unmanaged Nix
  first (§8 / D-04), `/nix/var/nix` is exclusively ours, so the standard
  socket path is safe; the store-prefix spike (§6.2 deliverable #4) confirms
  co-location is collision-free. **The socket is provided by `systemd` socket
  activation** owned `root:pkg-nix-broker` mode **`0660`**, with the parent
  `daemon-socket` dir `root:pkg-nix-broker` `0750` (Nix 2.34.8's self-created
  socket mode is hard-coded `0666` and **not** a `nix.conf` knob, so we own the
  socket via the unit instead). **Only the unprivileged singleton broker
  (§7.4)** — running as the dedicated `pkg-nix-broker` user — connects to
  **this** socket. The root helper's fixed repair path opens the exclusively
  managed local store directly because the Nix 2.34.8 daemon protocol does not
  implement `repairPath`; it never accepts a store selector. Ordinary user processes are not in the
  `pkg-nix-broker` group and cannot traverse/connect (I7). Build users: a `nixbld` group +
  `nixbld1..N` users created by the installer (*confirmed multi-user model* [^multi-user]).
- **macOS:** a `launchd` daemon
  (`system` domain `org.pkg.nix-daemon.plist`) for the same role, running as
  **`root`**. There is **no systemd-style socket activation** on macOS, so Nix
  **self-creates** the daemon socket — whose mode is hard-coded `0666` and is
  **not** a `nix.conf` knob. The enforced boundary is the **parent
  `daemon-socket` directory**, which the installer creates `root:pkg-nix-broker`
  `0750`; combined with `allowed-users = pkg-nix-broker`, ordinary users (not in
  the broker group) cannot traverse or connect. macOS builds run through Nix's
  macOS sandbox under the `nixbld` group / `_nixbld*` build users created by
  the installer (D-11); the daemon is configured with `sandbox=true` and
  `sandbox-fallback=false`, and `pkg` fails closed if sandbox or build-user
  readiness cannot be verified. Nix's macOS sandbox uses different, generally
  narrower platform primitives than Nix's Linux namespace/chroot sandbox (Nix
  implements its own sandbox; it does **not** invoke bubblewrap) —
  `pkg` never claims identical isolation (§12, Q7.4). Substitution is still
  tried first and preferred on every install.

The product's CLI never talks to the raw Nix daemon socket; it goes through
`pkg`'s **unprivileged singleton broker** (§7.4; the broker/helper boundary is
**accepted** — the framed public broker RPC/peer-auth/capability schemas are the
next required milestone), which is the sole client of the managed daemon and the
**bundled-Nix-CLI spawner**. The broker configures Nix via the generated
`nix.conf` + env at the adapter call site (plan 04). Neither the CLI nor user
processes rely on a system `$PATH` `nix`. The **bundled Nix CLI is installed
group-executable by `pkg-nix-broker` only** (not world-executable): the broker
spawns it for all normal build/substitute/`verify`/`gc` ops, and the **root
helper executes it only for the fixed `--store local` repair path** (as root,
because the daemon protocol does not implement `repairPath`); ordinary
users cannot execute it at all.

### 5.4 Process & resource limits (recap from plan 04 §8)

Configured via the root-owned `nix.conf` (§5.2) + per-call flags. V1
product-managed finite defaults: `max-jobs = 1` (bounds concurrent derivations
for **one client/connection**, **not** a CPU/mem/IO cap, and does **not** by
itself serialize multiple users — so the singleton broker holds a **fair
in-process build-admission mutex/queue**, separate from per-user state leases;
see below), `cores = 0` (only the `NIX_BUILD_CORES` cooperation hint),
`max-silent-time = 3600` s, `timeout = 86400` s **per derivation**, and
`max-build-log-size = 268435456` bytes (256 MiB; it **kills the builder and
never truncates**). Per-call
`timeout`/`max-silent-time` may only **tighten** these. `timeout`/
`max-silent-time`/`max-build-log-size` **terminate a builder**; they are
**not** memory/CPU/IO caps. **Stock Nix 2.34.8 provides no per-build
memory/CPU/IO cap:** the Linux `use-cgroups` setting (experimental feature
`cgroups`, rendered into `nix.conf` on **Linux only**; **omitted entirely** on
macOS, never set to `false`) is **per-build process grouping**, **lingering-
process cleanup**, and **CPU accounting/statistics** — **not** security
isolation and **not** a resource cap (Nix writes no `memory.max`/`cpu.max`/
`pids.max`/IO knobs). macOS has no cgroup equivalent. Preflight additionally
checks disk headroom, free space, and load (the load ceiling is
`build.max_loadavg`, default **2 × logical CPU count** — a preflight signal,
not an ongoing cap).

**Service-manager ceilings are distinct and both remain Pending defense-in-depth,
not accepted enforcement:**

- **systemd (Linux):** `MemoryMax`/`TasksMax`/`CPUQuota` would be an
  **aggregate service-cgroup ceiling over the daemon plus all descendants**
  (daemon + every builder), shared across the whole unit subtree — not a
  stable per-build control.
- **launchd (macOS):** `SoftResourceLimits`/`HardResourceLimits` are
  **inherited per-process RLIMIT ceilings** (e.g. `CPU`/`Data`/`FileSize`/
  `NumberOfFiles`/`NumberOfProcesses`/`ResidentSetSize`/`Stack`; there is no
  `AddressSpace` key in `launchd.plist`), **not** an aggregate daemon-subtree
  ceiling, and several keys are advisory or alter system `sysctls` for system
  daemons (which can be dangerous system-wide).

Both are **Pending** pending real managed-host behavioral evidence
(S5/DR-005); they are **not** lumped together as a single coarse limit and are
**not** presented as accepted enforcement. See plan 04 §8.

**Broker-internal build admission (no backing-file `flock`).** Because
`max-jobs=1` is enforced per client/connection and does **not** by itself
serialize local builds across users, the **singleton broker** holds a single
fair **in-process mutex/queue** for approved local builds — **not** a
machine-global backing-file `flock`. A per-operation `flock` cannot be relied on
inside one broker process (notably on macOS), so the mechanism is in-process.
Ordinary users never touch it; they request admission over the broker's
peer-authenticated socket (§7.4). A second operation that wants to build waits in
the fair queue (or cancels); after the broker grants admission it revalidates
approval, the `readiness` schema (sandbox/build-user/cgroup), and the volatile
disk/free-space/load preflight. Immediately before local-build execution the
broker recomputes the exact derivation/readiness `BuildPlan` and compares its
digest to the approved one, then re-measures disk/free-space/load **outside** the
digest; on a threshold failure it performs **exactly one immediate recheck** and
then **fails closed** (`PREFLIGHT_FAIL`) — no retry loop thereafter. On digest
mismatch or failed preflight it fails/re-prompts as specified and releases
admission on all exits. (Per-user **state** leases in plan 05 §12 remain
filesystem `flock`s held in the user's own process — distinct from this in-broker
build admission.)

**Broker-internal GC admission (counted RW gate).** Because `nix store gc` can
delete paths a concurrent substitute/build/realization depends on, the broker
guards GC with a single fair **in-process read/write gate** — again **not** a
backing-file `flock` and **not** per-operation `flock` inside the broker on
macOS. Every operation handle **acquires shared (GC-inhibit) BEFORE any
substitute / build / realization** and **holds it through durable GC-root
publication** by the root helper (§7.4) — or through a clean abort. A
broker-mediated `gc` obtains the **exclusive** slot: it waits for all shared
holders to drain, then runs liveness-respecting `collectGarbage`. (Nix 2.34.8
allows the unprivileged broker's normal build/substitute, its read-only verify,
and `collectGarbage`, but the daemon protocol **does not implement `repairPath`
even for root**; store repair is therefore a fixed, narrow local-store
root-maintenance op run by the helper — §7.4 — whose **approved rebuild**
(`build`) mode still acquires this gate's shared GC-inhibit and the broker build
mutex.) GC-inhibit is released only after the root helper confirms the
durable roots exist, never before. **All product GC is explicit and
broker-mediated**: the installer schedules/installs **no** automatic-GC `systemd`
timer or `launchd` job, and there is **no** `nix.conf` auto-GC setting to invent.
External-Nix residual roots remain a sole-manager concern (§11).

## 6. Store, prefix, and the `/nix/store` spike (§9)

### 6.1 Store layout

- Store root: **`/nix/store`** (stock Nix; I2).
- Daemon state (profiles, gcroots, temproots, db): `/nix/var/nix/...` per
  multi-user layout [^multi-user], or a product-scoped equivalent if the spike
  allows; otherwise the standard paths.
- **Product GC roots (per-user D-17/ARCH-INV-04; per-output D-18):** the broker
  **selects** the outputs, and the **root helper (sole root-set filesystem
  writer)** creates **one GC root per selected output** (not one per generation)
  under `/nix/var/nix/gcroots/pkg/users/<uid>/`, scoped to the caller's uid,
  **before** the `current` swap (§7.4). The broker's GC-inhibit (§5.4) is held
  across this publication. The Nix collector scans `/nix/var/nix/gcroots`
  [^gc-roots]. Detail & topology in plan 05 §8.3.
- **Product state split (D-17/INV-10; canonical layout plan 01 §9.2/§9.3):**
  - **Machine-global SERVICE state** (root-owned, shared, read-only to users)
    under `/var/lib/pkg/` (Linux) / `/Library/Application Support/pkg/`
    (macOS): `channel/` (TUF + descriptor), `index/<seq>/`, `nixpkgs/<rev>/`,
    `cache/`, `log/`. This is the runtime/channel/index/source service —
    **not** package environment state. **One deliberate exception:** the raw
    adapter/Nix log and authority-audit dir `log/broker/` (i.e. `/var/lib/pkg/log/broker` on Linux,
    `/Library/Application Support/pkg/log/broker` on macOS) is **not** part of
    the root-owned service tree — it is **owned and writable only by the
    unprivileged `pkg-nix-broker` account** (directory `0700`, files `0600`,
    **no access to any user**, including other service accounts; its allowlisted
    hash-chained `approvals.ndjson` is service-private authority evidence, while raw adapter logs
    remain separate files. The directory is created that
    way by the installer, §7.2/§7.3). Only **sanitized, versioned NDJSON** may
    leave it into a caller's `<user-state>` (§7.4/§12, plan 04 §10).
  - **Per-user authoritative package state** `<user-state>` (owned by the
    invoking uid, mode 0700): `manifest.json`, `lock.json`, `generations/`,
    `current` (relative symlink → `activations/gen-<id>/`, D-18),
    `activations/gen-<id>/` (the **symlink forest** `pkg` materializes outside
    `/nix/store` — entries point at `/nix/store` targets or approved sources;
    activation invokes no Nix and is bound by `treeDigest`), `journal/`,
    `cache/`, `log/`. Linux: `$XDG_DATA_HOME/pkg/`
    (default `~/.local/share/pkg/`); macOS: `~/Library/Application Support/pkg/`.
    A root-owned fallback `/var/lib/pkg/users/<uid>/` is used for accounts
    without a usable HOME (plan 01 §9.3, plan 05 §4). `<user-state>` is **not**
    created by the installer; each user's CLI lazily creates their own on first
    run (§7.2).

### 6.2 Required spike — store prefix & relocatability

**Statement of fact (current Nix):** the Nix store path (`/nix/store`) is
compiled into Nix; stock Nix is **not relocatable** to an arbitrary prefix.
The official multi-user installer creates `/nix` owned by root with the store
under it [^multi-user]. There is **no supported relocatable-store mode** in
stable Nix today (experimental/patched variants exist but are not a product
dependency).

**Spike deliverables (must complete before irreversible installer work; see
plan 11 critical path):**

1. Confirm the chosen bundled-Nix build uses `/nix/store` verbatim.
2. Confirm the product binary, state, and logs can live under a separate
   prefix (e.g. `/opt/pkg`, `/var/lib/pkg`) while Nix uses `/nix/store` — i.e.
   we depend on `/nix/store` existing but do **not** require our own binary to
   be inside it.
3. Confirm `/nix` ownership semantics: root-owned `/nix/store`, group-readable
   by the daemon's build users; no world-writable paths.
4. Decide whether to co-locate the daemon socket under `/nix/var/nix` or a
   product prefix (prefer product prefix to avoid colliding with any
   unmanaged Nix's `/nix/var/nix`).
5. Decide single-user vs multi-user install (recommendation: **multi-user with
   daemon** on both OSes for uniformity and sandbox support) — see §7.

**Go/no-go:** if the spike finds stock Nix cannot satisfy (1)–(3) without
custom patches, escalate to plan 12 and descope/defer; do **not** ship a
custom-patched Nix in v1 without security review (plan 08).

## 7. Installers (Linux & macOS)

### 7.1 Install topologies (decision: multi-user w/ daemon)

| Topology | Linux | macOS | Chosen? |
|----------|-------|-------|---------|
| Multi-user (daemon, root-owned store) | ✓ systemd | ✓ launchd | **YES (v1)** |
| Single-user (no daemon, user-owned store) | possible | possible | no — divergent trust/sandbox; revisit Q7.5 |

### 7.2 Linux installer

**Invocation:** `curl ... | sh` style or distro packages (deb/rpm) built in
release (plan 10). The installer binary is **not** setuid; privilege is
obtained via an explicit `sudo`/polkit prompt for the install steps only.

**Steps (idempotent; re-runnable):**

1. **Preflight:** arch/system OK; root/sudo available; **unmanaged-Nix scan**
   (§8) — if found, print remediation and exit 74; never modify.
2. Create the **`pkg-nix-broker`** unprivileged service user/group (the
   broker's identity; in `allowed-users`, never `trusted`) and the `nixbld` group +
   `nixbld1..16` build users (multi-user build isolation) [^multi-user]. The
   daemon itself runs as **root**.
3. Create `/nix` (root:root `0755`) and `/nix/store`, `/nix/var/nix/...`.
4. Extract bundled Nix to `/opt/pkg/nix` **root:pkg-nix-broker**, dir and
   binaries group-executable/traversable by `pkg-nix-broker` only (**not**
   world-executable/traversable — the broker can spawn the bundled Nix CLI;
   ordinary users cannot). Install the `pkg-nix-daemon.socket` unit (socket
   activation, owned `root:pkg-nix-broker` `0660`, parent `daemon-socket` dir
   `root:pkg-nix-broker` `0750`) and `pkg-nix-daemon.service` (runs as `root`)
   pointing at the socket and `nix.conf`; install the broker unit
   `pkg-nix-broker.service` running as `pkg-nix-broker`.
5. Write root-owned `nix.conf` from the **bundled channel descriptor** (plan
   02) — installer ships an initial descriptor+signature for bootstrapping.
6. Enable + start the daemon; `nix ping-store` health check [^ping-store].
7. Install the `pkg` binary to `/usr/local/bin/pkg` (or `/opt/pkg/bin`) and
   create the **root-owned service root** `/var/lib/pkg` (channel/index/source/cache/log).
   The root and `log/` ancestor are `root:pkg-nix-broker` mode `0710`: the broker
   receives search-only traversal to its daemon socket and private log leaf, but
   cannot list either ancestor. All other root-owned subtrees remain inaccessible;
   carve out the **broker-owned** raw-log dir `/var/lib/pkg/log/broker` owned
   `pkg-nix-broker:pkg-nix-broker` mode `0700` (files `0600`) — the **only**
   non-root-owned path in the service tree (§6.1/§7.4). **Per-user authoritative
   state** `<user-state>` (D-17) is **not** created by the installer; each user's
   CLI lazily creates their own `<user-state>` (owned by that uid, mode 0700) on
   first run.
8. **PATH integration (§10):** write `/etc/profile.d/pkg.sh` and shell-rc
   snippets; print next-step instructions.
9. Write an **uninstall manifest** (`/opt/pkg/uninstall/manifest.json`) listing
   every path/user/unit created, for total uninstall (§11). After all artifacts and services
   verify, atomically install the root-owned managed-Nix ownership receipt at
   `/var/lib/pkg/managed-nix/ownership-v1.json` (parent `0700`, file `0600`). Its digest and
   complete static privileged-install artifact list must come from authenticated release/channel metadata, never by
   rediscovering and blessing whatever happens to be on disk.

**Unattended:** `pkg-install --yes --target linux` for CI/containers; still
performs the unmanaged-Nix scan and refuses if found.

### 7.3 macOS installer

**Form:** a `.pkg` installer (or a script) run interactively; uses an
AuthorizationServices prompt (or `sudo`) for the privileged steps.

**Steps:**

1. Preflight: arch (`aarch64-darwin`/`x86_64-darwin`); **unmanaged-Nix scan**
   (§8) including `/nix`, `~/.nix-profile`, `~/.nix-defexpr`, Homebrew's
   `nix`, `launchctl list | grep nix`, `/Library/LaunchDaemons/org.nixos.*` —
   refuse with remediation if found.
2. Create the **`pkg-nix-broker`** unprivileged service user/group. Before
   touching `/nix`, provision a product-owned, encrypted, ownership-enabled
   APFS volume mounted at `/nix`: merge only the exact `nix` entry into
   `/etc/synthetic.conf`, keep the generated unlock secret in the System
   keychain with root-only access, and journal the volume UUID, keychain item,
   prior synthetic-file state, and mount state for exact rollback. Publish the
   dynamic UUID plus fixed keychain selector in the separate root:wheel `0600`
   `/Library/Application Support/pkg/managed-nix/store-volume-v1.json`; the
   authenticated ownership receipt remains the static artifact claim and never
   attempts to authenticate host-generated bytes. A fixed `org.pkg.store-volume` launchd job invokes only the root
   helper's closed `--mount-store-volume` verb; no secret or UUID is placed in
   plist argv or logs. Then create `/nix/store`, `/nix/var/nix/...` (stock Nix
   still uses `/nix/store`) with the **`daemon-socket` parent dir `root:pkg-nix-broker`
   `0750`** (Nix self-creates the socket `0666` on macOS; traversal +
   `allowed-users = pkg-nix-broker` is the boundary — there is no socket
   activation on macOS); create the `nixbld` group + `_nixbld1..N` build users
   (multi-user build isolation, [^multi-user]); verify the host's native
   toolchain (Xcode/Command Line Tools) is present for local builds.
3. Extract bundled Nix to `/opt/pkg/nix` **root:pkg-nix-broker**, group-
   executable/traversable by `pkg-nix-broker` only (broker spawns it; ordinary
   users cannot).
4. Install `org.pkg.store-volume.plist`, `org.pkg.nix-daemon.plist` (daemon
   waits for the mounted store, runs as **root**, substitutes/builds via
   `_nixbld`), `org.pkg.root-helper.plist`, and the **broker** launchd job (runs
   as `pkg-nix-broker`) into `/Library/LaunchDaemons` and bootstrap them in the
   system domain.
   `pkg-root-helper` is `root:wheel` `0700`; the broker cannot execute it and
   reaches only its private socket. The mount verb independently requires uid 0
   plus the root-only ownership receipt and accepts no UUID, path, keychain
   handle, or secret from argv.
   The shipped service binaries accept the launchd `--serve-macos` verb exactly:
   the broker requires the installed broker uid and primary gid, while the helper
   requires uid 0 with that broker gid. They validate the exact root-owned managed
   socket directory chain with no extended ACLs, replace only an exact stale socket
   owned by the expected service identity, create only the compiled path/mode without
   a pathname-based chmod window, and then use kernel peer
   credentials plus bounded framing. Unknown verbs, extra arguments, and custom
   paths fail closed. The `--mount-store-volume` path separately requires
   root:wheel, reads only the bounded root-only volume record, verifies UUID/name/
   encryption/ownership, streams the System-keychain secret directly to `diskutil`
   stdin, applies a 30-second process-group deadline, and verifies the final `/nix`
   mount.
5. Write root-owned `nix.conf` (`trusted-users = root`, `allowed-users =
   pkg-nix-broker`, `sandbox=true`, `sandbox-fallback=false`,
   `build-users-group=nixbld`; substituters/keys from descriptor) — the complete
   block in §5.2.
6. Install `pkg` to `/usr/local/bin/pkg` (or `/opt/pkg/bin`); create the
   **root-owned service root** `/Library/Application Support/pkg` mode `0711`
   (search-only for ordinary users, so they can reach only the known public
   broker endpoint). Its `run/` ancestor is `root:pkg-nix-broker` `0751`, the
   public `run/broker/` leaf is `root:pkg-nix-broker` `0771`, and the broker
   socket is `0666`; callers can traverse/connect but cannot list or replace
   the endpoint. The private `run/helper/` leaf remains
   `root:pkg-nix-broker` `0750` with a `0660` helper socket. Carve out
   the **broker-owned** raw-log dir `/Library/Application Support/pkg/log/broker`
   owned `pkg-nix-broker:pkg-nix-broker` mode `0700` (files `0600`) — the
   **only** non-root-owned path in the service tree (§6.1/§7.4).
7. PATH integration (§10); uninstall manifest (including the exact APFS volume,
   System-keychain item, synthetic entry, mount record, and mount job); then, only after the complete artifact set
   verifies against authenticated release/channel metadata, atomically install
   `/Library/Application Support/pkg/managed-nix/ownership-v1.json` with a root-owned `0700`
   parent and `0600` file.

**macOS build readiness:** the macOS daemon runs with `sandbox=true`/
`sandbox-fallback=false` and the `nixbld` group / `_nixbld*` users created at install
(D-11). Substitution is tried first; on a cache miss an **approved** native
sandboxed build is permitted. A build that is impossible or disallowed
(unsupported/broken/impure derivation, or sandbox/build-user unavailable, or
`buildPolicy` denies the system) yields `ACQUIRE_NO_BINARY` and never runs,
even with approval (plan 04/06). Plan 04's **full-closure cache preflight**
still classifies **every** path in the recursive closure against
`cache.nixos.org` before any acquire — now as an **availability signal** (it
tells the user up front which paths will substitute vs. build and surfaces
disallowed builds early), not as binary-only enforcement. `pkg info`/`pkg
install --dry-run` surface this up front so users never reach a partial
activation. macOS builds need the host's native toolchain (Xcode/Command Line
Tools); the installer verifies its presence and `doctor` reports it.

### 7.4 Privilege & daemon protocol

- The **product CLI runs as the invoking user** and never connects to the raw
  Nix daemon socket and **never invokes the root helper directly**. All general
  Nix-side work — substitution, build admission, `verify`, liveness-respecting
  `gc`, and per-output GC-root *selection* — is mediated by `pkg`'s
  **unprivileged singleton broker** (the broker/helper boundary is **accepted**;
  the framed public broker RPC/peer-auth/capability schemas are the next required
  milestone). The broker runs as the dedicated **`pkg-nix-broker`** user, is the sole client of the managed `nix-daemon`, and is the **bundled-CLI spawner** for normal operations. The root helper invokes the same pinned CLI only for fixed local-store maintenance below and never connects to the daemon.
  It is `allowed-users` but **never** `trusted-users`; in Nix 2.34.8 trusted-users
  are effectively root-equivalent, so the daemon accepts the broker's normal
  build/substitute requests, its **read-only `nix store verify`**, and
  `collectGarbage`, but **rejects the mutating `nix store repair`** (I7).
- **Peer-authenticated privilege boundary (D-17/ARCH-INV-06):** the broker
  authenticates each calling user via socket peer credentials (`SO_PEERCRED` on
  Linux; `getpeereid` on the launchd-managed Unix transport on macOS), and the **root helper
  re-checks the broker-authenticated caller** — the helper is reachable **only**
  from the broker over a **private closed channel**, never from the CLI. The
  helper is **non-setuid** and is the **sole root-set filesystem writer**. The
  privileged store-side writes it performs on a caller's behalf are: (a) **one GC
  root per broker-selected output** (D-18) under that user's own
  `/nix/var/nix/gcroots/pkg/users/<caller-uid>/`, never under another uid's dir,
  never reading/mutating another user's `<user-state>` (plan 01 §8, plan 05 §8.3);
  (b) the fixed repair path below; and (c) root-owned config/asset writes
  (`nix.conf` rewrite, install, uninstall, daemon/broker restart). Authoritative
  package state (manifest/lock/generations/activation/journal) is per-user and
  never globally shared (INV-10).
- **Store repair is split into a read-only verify and a mutating repair.**
  `nix store verify` is **read-only**; the broker runs it itself (allowed, not
  trusted) to detect corrupt/missing paths. `nix store repair` is the **separate
  mutating** command. The daemon rejects it from the untrusted broker, and the
  Nix 2.34.8 remote-store protocol also reports `repairPath is not supported by
  store 'daemon'` for root. The **helper/service therefore runs the fixed command
  as root with `--store local`** against the exclusively managed store. Under
  the hood `Store::repairPath` first **substitutes**, and
  only on a cache miss with a valid deriver may **rebuild all outputs** of that
  derivation — so the helper bounds that rebuild explicitly across the fixed
  phases below: **Phase 0** (read-only `nix store verify`), **Phase A**
  (cache-only), and **Phase B** (approved rebuild fallback) (detailed Phase 0/A/B
  design in plan 05 §10). The CLI never calls the helper; it asks the broker,
  which sponsors an **opaque maintenance capability** the helper validates
  server-side.
- **The helper never accepts broker-selected raw `StorePath`s, argv, or
  options.** Repair is authorized by a single **helper-issued, expiring,
  single-use maintenance capability** that the helper **binds server-side** to:
  the **caller UID**; an **existing pkg-owned rooted generation/closure** (the
  target must be part of a GC root pkg owns under
  `/nix/var/nix/gcroots/pkg/users/<caller-uid>/`); the **exact typed path set**,
  validated server-side to be **members of the exact stored closure reachable
  from this caller uid's existing pkg-owned rooted generation** — **not** merely
  a subset of `activation.outputRoots` (corruption can live in a transitive
  dependency that is not itself a selected output root). The helper resolves the
  generation/root identities server-side from the capability and caller uid
  using **fixed store queries only** — it accepts **no** broker/public raw path
  input, no installables, and no store-root enumeration (the broker identifies
  the generation and may name a typed path set within its closure, but the
  helper validates membership itself and never accepts raw/unvalidated paths).
  A target may be **registered/expected but currently missing on disk** after a
  failed repair; absence does not revoke closure membership, so such a target is
  never rejected simply for being absent; the **`RepairBuildPlan` digest
  covering ALL outputs** Nix may rebuild
  (plan 05 §10.4; so a deriver fallback cannot rebuild outside the approved
  scope); a **`policyVersion`**; and a **mode** (`cache-only` or `build`).
  Stale, replayed, mismatched, or cross-UID capabilities **fail closed**; the
  exact RPC framing/schema is the next required milestone (the broker/helper
  boundary is **accepted**). The helper accepts **no** installables,
  flake/expression targets, derivation targets, arbitrary argv, options, or
  trust knobs, and it does not mutate `trusted-users`/`allowed-users`; it uses
  the already root-owned, channel-pinned `substituters`/`trusted-public-keys`
  from `nix.conf` (§5.2), never broker-supplied substituters/keys.
- **Phase A — cache-only repair (automatic on a cache hit; no approval; plan
  05 §10.3).** The helper runs `nix store repair` as root, **one
  path at a time**, with **`max-jobs = 0`** (blocks local build) **and `builders
  =` empty** (blocks remote/distributed build), using the managed pinned
  substituters/keys (never per-call flags). The op handle holds a **shared
  GC-inhibit permit** (§5.4) across the phase. With both build paths blocked, a
  **cache hit substitutes and repairs automatically** with no user prompt; a
  cache miss with a valid deriver is **blocked** (`max-jobs=0` + empty
  `builders`) and the helper **stops before any build**.
- **Phase B — approved rebuild fallback (explicit approval mandatory; plan
  05 §10.4/§10.5).** When a path cannot be cache-repaired (miss + valid
  deriver), the helper stops at the **Phase B preview**; the ordinary **public
  preview + explicit approval** flow (plan 04/05/06) is **mandatory** — there is
  no automatic rebuild. The preview's **`RepairBuildPlan` digest covers ALL
  outputs** Nix may rebuild for that deriver. Once approved, the broker holds the
  **machine build mutex** (§5.4 build admission) **and a shared GC-inhibit
  permit** (§5.4 GC gate) for the duration, uses **no remote builders**
  (`builders =` empty), and the root helper runs local `nix store repair` with a
  **bounded nonzero `max-jobs`** (call-site override of the managed default).
- **`pkg repair` is explicitly user-invoked and never atomic.** `pkg repair` is
  an **explicit** user action; before it starts it warns that **during repair
  the affected commands can be missing or observe partial/mismatched content**
  while a target path is mid-fix (users are warned), and asks for confirmation.
  Verified Nix 2.34.8 `LocalStore::addToStore(Repair)` (the store-repair path)
  is **non-atomic**: **cache repair deletes the live real store path, restores
  the replacement NAR directly into that same path, then
  hashes/validates/canonicalizes/registers it** (the pre-existing database
  record may still say `valid` during this window), and **local rebuild repair
  moves the old path aside before replacing it**; a crash, power loss, or
  killed builder between the delete/move-aside and the completed restore can
  therefore **leave a registered, closure-member path absent OR partially
  restored (effectively corrupt) on disk**, while the path's GC root still
  references it. pkg **never claims atomicity** for
  repair and **never creates or swaps a generation** for normal store repair —
  the activation forest, generation swap, and `treeDigest` (D-18) are untouched
  by store repair; any per-forest recovery is handled **separately, in Rust,
  outside this root-maintenance path**. To bound the non-atomic window the
  helper journals **per path** an `intended → in-progress → post-verify` status
  record (under the broker/service-private `0700`/`0600` log dir) so a restarted
  or re-run repair knows exactly which paths still need work. **The final
  read-only `nix store verify` governs success**: the helper **does not claim a
  path repaired until that verify** (run by the broker) confirms it. A path
  still failing verify after repair is retried (cache-only) or escalated to the
  approved-rebuild preview; the broker never reports "repaired" for a path the
  final verify still fails.
- **Restart and re-run recovery.** Because Phase A (cache-only) repair needs
  **no** user approval, the helper **automatically retries only cache-only
  repair** of any still-failing/missing paths on broker/service restart and on
  the next `pkg repair`/`pkg doctor` pass — it **never** silently re-runs a
  **local rebuild**. Repeating a **local (build) repair** requires a **fresh
  Phase B preview, explicit approval, and a fresh single-use maintenance
  capability**;
  a stale or already-consumed capability for a prior rebuild fails closed. An
  interrupted repair has **no clean "unavailable-only" guarantee**. After any
  crash/power loss/killed builder mid-repair, the affected generation/closure is
  **health = unknown/unhealthy**, and `pkg` **blocks success reporting and any
  further state-changing operation that relies on that closure** (install/switch
  activation, `gc` of its roots, broker build admission into it) until **broker
  startup recovery** performs a **content-aware read-only `nix store verify`**
  of the involved paths — which **retries Phase A cache-only repair per path**
  (a **local rebuild still requires a fresh Phase B preview, explicit approval,
  and a fresh single-use capability**); **only a final content-hash + trust
  verification clears the unhealthy state**, and until then a path that was
  mid-fix stays health=unknown. `pkg` does **not** relocate the repaired
  content: a command reachable from an already-configured PATH whose target is
  mid-repair can still be **executed directly and observe missing/partial
  content**, because the NAR content lives at a fixed `/nix/store` path — **this
  direct execution from an already-configured PATH is an explicit residual risk
  of delegating to Nix 2.34.8's non-atomic repair**. The **install/service
  manager** (systemd/launchd) recovering the broker/helper units does **not**
  itself run repair; it only restores the services so the broker can re-sponsor
  cache-only retries per the journal.
- **Raw Nix logs are confined and broker-owned.** Build/repair logs live
  **only** in the **broker-owned** `log/broker` directory
  (`/var/lib/pkg/log/broker` on Linux,
  `/Library/Application Support/pkg/log/broker` on macOS) — carved out of the
  root-owned service tree and **owned/writable only by the unprivileged
  `pkg-nix-broker` account** (directory `0700`, files `0600`, **no access to
  any user**; §6.1); only **sanitized, versioned NDJSON** may be copied into
  the caller's `<user-state>` (never raw logs).
- **Build & GC admission** (§5.4) are **in-process** to the singleton broker
  (a fair mutex/queue for builds; a counted RW gate where every op holds shared
  GC-inhibit across substitute/build/realization through durable-root
  publication or abort, and broker `gc` takes exclusive). There is **no**
  machine-global backing-file `flock` for either, and **no** automatic-GC
  timer/unit/launchd job is installed.

## 8. Unmanaged-Nix detection & refusal (V1 policy)

**Policy (plan 00):** V1 takes exclusive managed ownership; on detecting any
existing unmanaged Nix, **refuse** with manual remediation; **never
auto-delete.**

**Detection signals (any non-empty ⇒ refuse):**

| Signal | Linux | macOS |
|--------|-------|-------|
| `/nix` exists without authenticated pkg ownership proof | ✓ | ✓ |
| `/nix/store` non-empty with paths we didn't create | ✓ | ✓ |
| `/nix/var/nix/daemon-socket/socket` exists | ✓ | ✓ |
| a `nix`/`nix-daemon`/`nix-store` on `$PATH` not under our prefix | ✓ | ✓ |
| systemd unit `nix-daemon.service` (not ours) | ✓ | — |
| launchd label `org.nixos.nix-daemon` (not ours) | — | ✓ |
| `~/.nix-profile`, `~/.nix-defexpr`, `~/.nix-channels` | ✓ | ✓ |
| `$NIX_REMOTE`, `$NIX_PATH`, etc. set in login shell | ✓ | ✓ |
| existing `/etc/nix/nix.conf` not ours | ✓ | ✓ |

**Behavior:** `pkg doctor` (and every mutating verb) performs this scan first;
on a hit it prints:

```
error[UNMANAGED_NIX]: an existing Nix installation was detected:
  • /nix/store contains 12,431 paths not managed by pkg
  • systemd unit nix-daemon.service is active
  • /etc/nix/nix.conf exists
V1 manages its own Nix exclusively and will not modify or remove the existing
installation. To proceed:
  1) Back up any profiles/data you need from the existing Nix.
  2) Uninstall it using its own uninstaller (see <docs URL>).
  3) Remove /nix and the listed units/configs.
  4) Re-run: pkg doctor
```

The product **never** runs `rm -rf /nix` or stops/removes the foreign unit. No
`--force` flag bypasses this in V1 (flagged in plan 12 as a hard "no").

**Existing managed-install recognition:** a path, service label, broker configuration, or local
marker is only an ownership *claim*. Runtime/doctor may classify the installation as pkg-managed
only when all of the following hold:

1. The caller supplies an expected system, exact Nix version, asset-manifest digest, and complete
   static privileged-install artifact set from separately authenticated release/channel metadata.
   The manifest uses pkg's canonical versioned encoding; the verifier recomputes its SHA-256 and
   rejects a digest paired with any truncated, extended, or altered artifact list. Signed artifacts
   carry stable group roles (`root`, `broker`, `buildUsers`) rather than non-portable numeric gids;
   the privileged installer resolves those roles against the just-created/validated local service
   groups and records the resulting gid bindings in the root-owned receipt.
2. The fixed receipt is root-owned and root-only, with symlink-free non-writable ancestors:
   `/var/lib/pkg/managed-nix/ownership-v1.json` on Linux or
   `/Library/Application Support/pkg/managed-nix/ownership-v1.json` on macOS.
3. The strict versioned receipt matches that trusted expectation exactly (unknown fields,
   oversized input, duplicates, and out-of-scope paths fail closed).
4. Every declared file/directory/symlink matches type, root ownership, group, mode, and—where
   applicable—exact byte size, streaming SHA-256 digest, or symlink target. Parent-path escape and
   receipt replacement during verification fail closed.

Until PR-12 installs that receipt from signed inputs, even an otherwise healthy product-looking
Nix tree remains `nix_ownership_unknown`; this is intentional and carries no removal advice.
The receipt does not freeze an exact `/nix/store` inventory: store paths realized after install are
dynamic Nix/pkg state. Their exclusive origin follows from the privileged clean preflight performed
immediately before provisioning plus the product-private daemon/broker boundary; their integrity and
liveness are checked through Nix metadata, signatures/hashes, pkg state, and GC roots. Root-level
tampering remains outside the receipt's threat model and is handled as host compromise.

## 9. The store-prefix spike (consolidated)

(Specified in §6.2.) This is the **single highest-risk unknown** for the
substrate; it gates the installer design and is placed first on the PR
critical path in plan 11. Until it closes, all path choices here are
conditional.

## 10. Shell PATH integration

The active generation's activation **forest** exposes `bin/`, `sbin/`,
`share/man`, etc. as a Rust-materialized symlink forest under
`<user-state>/activations/gen-<id>/` whose entries point at `/nix/store`
targets or approved sources — **not** a Nix `buildEnv` store object (D-18;
activation invokes no Nix). `current` is a relative symlink to that forest.
PATH integration points each user's shell at their own
`<user-state>/current/bin` (plan 01 §9.3, plan 05).

**Per-user PATH (D-17):** each user's activation is their own
`<user-state>/current` → `activations/gen-<id>/` (a symlink forest, D-18), so
PATH integration points at the **invoking user's** own
`<user-state>/current/bin` — there is **no** single global
`/opt/pkg/profiles/default` symlink, because it could not represent per-user
activations.

**Mechanism:**

- **Linux:** a sourced `/etc/profile.d/pkg.sh` expands each user's own state
  dir:
  ```sh
  # managed by pkg — do not edit
  __pkg_state="${XDG_DATA_HOME:-$HOME/.local/share}/pkg"
  case ":$PATH:" in
    *":$__pkg_state/current/bin:"*) ;;
    *) PATH="$__pkg_state/current/bin:$PATH" ;;
  esac
  export MANPATH="$__pkg_state/current/share/man:${MANPATH:-}"
  unset __pkg_state
  ```
- **macOS:** `/etc/paths.d`/`path_helper` cannot expand per-user `$HOME`, so
  the installer writes a sourced login snippet (read by `/etc/profile` for bash
  and a `/etc/zprofile`-equivalent for zsh) that expands the user's own
  `~/Library/Application Support/pkg/current/bin`; `pkg shell-init` prints the
  same `eval` for interactive/non-login shells.
- **Interactive-shell rc:** `pkg shell-init` prints the right `eval` snippet
  for the current shell; `pkg completion <shell>` for completions (plan 06).
- `pkg doctor` verifies **the invoking user's** `<user-state>/current/bin` is
  on PATH and warns if shadowed by another tool earlier in PATH.

**`root`/system PATH:** the daemon does **not** need our bin on PATH (it uses
absolute paths to the bundled Nix).

## 11. Uninstall boundaries

**V1 policy:** uninstall is **explicit and total**. `pkg-uninstall` (a
separate binary or `pkg self-uninstall`) requires `--yes` and:

1. Re-reads the uninstall (asset) manifest `/opt/pkg/uninstall/manifest.json`
   (assembled from the managed-Nix + installer asset manifests recorded by
   plan 11 PR-12/27/28). The local manifest records only the target `system`,
   the authenticated managed-runtime manifest digest, and one exact entry for
   every compiled platform asset: stable `id` plus `created`/`preExisting`.
   It contains no deletion path, account name, unit name, argv, or option.
   PR-29 rejects missing, duplicate, unknown, or malformed ids and resolves all
   targets from the compiled Linux/macOS allowlists.
2. Refuses if any unmanaged-Nix signals are present that we didn't create
   (safety: don't touch a `/nix` someone else populated).
3. Stops & removes our daemon **and broker** units (systemd/launchd) — both
   `pkg-nix-daemon`/`pkg-nix-broker` on Linux, both plists on macOS.
4. Removes per-user GC roots under `/nix/var/nix/gcroots/pkg/users/*/`
   (D-17/ARCH-INV-04; all product roots live here — plan 05 §9).
5. Runs `nix store gc` once (plan 01 §11; plan 05 §9) to free our paths
   (best-effort; may leave paths if shared — but v1 is sole-manager so our
   paths are all of them).
6. Removes `/nix` **only if** it was created by us (manifest records
   creation) **and** is now empty of foreign paths; otherwise leaves `/nix`
   in place and prints a notice. (Conservative.)
7. Removes `/opt/pkg`, each user's `<user-state>`, the machine-global service
   state under `/var/lib/pkg` (Linux) / `/Library/Application Support/pkg`
   (macOS), the `pkg` binary, profile.d snippets, and the users & groups created
   by us (`pkg-nix-broker`, `nixbld*`/`_nixbld*`). It also removes **no
   automatic-GC timer/unit/launchd job** — none is ever installed (§5.4); any
   foreign GC scheduler found is left untouched.
8. Prints a final summary.

PR-29 implements this as a pure deterministic `plan_uninstall` preview followed
by a separate privileged execution step. Execution revalidates that the plan is
the exact plan for the manifest, checks privilege, authenticates the ownership
receipt and complete signed asset set, and repeats the unmanaged-Nix scan before
the first mutation. Stopping services is a hard barrier: if it fails, nothing is
removed. After that barrier cleanup is best-effort across every remaining exact
action, and the final action always checks for privileged residue. A failed
cleanup or residue check reports an incomplete uninstall; reinstall is the only
supported recovery path.

**Partial uninstall is intentionally unsupported** (I6). If the user wants to
keep packages but remove the product, that's not a V1 path (they'd lose the
manager) — documented as such.

**Uninstall does not** touch: Homebrew, the user's dotfiles beyond our managed
snippets, or any foreign `/nix`.

## 12. Security considerations (full model: plan 08)

- Installer & root helper are the highest-privilege code paths — minimal,
  audit-targeted, no network in the helper, fixed allowlist of subcommands. The
  helper is **non-setuid** and reachable **only** from the broker over a private
  closed channel; it is the **sole root-set filesystem writer** and the sole
  executor of the **fixed, narrow store-repair path**. It accepts **only an
  opaque helper-issued, expiring, single-use maintenance capability** bound
  server-side to caller UID + existing pkg-owned rooted generation/closure +
  server-derived typed path set (validated as members of the exact stored
  closure reachable from that caller uid's rooted generation, **not** a subset
  of `activation.outputRoots`) + internal plan digest (covering all outputs) +
  `policyVersion` + mode — **never** raw broker-supplied `StorePath`s/argv/
  options, and **no** installables/expressions/trust knobs. Cache-only repair
  runs with `max-jobs=0` + empty `builders` (builds blocked); an approved
  rebuild runs with bounded nonzero `max-jobs`, no remote builders, under the
  broker's build mutex + GC-inhibit. The CLI never invokes it directly.
- Raw Nix build/repair logs (and the per-path repair `intended → in-progress →
  post-verify` status journal) are **never** written to `<user-state>`; they
  live only in the **broker-owned** `log/broker` directory
  (`/var/lib/pkg/log/broker` on Linux,
  `/Library/Application Support/pkg/log/broker` on macOS) — carved out of the
  root-owned service tree and **owned/writable only by the unprivileged
  `pkg-nix-broker` account** (directory `0700`, files `0600`, **no access to
  any user**; §6.1/§7.4), and only **sanitized, versioned NDJSON** may be
  copied into a user's state.
  pkg **never claims store repair is atomic** (§7.4/§13): the helper journals
  per-path status, but a crashed/killed repair can leave a registered path
  **absent or partially restored**; `pkg` therefore marks the affected
  generation/closure **health=unknown/unhealthy and blocks** success and any
  further state-changing op that relies on it until broker startup recovery's
  content-aware read-only `nix store verify` (retrying cache-only per path; a
  local rebuild still needs a fresh preview/approval/capability) clears it with
  a **final content-hash + trust verification**. Direct execution of an
  already-PATH'd command mid-repair is an explicit residual risk (§7.4).
- `nix.conf` is root-owned and checksummed; tampering ⇒ refuse ops. It pins
  `trusted-users = root` and `allowed-users = pkg-nix-broker` exactly; `doctor`
  verifies these lines and flags drift.
- Build users (`nixbld*` on Linux, `_nixbld*` on macOS) are created with no
  login shell, no password, confined home; `sandbox=true`/
  `sandbox-fallback=false` on **both** platforms (D-11). `pkg` fails closed if
  sandbox or build-user readiness cannot be verified.
- The raw Nix daemon socket is **not exposed to ordinary users**; only the
  unprivileged broker connects to it (I7). On **Linux** systemd socket
  activation owns the socket `root:pkg-nix-broker` `0660` (parent dir `0750`);
  on **macOS** Nix self-creates a hard-coded `0666` socket, and the
  `root:pkg-nix-broker` `0750` parent dir + `allowed-users` is the enforced
  boundary. There is **no** blanket socket-mode `0600` claim (the hard-coded
  mode is `0666`, not a `nix.conf` knob). The broker's peer-authenticated socket
  is the only user-facing surface.
- The bundled Nix CLI is installed group-executable by `pkg-nix-broker` only
  (not world-executable): the broker spawns it for normal ops, the root helper
  executes it only for the fixed repair path, and ordinary users cannot.
- No `setuid` binaries anywhere (I5).
- macOS now shares the Linux build-time risk surface (T-BUILD-*), mitigated
  by the same sandbox/build-user/approval gates, with the honest caveat that
  Nix's macOS sandbox uses different, generally narrower platform primitives
  than Linux's (D-11); see plan 08.
- Uninstall is conservative about `/nix` to avoid destroying foreign data.

## 13. Failure matrix (selected)

| Scenario | Behavior |
|----------|----------|
| Unmanaged Nix present at install | Refuse (exit 74), print remediation, change nothing. |
| `/nix` not writable by daemon | Installer fails preflight; no partial state. |
| Broker/daemon unreachable at runtime | `doctor` fails that check; normal ops, broker-mediated `gc`, read-only `nix store verify`, and store repair all exit 79 `ENGINE_UNAVAILABLE` — the helper refuses repair without a fresh valid maintenance capability (only the reachable broker can sponsor one). |
| Store path corrupt/missing (repair needed) | The broker's **read-only** `nix store verify` detects it; the **mutating** `nix store repair` is daemon-rejected for the untrusted broker, so the helper runs it as root against an **opaque, expiring, single-use capability** (caller UID + pkg-owned rooted closure + a server-derived path set (validated as **members of the exact stored closure reachable from that caller uid's rooted generation**, not a subset of `activation.outputRoots`, via fixed store queries with no broker/public raw path input; a registered/expected target currently missing on disk is never rejected for absence) + plan digest covering all outputs + `policyVersion` + mode) — never raw broker `StorePath`s/argv/options. A **cache hit** repairs automatically (`max-jobs=0`, empty `builders`); a **cache miss + valid deriver** stops before build and requires the ordinary explicit approval; an **approved rebuild** runs with bounded nonzero `max-jobs`, no remote builders, under the broker build mutex + GC-inhibit, and is confirmed by a final read-only verify. |
| Mid-repair crash / power loss / killed builder | `pkg repair` is **explicitly user-invoked and never atomic**: cache repair deletes the live store path, restores the NAR into that same path, then hashes/validates/canonicalizes/registers (the DB record may still say `valid` during the window), and local rebuild repair moves the old path aside before replacement; a crash can therefore leave a registered closure-member path **absent OR partially restored (effectively corrupt)** while its GC root still references it — so **during repair the affected commands can be missing or observe partial content** (users are warned). The helper journals per-path `intended → in-progress → post-verify` status in the broker/service-private `0700`/`0600` log dir. After any interrupted repair the affected generation/closure is **health=unknown/unhealthy**: `pkg` **blocks success and any further state-changing op that relies on it** until **broker startup recovery** runs a content-aware read-only `nix store verify`, **retrying Phase A cache-only repair per path** (a **local rebuild is never silently retried** — it needs a fresh Phase B preview, explicit approval, and a fresh single-use capability); **only a final content-hash + trust verification clears the unhealthy state**. The install/service manager (systemd/launchd) recovering the units does **not** itself run repair; it only restores the services so the broker can re-sponsor cache-only retries per the journal. `pkg` does **not** relocate the repaired content, so direct execution of an already-PATH'd command mid-repair is an **explicit residual risk** of delegating to Nix 2.34.8's non-atomic repair. A final read-only `verify` governs success; store repair never creates/swaps a generation, and activation-forest recovery is separate and Rust-only. |
| User edits `/etc/nix/nix.conf` | `doctor` detects checksum mismatch; **ordinary broker operations stay blocked** (exit 78 `CONFIG`) until an **explicitly signed installer/runtime-maintenance `nix.conf` reconciliation command** reruns the render-from-descriptor step (gated by the signed descriptor) — **not** the bare store-path `pkg repair` (§7.4), which fixes NAR store paths and never rewrites `nix.conf`. |
| `nix.conf` trust knobs overridden via env | Ignored; `doctor` warns if env set. |
| Unsupported arch | Installer refuses with support-matrix link. |
| Rosetta-only Mac | Reported as `aarch64-darwin`; no x86_64-darwin fallback. |
| Uninstall with foreign paths in `/nix` | Leaves `/nix`, prints notice; removes only our files. |
| Daemon crash mid-op | Nix store is durable; product recovers via journal (plan 04/05). |
| macOS path impossible/disallowed to build (unsupported/broken/impure, or sandbox/build-user unavailable) | `ACQUIRE_NO_BINARY` (plan 04); never builds, even with approval. A buildable macOS cache miss offers the build preview instead. |

## 14. Dependencies on other plans

- **plan 00** — V1 exclusive-management & no-auto-delete decisions; arch
  matrix.
- **plan 01** — layered architecture; where the daemon/adapter live.
- **plan 02** — channel descriptor supplies Nix version/hash, substituters,
  keys; installer bootstrap descriptor.
- **plan 03** — index cached under the machine-global service state
  `/var/lib/pkg/index/<seq>/` (root-owned, shared; plan 01 §9.2, plan 03 §7) —
  **not** per-user.
- **plan 04** — pipeline uses the daemon socket, `nix.conf`, build resources
  defined here.
- **plan 05** — `<user-state>` paths and GC root topology referenced here.
- **plan 06** — `doctor` surfaces this plan's checks; `completion`/PATH.
- **plans 08–10** — installer/helper threat model, platform test lanes, release
  packaging (deb/rpm/pkg).

## 15. PR-shaped implementation checkpoints

- **PR-P0 — Store-prefix spike (research + report).** No code; documents
  findings vs §6.2 deliverables; go/no-go. *Gate for all PR-Pn below.*
- **PR-P1 — Arch/system detection + unmanaged-Nix scan library.** *Acceptance:*
  each detection signal has a fixture; scan is read-only.
- **PR-P2 — Bundled-Nix packaging + generated `nix.conf` writer.** *Acceptance:*
  daemon starts and `nix ping-store` ok in a container/VM.
- **PR-P3 — Linux installer (systemd, build users, paths, manifest).**
  *Acceptance:* clean install on a fresh VM; re-run is idempotent; unmanaged
  fixture → refuse; `pkg-nix-daemon.socket` (`root:pkg-nix-broker` `0660`,
  parent `0750`) + `pkg-nix-daemon.service` (root) + `pkg-nix-broker.service`
  (`pkg-nix-broker`) all present; bundled Nix CLI group-executable by
  `pkg-nix-broker` only.
- **PR-P4 — macOS installer (launchd, `.pkg`, `_nixbld` build users, paths).** *Acceptance:* clean
  install on fresh macOS VM; `_nixbld` build users + `sandbox=true`/
  `sandbox-fallback=false` verified; native toolchain (Xcode/CLT) check present;
  `daemon-socket` parent dir `root:pkg-nix-broker` `0750`; broker launchd job
  running as `pkg-nix-broker`; bundled Nix CLI group-executable by `pkg-nix-broker`
  only.
- **PR-P5 — Privileged root helper (fixed allowlist).** *Acceptance:* every
  subcommand unit-tested; no network in helper; helper reachable only from the
  broker over the closed channel; the **store-repair** path accepts **only** an
  opaque helper-issued, expiring, single-use maintenance capability bound
  server-side to caller UID + pkg-owned rooted generation/closure + a server-
  derived typed path set validated as **members of the exact stored closure
  reachable from that caller uid's existing rooted generation** (**not** merely
  a subset of `activation.outputRoots`), via **fixed store queries only** with
  no broker/public raw path input (a target registered/expected but missing on
  disk is never rejected for absence) + plan digest (all outputs) + `policyVersion`
  + mode, and **rejects** raw `StorePath`s/installables/expressions/argv/options/trust
  knobs; **cache-only** mode runs `max-jobs=0` + empty `builders` (cache hit
  repairs, miss+deriver stops before build); **approved rebuild** (`build`)
  mode runs bounded nonzero `max-jobs`, no remote builders, under broker build
  mutex +
  GC-inhibit; repair is **explicitly user-invoked and never atomic** (cache
  repair deletes the live store path, restores the NAR into that same path,
  then hashes/validates/canonicalizes/registers; local rebuild repair moves the
  old path aside before replacement — a crash can leave the path **absent or
  partially restored**), warns that **during repair the affected commands can be
  missing or observe partial content**, journals per-path
  `intended → in-progress → post-verify` status in the broker/service-private
  `0700`/`0600` log dir, and **never creates/swaps a generation** (activation-
  forest recovery stays separate and Rust-only); on restart the helper retries
  **only cache-only** repair of still-failing/missing paths and never silently
  re-runs a local rebuild (which needs a fresh preview/approval/capability),
  with the interrupted generation/closure held **health=unknown/unhealthy and
  blocked** until broker startup recovery's content-aware read-only `verify`
  clears it with a **final content-hash + trust verification** (direct execution
  of an already-PATH'd command mid-repair is an explicit residual risk); no
  path reported repaired before a final read-only `verify` governs success;
  stale/replay/mismatch/cross-UID capabilities fail closed; raw logs (and the
  per-path repair journal) confined to broker/service-private `0700`/`0600`
  (only sanitized versioned NDJSON reaches `<user-state>`); plan-08 audit
  checklist met. (Exact RPC framing/schema is the next milestone.)
- **PR-P6 — PATH integration (`profile.d`, `paths.d`, `shell-init`).**
  *Acceptance:* new login shell has `current/bin` on PATH; `doctor` verifies.
- **PR-P7 — Total uninstaller.** *Acceptance:* uninstall on a sole-manager
  host removes everything; with foreign paths, leaves `/nix` and notices.
- **PR-P8 — `doctor` wiring (plan 06 PR-C2) of all platform checks.**

## 16. Testable acceptance criteria

1. On a host with **no** Nix, `pkg doctor` exits 0 and the daemon responds to
   `nix ping-store`.
2. On a host with an existing unmanaged `/nix` (≥1 foreign store path) and/or
   a foreign `nix-daemon` unit, **every** mutating verb and `doctor` exit 74
   with remediation text, and **no** file under `/nix`, `/etc/nix`, or the
   foreign unit is modified or removed.
3. The store prefix is `/nix/store`; the product's own binary/state are **not**
   required to be inside `/nix/store` (spike acceptance).
4. Linux and macOS local builds run sandboxed under the daemon's build users
   (`nixbld*`/`_nixbld*`) with `sandbox=true`/`sandbox-fallback=false` only
   after explicit approval (D-11); a build that is impossible/disallowed
   (unsupported/broken/impure, or sandbox/build-user unavailable) yields
   `ACQUIRE_NO_BINARY` even with approval. The full-closure cache preflight
   (plan 04 §5/§6) is an availability signal that classifies every closure
   path up front, not binary-only enforcement.
5. After install, a new login shell has `<user-state>/current/bin` on PATH on both
   OSes; `pkg doctor` confirms and warns if shadowed.
6. `pkg self-uninstall --yes` on a sole-manager host leaves no `pkg` files,
   units, users, or `/nix` (when empty of foreign paths); on a host with
   foreign paths it leaves `/nix` and prints a notice.
7. Editing `/etc/nix/nix.conf` is detected by `doctor`; **ordinary broker
   operations stay blocked** (exit 78 `CONFIG`) until an **explicitly signed
   installer/runtime-maintenance `nix.conf` reconciliation command** reruns the
   render-from-descriptor step (gated by the signed descriptor) — **not** the
   bare store-path `pkg repair` (§7.4), which fixes NAR store paths and never
   rewrites `nix.conf`.
8. `NIX_SUBSTITUTERS`/`NIX_TRUSTED_PUBLIC_KEYS` set in the environment are
   ignored by the product and reported by `doctor`.
9. Architecture detection correctly reports `aarch64-darwin` on Apple Silicon
   and does not substitute `x86_64-darwin` paths.
10. The rendered root-owned `nix.conf` sets `allow-import-from-derivation = false`
    on **both** Linux and macOS; `doctor` verifies it (checksum) and flags any drift.
    No resolve/preflight/`BuildPlan` evaluation passes `--impure`; a derivation
    requiring import-from-derivation yields `ACQUIRE_NO_BINARY` (plan 04) and never
    builds, even with approval.
11. The local-build `readiness` schema is stable and cross-platform: every
    approved `BuildPlan` carries `buildUsersGroup` (`nixbld`), `buildUsersReady`,
    `useCgroupsEnabled`, and `cgroupV2Ready` explicitly (plus sandbox). Linux reports
    the cgroup fields `true` when local builds are allowed; macOS reports them
    `false` (never absent).
12. Build admission is a fair **broker-internal mutex/queue** and GC admission a
    fair **broker-internal counted RW gate** (operation handles hold shared
    GC-inhibit before any substitute/build/realization and through durable-root
    publication/abort; broker `gc` takes exclusive) — both **in-process**, with
    **no** machine-global backing-file `flock` and **no** reliance on per-operation
    `flock` inside the broker on macOS. Per-user **state** leases remain
    filesystem `flock`s in user processes (plan 05 §12). **No** automatic-GC
    timer/unit/launchd job is installed and there is no `nix.conf` auto-GC knob.
13. The per-user activation is a symlink forest under
    `<user-state>/activations/gen-<id>/` **outside** `/nix/store` (D-18);
    `current` is a relative symlink to it; activation materialization invokes
    **no Nix**; a `treeDigest` binds the path→target records; the **broker
    selects the outputs and the root helper creates one GC root per selected
    output** before the `current` swap. The raw Nix daemon socket is never
    exposed to ordinary users (I7); the unprivileged singleton broker is the
    sole daemon client; the root helper uses only the capability-gated local-store
    repair operation and never connects to the daemon (I7).
14. The rendered root-owned `nix.conf` sets `builders =` (empty) on **both**
    Linux and macOS (no remote/distributed builders in v1, D-11/INV-08);
    `doctor` verifies it (checksum).
15. The rendered root-owned `nix.conf` carries exactly `trusted-users = root`
    and `allowed-users = pkg-nix-broker` on **both** Linux and macOS; `doctor`
    verifies these lines (checksum) and flags drift. The broker user is never
    trusted. The daemon protocol does not implement mutating repair even for root
    (read-only `nix store verify` is accepted), so repair is routed to the
    fixed helper path, which accepts **only an opaque helper-issued maintenance
    capability** (caller UID + pkg-owned rooted closure + a server-derived
    path set (validated as members of the exact stored closure reachable from
    that caller uid's rooted generation, not a subset of
    `activation.outputRoots`) + plan digest covering all outputs +
    `policyVersion` + mode) and never raw broker `StorePath`s/argv/options (§7.4).
    The helper pins `--store local`; no store URL can cross the transport.
16. On Linux the daemon socket is `root:pkg-nix-broker` `0660` (parent
    `daemon-socket` dir `0750`) via systemd socket activation; on macOS the
    socket is Nix's hard-coded `0666` inside a `root:pkg-nix-broker` `0750`
    parent dir, with `allowed-users = pkg-nix-broker` as the boundary. Ordinary
    users (not in the `pkg-nix-broker` group) cannot traverse/connect on either
    OS; `doctor` verifies these modes/owners. There is **no** blanket socket-mode
    `0600` claim.
17. Store repair is split into a **read-only `nix store verify`** (broker-run
    through the daemon) and a **mutating `nix --store local store repair`**
    (helper-run as root because Nix 2.34.8's daemon protocol rejects
    `repairPath`, including for root). The helper accepts **only** an
    opaque, expiring, single-use capability bound server-side to caller UID + a
    pkg-owned rooted generation/closure + the server-derived typed path set,
    validated as **members of the exact stored closure reachable from that
    caller uid's existing rooted generation** (not merely a subset of
    `activation.outputRoots`), via **fixed store queries only** with no
    broker/public raw path input + the internal plan digest covering **all
    outputs** + `policyVersion` + mode; a target registered/expected but
    currently missing on disk (e.g. after a failed repair) is never rejected
    for absence. It **rejects** raw broker `StorePath`s/argv/options/trust knobs,
    and stale/replay/mismatch/cross-UID capabilities fail closed. In **cache-only**
    mode (`max-jobs=0`, `builders` empty) a cache hit repairs automatically and a
    cache miss + valid deriver **stops before build**; in **approved rebuild**
    (`build`) mode (after the ordinary explicit approval; no remote builders;
    broker build
    mutex + GC-inhibit held) the helper runs local repair with bounded nonzero
    `max-jobs`. **`pkg repair` is explicitly user-invoked and warns that
    during repair the affected commands can be missing or observe partial
    content; it is never atomic** (cache repair deletes the live store path,
    restores the NAR into that same path, then hashes/validates/canonicalizes/
    registers; local rebuild repair moves the old path aside before
    replacement, so a crash/power loss can leave a registered closure-member
    path **absent or partially restored (effectively corrupt)**), **never
    creates or swaps a generation** (activation-forest recovery is separate and
    Rust-only), journals per-path `intended → in-progress → post-verify` status
    in the broker/service-private `0700`/`0600` log dir, and **after an
    interrupted repair holds the generation/closure health=unknown/unhealthy
    and blocked** until broker startup recovery's content-aware read-only verify
    clears it with a **final content-hash + trust verification** (direct
    execution of an already-PATH'd command mid-repair is an explicit residual
    risk). No path is reported repaired before a **final read-only verify**
    governs success; on restart the helper **automatically retries only
    cache-only repair** (Phase A) of still-failing/missing paths, while
    repeating a **local rebuild** requires a fresh Phase B preview, explicit
    approval, and a fresh single-use capability.
18. Raw normal-operation Nix logs live only in the broker-owned `log/broker`
    directory; raw privileged repair logs live only in the distinct root-owned
    `log/helper` directory. Both are `0700` with `0600` files and inaccessible
    to ordinary users; only sanitized, versioned NDJSON may reach user state.
    The helper likewise uses a distinct root-owned private HOME/TMPDIR and never
    writes into the broker-owned private home.
19. On Linux, `pkg-root-helper` consumes exactly one systemd-activated Unix
    listener and verifies it is `/run/pkg-helper/root-helper.sock`. Startup
    fails closed for a non-root uid, missing/root-valued broker account, wrong
    listener, unsafe managed runtime/private home, or unsafe GC-root ancestors.
    Beneath an already trusted `/nix/var/nix/gcroots`, it creates only the
    root-owned `pkg/users` subtree, then binds the authenticated capability
    session to the real fixed local-store executor. Malformed or unauthenticated
    connections are rejected locally without widening the helper grammar.
20. On Linux, `pkg-nix-broker` likewise consumes exactly one systemd-activated
    listener and verifies `/run/pkg/broker.sock`, but must run as the resolved
    non-root `pkg-nix-broker` account. Every accepted connection is authenticated
    from `SO_PEERCRED` before frame decoding. The process admits at most 32
    concurrent client sessions; excess connections are closed, idle reads expire
    after five minutes, and blocked writes after 30 seconds. These are monotonic
    whole-frame deadlines, not per-syscall timers a slow-drip peer can reset. Each
    session owns its cleanup permit even if its worker exits abnormally. This entry point exposes the
    accepted operation lifecycle and six closed Real-Nix adapter methods using the fixed managed
    binary and broker-private home. Method/operation authorization is explicit, and GC admission is
    acquired before collection. Build is intentionally not exposed: caller-created receipt fields
    cannot authorize privileged execution. The in-process broker now retains one private `BuildPlan`
    behind a caller/epoch-bound build handle, returns only its sanitized preview/digest, and permits
    one exact approved consumption; wrong-UID, wrong-digest, replay, cancel, disconnect, expiry, and
    restart all fail closed. No build method is assigned on the wire until the dispatcher journals
    approval and connects that private capability to admission-time replan and execution. Adapter
    failures now return only a closed `NixAdapterErrorCode` and leave a valid
    connection reusable, while authorization/protocol failures remain connection-fatal. The
    authenticated build execution and product-command dispatch remain PR-36 wiring
    slices; there is still no generic argv or Nix-expression surface.

## 17. Unresolved questions / spikes

- **Q7.1 Store-prefix spike outcome** (§6.2/§9). *Blocking. Default if
  inconclusive: depend on `/nix/store`, multi-user install.*
- **Q7.2 Single-user mode.** Defer; multi-user chosen for uniformity/sandbox.
  *(Plan 12.)*
- **Q7.3 musl/Linux-static.** v1 targets glibc `*-linux`; musl is a spike
  (affects bundled-Nix build and `system`). *(Default: defer.)*
- **Q7.4 macOS sandbox primitives.** Nix's macOS sandbox is supported but
  uses different, generally narrower platform primitives than Linux; `pkg`
  requires `sandbox=true`/`sandbox-fallback=false` and fails closed if
  readiness cannot be verified, while honestly disclosing that macOS isolation
  is not identical to Linux's. Custom `sandbox-exec` profiles are out of scope.
  *(Default: rely on Nix's macOS sandbox; never claim parity.)*
- **Q7.5 Per-user vs shared profile.** **RESOLVED → multi-user per-user
  authoritative state (D-17/INV-10).** The managed
  runtime/channel/index/source/store service is root-owned and shared;
  manifest/lock/generations/activation/journal/current/PATH are per-user keyed
  by uid (plan 01 §9, plan 05 §4, §10). Affects installer group membership and
  the per-user PATH snippet (§10).
- **Q7.6 Container/WSL specifics.** systemd-in-WSL, rootless daemon socket
  paths. *(Default: detect and adapt; document matrix.)*
- **Q7.7 Bootstrap descriptor distribution.** How the installer obtains the
  initial signed descriptor + bundled Nix out-of-band (chicken/egg) — likely a
  pinned copy shipped in the installer image, verified against a long-term
  root key. *(Tie to plan 02/10.)*

## 18. Sources (current Nix behavior)

[^system]: `system` values, Nix Reference Manual (e.g. `builtins.currentSystem`)
& Nixpkgs `lib.systems` → https://nixos.org/manual/nixpkgs/stable/#sec-system-pkgs
[^multi-user]: Multi-user installation & build users, Nix Reference Manual →
https://nixos.org/manual/nix/stable/installation/multi-user.html
[^gc-roots]: Garbage collector roots →
https://nixos.org/manual/nix/stable/package-management/garbage-collection.html
[^ping-store]: `nix ping-store` →
https://nixos.org/manual/nix/stable/command-ref/new-cli/nix3-store-ping.html
