# 08 — Security Model & Threat Catalog

**Owner:** Assurance track (plans 08–12). **Status:** Draft v1 (planning only).
**Depends on:** `00-overview-and-decisions.md`, `01-system-architecture.md`, `02-trust-and-update-model.md`, `03-nixpkgs-source-and-index.md`, `04-resolution-install-build.md`, `05-state-locks-generations-gc.md`, `06-cli-and-user-experience.md`, `07-platform-installation-and-runtime.md`.
**Feeds into:** `09-testing-and-validation.md` (security test lane), `10-release-and-operations.md` (key/incident/revocation), `11-pr-roadmap.md` (security PRs), `12-open-decisions-and-risks.md` (security risks & crypto decisions).

---

## 1. Purpose & Scope

This document defines an **explicit, falsifiable threat model** for the product: a Rust
package manager that hides a bundled, **managed** Nix runtime behind brew/paru-style
commands, where Nixpkgs-at-an-exact-revision is the catalog and `cache.nixos.org` is the
v1 binary cache.

### In scope
- All v1 surfaces: installer, root helper / privileged daemon, product↔Nix IPC, CLI,
  state/locks/generations, GC roots, channel metadata, the disposable search index,
  substitution, local Linux builds, uninstall, and **runtime execution of installed
  packages**.
- The full life cycle: first install → steady state → update → rollback → uninstall.

### Explicitly out of scope (v1)
- **Runtime sandboxing of installed applications.** Once a package is *activated* into the
  user's PATH, it executes with the user's normal privileges. v1 does **not** containerize,
  seccomp, or otherwise isolate installed apps at runtime. See §11 (honest limitations) and
  threat **T-RUN-1**.
- **Multi-user features beyond the D-17 per-user state split.** v1 *does* isolate
  users: authoritative package state (manifest/lock/generations/activation/journal)
  is per-user keyed by uid, and the privileged boundary is UID-authenticated
  (D-17/INV-10/ARCH-INV-06; see T-INST-6, AC-S11/S12). What remains out of scope is
  *deeper* multi-user features: per-user substituters/trust keys, shared or
  co-owned activations, and runtime isolation of one user's installed apps from
  another (those stay with the per-user trust model above, not a new surface).
- Windows (no v1 target).
- Arbitrary user-supplied Nix expressions, flakes, overlays, or substituters — these are
  **removed from the attack surface by design** (see `02` and §6.4).

### Reading convention
Throughout this document:

> **ℹ️ FACT (current Nix behavior)** — verifiable against official Nix documentation today.
>
> **📐 DECISION (product design)** — a choice we make on top of Nix; not a Nix guarantee.

Primary sources cited by short key (full list in §13):
`[NIX-MANUAL]`, `[NIXPKGS-MANUAL]`, `[NIX-SEC]` (security chapter), `[TUF]`, `[TOUGH]`,
`[SIGSTORE]`, `[RB]` (Reproducible Builds), `[NIX-CVE]`, `[NIXOS-SA]`, `[HYDRA]`.

---

## 2. System Under Consideration & Trust Boundaries

### 2.1 Components

| ID | Component | Owner plan | Runs as | Persisted where |
|----|-----------|-----------|---------|-----------------|
| CLI | User-facing command binary (`pkg`) | `06` | invoking user | — |
| CORE | Rust domain core: state, locks, generations, resolver, install pipeline | `04`,`05` | invoking user | product state dir |
| CHANNEL | Signed channel descriptor client (mature update metadata) | `02` | invoking user | product state dir |
| INDEX | Disposable search/list/info index (derived from pinned Nixpkgs) | `03` | invoking user | product cache dir |
| NIXCLIENT | Product's adapter that talks to the managed Nix daemon/CLI over JSON | `01`,`04` | invoking user | — |
| NIXD | Managed Nix daemon (bundled, pinned) | `01`,`07` | privileged (root-owned svc, builder users) | `/nix`-style store |
| STORE | The Nix store (closure of built/substituted paths) | `01`,`07` | root-owned files, builder-writable during build | managed store prefix |
| HELPER | Privileged installer/root helper (Linux setuid or polkit; macOS launchd/LP) | `07` | root | — |
| RELEASE | CI/release infra that signs channel metadata & publishes index/targets | `10` | release service | product CDN |

### 2.2 Trust boundary diagram

```mermaid
flowchart TB
    subgraph Internet["Untrusted network"]
        CDN["Product CDN<br/>(channel, index, managed-Nix tarball)"]
        CACHE["cache.nixos.org<br/>(binary cache)"]
        NIXPKGS["Nixpkgs git<br/>(catalog source)"]
    end

    subgraph UserSpace["User-owned process space"]
        CLI["pkg CLI (user)"]
        CORE["pkg-core (user)"]
        CHANNEL["channel client (user)"]
        INDEX["index (user)"]
    end

    subgraph PrivSpace["Privileged boundary"]
        HELPER["root helper"]
        NIXD["managed nix-daemon"]
        STORE[("/nix store")]
    end

    subgraph HostFS["Host filesystem"]
        STATE[("product state dir\nlocks, generations, journal")]
        HOME[("~/.pkg (profile, PATH link)")]
    end

    CLI --> CORE
    CORE --> CHANNEL
    CORE --> INDEX
    CORE -- "JSON/CLI contract" --> NIXD
    CHANNEL -->|HTTPS+sig| CDN
    INDEX -->|verify narHash| NIXPKGS
    NIXD -->|substitute+verify sig| CACHE
    NIXD --> STORE
    HELPER --> NIXD
    HELPER --> STORE
    CORE --> STATE
    CORE --> HOME
```

**Trust boundaries crossed (each is a control point):**
1. **Internet → product CDN → CHANNEL.** Signed update metadata must verify (T-CHAN-*).
2. **Internet → cache.nixos.org → STORE.** Substituted paths must verify against the
   channel-approved key set (T-CACHE-*).
3. **User space → privileged (NIXD/HELPER).** IPC must authenticate the caller and reject
   path/symlink injection (T-DAEMON-*, T-HELPER-*, T-PATH-*).
4. **User space → Host FS (STATE, HOME).** State must be integrity-checked and atomic
   (T-STATE-*, T-PATH-*).
5. **STORE → user runtime (PATH).** Activation maps provenance → executed code
   (T-RUN-*); we provide *provenance + reproducibility*, not runtime isolation.

---

## 3. Assets & Security Goals

| Asset | Confidentiality | Integrity | Authenticity | Availability |
|-------|:---:|:---:|:---:|:---:|
| Product state (locks, generations, journal) | low | **critical** | **critical** (anti-rollback) | high |
| Channel metadata + signing keys (root/targets) | n/a | **critical** | **critical** | high |
| Managed Nix install (binaries, daemon config) | low | **critical** | **critical** | high |
| Disposable search index | low | medium (regenerable) | medium (narHash-pinned) | low |
| Nix store contents | low | **critical** | **critical** (signed/NAR-verified) | high |
| User data / shell environment | medium | high | n/a | high |
| Signing keys (release-side) | **critical** | **critical** | n/a | high |

**Primary security goals (G1–G6):**
- **G1 Authenticity of catalog & runtime.** Only the product-approved Nixpkgs revision,
  managed-Nix version, index hash, and substituter/key set are ever used.
- **G2 Integrity of installed software.** Every store path is either substituted under a
  channel-trusted key or locally built from the pinned Nixpkgs under sandbox; corrupted
  paths are detected and repaired.
- **G3 Anti-rollback / anti-freeze of channel metadata.** Clients reject stale, replayed,
  or frozen channel descriptors.
- **G4 State integrity & recoverability.** Operations are atomic; a failed install leaves
  the prior generation intact; tampered state is detected, not silently repaired from
  attacker data.
- **G5 Least privilege.** The privileged helper and daemon expose a minimal, authenticated
  surface; no arbitrary eval/substituter/expression controls reach the user.
- **G6 No silent privilege escalation or persistence.** Installer/daemon setup is explicit
  and reversible; uninstall is bounded and never removes assets we did not create.

---

## 4. Actors & Positions

| Actor | Trust level | Capabilities assumed |
|-------|-------------|----------------------|
| End user (legitimate) | Trusted for their own data; **not** trusted to supply Nix inputs | Runs CLI; can edit files they own; **cannot** supply flakes/overlays/substituters |
| Network attacker (MITM) | Untrusted | Can tamper with/drop/replay HTTPS to CDN or cache; can serve old artifacts |
| Malicious package in Nixpkgs | **Partially trusted by Nix** as a *build recipe* | Can execute arbitrary build logic under sandbox; can produce malicious binaries (see §11) |
| Compromised CDN account | High impact, low likelihood | Can publish new channel/index/Nix artifacts; **cannot** forge signatures without keys |
| Compromised release-signing key | **Critical** | Can sign malicious channel metadata until detected & revoked |
| Local unprivileged attacker on host | Can read/write their own files; may try symlink/path tricks | Targets STATE/HOME/cache paths; cannot write `/nix` store or privileged dirs |
| Local privileged attacker (root) | **Out of scope** | Root on the host can always win (tamper with store, keys, kernel). We detect, we don't prevent. |

---

## 5. Nix Guarantees — Facts vs. What We Add

It is essential to be precise about what Nix **does** and **does not** promise, because a
naïve reading of "Nix is reproducible and signed" overstates our security posture.

### 5.1 What Nix actually guarantees (FACTs)

> **ℹ️ FACT.** Nix store paths are content-addressed by derivation input hash
> (`/nix/store/<hash>-<name>`); the hash binds all build inputs. `[NIX-MANUAL]`
>
> **ℹ️ FACT.** Binary caches sign each downloadable path with **Ed25519** keys; a client
> only accepts a substitute if the path's signature verifies against a configured
> **trusted-public-keys** set and the substituter is permitted. `[NIX-MANUAL]` "Secure
> Binary Caches".
>
> **ℹ️ FACT.** `nix-store --verify` recomputes NAR serializations to detect local
> corruption; `--repair` re-fetches/rebuilds from trusted sources. `[NIX-MANUAL]`
>
> **ℹ️ FACT.** With **`sandbox = true`** (default on supported Linux), builds execute in an
> isolated mount/PID/network namespace and may access only declared inputs. macOS sandboxing
> is more limited. `[NIX-MANUAL]` "Sandboxed builds".
>
> **ℹ️ FACT.** `nix-daemon` authenticates clients via Unix-socket permissions plus the
> `trusted-users` / `allowed-users` config; only trusted users can perform privileged
> operations. `[NIX-MANUAL]`
>
> **ℹ️ FACT.** Nixpkgs reproducibility is **per-derivation and incomplete**; many
> attributes are not bit-for-bit reproducible. Reproducible Builds tracks coverage.
> `[RB]`, `[NIXPKGS-MANUAL]`

### 5.2 What Nix does **not** guarantee — and therefore what the product must be honest about

> **ℹ️ FACT (the central honesty point).** **Nixpkgs is not a security audit or allowlist.**
> It is a large, community-reviewed repository. Pinning a revision gives **reproducibility
> and provenance of the build**, **not** assurance that the recipe or its upstream source is
> free of malware. Public supply-chain incidents have affected Nixpkgs. `[NIXOS-SA]`,
> `[NIXPKGS-MANUAL]`. **Consequence:** the product *inherits* this risk and must say so to
> users (see §11, T-RUN-1). We add **no** claim of "safe to install."

> **ℹ️ FACT.** Local builds run the **derivation's build script on the host.** Even with
> sandboxing, the build script is whatever the pinned Nixpkgs says; a malicious recipe can
> attempt to exfiltrate during build (sandbox blocks network on Linux) or produce a
> malicious output that is then *executed at runtime*.

> **ℹ️ FACT.** Signature verification only proves a path was vouched for by a trusted
> **cache key**. It does **not** prove the path is benign. Trusting `cache.nixos.org` means
> trusting whatever Hydra built from that Nixpkgs revision. `[HYDRA]`

### 5.3 What the product adds on top (DECISIONs)

> **📐 DECISION (G1).** The product reads **only** its signed channel descriptor for the set
> of `trusted-public-keys`, `substituters`, supported systems, Nixpkgs `rev`/`narHash`,
> managed-Nix version, and index hashes. User config cannot broaden this set. (`02`)
>
> **📐 DECISION (G3).** Channel metadata uses a **mature** signed-update framework with
> built-in rollback/freeze protection — **real TUF** via the Rust `tough` crate (used by AWS
> Bottlerocket), **not** a hand-rolled "TUF-lite." See §7 and DR-002 in `12`. `[TUF]`,
> `[TOUGH]`
>
> **📐 DECISION (G5).** The product owns GC roots, generations, and activation; Nix profile
> state is **not authoritative**. The managed Nix daemon is configured with `trusted-users`
> restricted to the product's own runtime identity and substituters pinned to the channel
> set. (`05`, `07`)
>
> **📐 DECISION (G4).** State writes are atomic (temp+fsync+rename), generation-numbered,
> and journal-backed; integrity is self-checked on load. Tampered state fails **closed** and
> surfaces the last known-good generation, never auto-repairing from attacker-controlled
> files. (`05`)

---

## 6. Threat Catalog

Each threat: **ID · component · STRIDE · attacker position · capability needed · impact ·
Nix-native control · Product control · residual · plan refs.** Severity = Likelihood ×
Impact (L/M/H) using the rubric in §6.0.

### 6.0 Severity rubric
- **Likelihood:** L = requires key compromise or privileged position; M = network position or
  public-cache write; H = any local user or trivial public input.
- **Impact:** L = degraded UX / regenerable data; M = denial of service / wrong version
  installed (still pinned & reproducible); H = code execution as user / persistence /
  loss of the product's authenticity guarantee.

### 6.1 Installer & root helper

| ID | Threat | STRIDE | Attacker pos. | Capability | Impact | Nix control | Product control | Residual | Refs |
|----|--------|--------|---------------|-----------|--------|-------------|-----------------|----------|------|
| **T-INST-1** | Malicious/modified installer script downloaded over MITM or from spoofed host | Spoofing/Tampering | Network | Serve altered installer | H | none | Pin installer checksum in docs; fetch over TLS with pinned cert/commit; verify detached signature; `pkg doctor` self-verify | M (key/cert compromise) | `07`,`10` |
| **T-INST-2** | TOCTOU / symlink swap in directories the installer writes before privilege drop | Tampering/EoP | Local unpriv. | Pre-create symlink in writable path | H | none | Installer uses `O_NOFOLLOW`, `mkdtemp` under product-owned root, `fchdir`+`openat` relative opens; rejects world-writable ancestors | L | `07` |
| **T-INST-3** | Root helper accepts unauthenticated commands from any local user | EoP | Local unpriv. | Talk to helper socket | H | n/a | Helper authenticates caller via `SO_PEERCRED` (Linux)/`getpeereid` (macOS); single fixed socket with 0600; command allowlist; no path/string passthrough to shell | L | `07`,`04` |
| **T-INST-4** | Existing **unmanaged** Nix present; product silently co-mounts `/nix/store` and crosses trust domains | EoP/Tampering | Pre-existing install | Pre-seed store/profiles | H | none | **Fail closed**: refuse to install/manage; emit manual remediation; **never auto-delete** user's Nix. (G6) | L (UX: user must remediate) | `07`,`01` |
| **T-INST-5** | Installer leaves privileged helper with overly broad permissions (setuid root, world-executable) | EoP | Local unpriv. | Invoke helper | H | n/a | Prefer **polkit** (Linux) / **launchd + authorized-client** (macOS) over setuid; if setuid unavoidable, drop privs ASAP, cap to install/daemon ops | M | `07` |
| **T-INST-6** | Cross-user tampering / UID confusion: a local user tries to read/modify another user's authoritative state or trick the helper/daemon into creating GC roots under another uid | EoP/Tampering | Local unpriv. | Spoof identity / cross-uid path | H | socket peer creds | Per-user `<user-state>` owned by uid 0700 (D-17/INV-10); helper authenticates caller uid via `SO_PEERCRED`/`getpeereid`/Audit Token and scopes GC-root writes to `/nix/var/nix/gcroots/pkg/users/<caller-uid>/` only (ARCH-INV-06); daemon `trusted-users` restricted; package state never globally shared | L | `01`,`05`,`07` |

### 6.2 Daemon & IPC protocol (product↔Nix and product↔helper)

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-DAEMON-1** | Untrusted client drives the managed `nix-daemon` to build/eval arbitrary expressions or pull from arbitrary substituters | EoP/Tampering | `trusted-users` too broad; user supplies `--expr`/`--substituters` | Product never exposes expression/substituter flags; daemon `trusted-users` restricted to product runtime identity; `restrict-eval` + `--allowed-uris` for any eval we perform | L | `01`,`04` |
| **T-DAEMON-2** | JSON/CLI contract parsing: malformed or huge Nix output causes panic/RCE in product | DoS/RCE | Nix emits unexpected JSON | Parse with strict serde, size-capped, never `eval`; contract tests in `09` (golden + fuzz) | L | `04`,`09` |
| **T-DAEMON-3** | Replay of an old daemon command stream | Tampering | Capture socket traffic | Daemon ops are idempotent & validated against current state; no replay-sensitive tokens over the socket | L | `04` |
| **T-DAEMON-4** | Rogue process impersonates the daemon on the socket (path hijack) | Spoofing | Replace socket file | Socket created by privileged setup with 0600 owner=root/product; product connects only to the well-known path created by HELPER; verify peer creds | L | `07` |

### 6.3 Source evaluation (pinned Nixpkgs reevaluation)

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-EVAL-1** | A pinned Nixpkgs attribute performs **impure** fetches at eval time (e.g., `fetchTarball` without hash, `builtins.currentTime`) | Tampering/Info disclosure | Uncontrolled network/impurity during resolve | We pin `narHash`/`rev`; use `--pure-eval`/`restrict-eval`; forbid `--impure`; reject attributes that require impurity | L | `03`,`04` |
| **T-EVAL-2** | Eval-time side effects leak host info (env, paths) | Info disclosure | Impure eval reads env | Pure-eval enforces; product passes minimal env to the Nix invocation; env scrub documented | L | `04` |
| **T-EVAL-3** | Eval is expensive → DoS via pathological attribute | DoS | Resolve hangs/OOMs | Time + memory caps on eval (`RLIMIT`, `timeout`), cancellation, preflight cost preview to user | M | `04`,`09` |

### 6.4 Index poisoning (disposable search index)

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-IDX-1** | Attacker tampers with the published index to make `search`/`info` misdirect users (e.g., point "openssl" at a malicious pname) | Spoofing/Tampering | MITM or CDN compromise | Index is **disposable** and re-derived from the pinned Nixpkgs `narHash`; published index carries a hash recorded **in the signed channel descriptor**; client verifies hash before use, regenerates on mismatch | L | `03`,`02` |
| **T-IDX-2** | Local cache of the index is swapped for a poisoned one | Tampering | Local unpriv. writes to cache dir | Index path under product-owned dir with 0700; hash verified every load; on mismatch, re-derive (do **not** trust local copy) | L | `03` |
| **T-IDX-3** | Index regeneration is non-deterministic → silent drift | Tampering (false confidence) | Build nondeterminism | Reproducible index build (sorted, pinned inputs); CI asserts determinism across hosts (`09`) | L | `03`,`09` |
| **T-IDX-4** | "Typosquatting"/naming confusion **within Nixpkgs itself** is surfaced to the user as if curated | Spoofing | Nixpkgs contains confusing/duplicate names | We **do not** claim curation; search results show exact attribute + version + closure; `info` surfaces upstream homepage/license so users can disambiguate (honesty, not a fix) | **M (inherited)** | `03`,`06` |

> **📐 DECISION.** The index is **defense-in-depth**, never a trust root. A package is
> *resolved and realized* from the pinned Nixpkgs, not "installed from the index." A poisoned
> index can mislead the UI but **cannot** change what bytes get installed (T-IDX-* impact is
> bounded to UX misleading, not RCE).

### 6.5 Channel rollback / freeze

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-CHAN-1** | **Rollback attack**: attacker serves an older, validly-signed channel descriptor (e.g., one pinning a vulnerable Nixpkgs with a known-bad package) | Tampering | Replay old descriptor | **Real TUF** provides timestamp+snapshot roles with expiry and a monotonically-checked version; client refuses descriptors past expiry or with lower version than previously seen | L | `02`,`12` (DR-002) |
| **T-CHAN-2** | **Freeze attack**: attacker blocks updates so client keeps using a stale-but-valid descriptor indefinitely | DoS/Tampering | Block HTTPS | TUF `timestamp.json` has a short expiry; product surfaces "channel is stale" to user and refuses `update`/`upgrade` after grace window | M (UX) | `02` |
| **T-CHAN-3** | **Mix-and-match**: attacker combines roles from different versions | Tampering | Combine snapshots/targets | TUF roles are cross-signed within a consistent snapshot; client verifies the snapshot hashes targets | L | `02` |
| **T-CHAN-4** | **Endless-data**: attacker serves huge metadata to exhaust resources | DoS | Large JSON | Size caps; TUF metadata is tiny by construction (small target set) | L | `02` |
| **T-CHAN-5** | **Key compromise**: a channel signing key is stolen | Tampering (forge) | Steal key | TUF **threshold** signatures + key rotation/revocation via a new root role; revocation procedure in `10`; offline root key | M (until rotation) | `02`,`10` |

### 6.6 Cache substitution (`cache.nixos.org`)

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-CACHE-1** | MITM serves an unsigned or differently-signed path | Tampering | Network | Nix verifies Ed25519 sig against channel-trusted keys; substituters pinned to `cache.nixos.org` only; product ignores `~/.config/nix/nix.conf` substituters | L | `02`,`01` |
| **T-CACHE-2** | cache.nixos.org itself is compromised and serves paths signed by its own (trusted) key that are malicious | Tampering (trusted!) | Cache-key abuse | We **cannot** prevent (we trust the key); mitigate via: pin Nixpkgs rev so the *closure* is fixed & reproducible; offer `repair` (re-verify NAR); document inherited risk; future: second-source/mirror for v2 | **H (inherited, low likelihood)** | `10`,`11` |
| **T-CACHE-3** | Path collides with local store but NAR differs (corruption masked) | Tampering | Local corruption | `nix-store --verify` detects; product runs verify on `repair` and on doctor | L | `04` |
| **T-CACHE-4** | Substituter key rotation by upstream not reflected in channel | DoS | Old key removed | Channel descriptor is the single source of allowed keys; we update via signed channel update, not upstream nix.conf | L | `02` |

> **ℹ️ FACT.** Trusting `cache.nixos.org` is equivalent to trusting Hydra's build of that
> Nixpkgs revision. `[HYDRA]` **📐 DECISION.** v1 accepts this single source of binaries and
> says so explicitly; a multi-cache/threshold-binary-trust model is **deferred** to v2
> (T-CACHE-2 residual).

### 6.7 Local builds (Linux only, after preview+approval)

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-BUILD-1** | Build script (from Nixpkgs) runs arbitrary code during local build | RCE-as-builder | Any local build | `sandbox=true` (network blocked, fs restricted); builder users isolated; **explicit user approval** with closure-size + derivation preview; macOS is **binary-only** in v1 (no local build) | M (sandbox escapes are rare but exist) | `04`,`07` |
| **T-BUILD-2** | Resource exhaustion during build (fork bomb, disk fill) | DoS | Pathological derivation | RLIMIT_AS/CPU/FILES, cgroup CPU/mem/io caps, disk-quota guard, timeout | M | `04` |
| **T-BUILD-3** | Build writes outside sandbox (sandbox escape) | Tampering/EoP | Kernel/CVE | Run as unprivileged builder users; drop all caps; seccomp filter; monitor for sandbox=`broken` | M (depends on kernel) | `07` |
| **T-BUILD-4** | Non-reproducible local build diverges from cache → "two valid builds" confusion | Tampering (trust) | Nondeterminism | When a cache path exists for the same drv, **prefer cache**; only build locally when absent or user-forced; `--check` available in `repair` | M | `04`,`03` |
| **T-BUILD-5** | Approval bypass (UI race) makes "build" the default | Tampering | UX bug | Approval is an **explicit** non-default action; never auto-build; TUI records consent event in journal | L | `06`,`04` |

### 6.8 Path / symlink attacks on state & activation

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-PATH-1** | Attacker pre-creates symlinks in product dirs to redirect writes (e.g., `current` → `/etc/passwd`) | Tampering/EoP | Local unpriv. | Product dirs `0700` owner=product; atomic `rename`/`symlink` with `O_NOFOLLOW`; reject if target exists & not owned; `openat`-relative | L | `05` |
| **T-PATH-2** | Activation swaps a binary in `~/.pkg/bin` after verify but before exec | TOCTOU | Local unpriv. | Activation symlinks point into the read-only store (`/nix/store/...`), not user-writable copies; verify store path at activation time | L | `05`,`01` |
| **T-PATH-3** | PATH injection makes `pkg` invoke a trojaned `nix`/helper | EoP | PATH tamper | Product resolves managed-Nix & helper by **absolute path** from its install root, never `$PATH`; doctor warns on shadowing | L | `07`,`06` |
| **T-PATH-4** | World-writable ancestor directory of state files | Tampering | Bad perms | On load & write, walk ancestors, refuse if any is group/world-writable & not product-owned | L | `05` |

### 6.9 State tampering, concurrency, journal

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-STATE-1** | Direct edit of state to pin a malicious realized set or hide a downgrade | Tampering | Local unpriv./root | State is integrity-tagged (Merkle/sum over generations + signed anchor from channel); on mismatch, **fail closed** to last good; never auto-repair from local file | L (root can still win; detect only) | `05` |
| **T-STATE-2** | Two `pkg` instances race on the same state | Tampering/DoS | Concurrent CLI | Single-writer lock (flock on lockfile with pid+boot-id); stale-lock recovery via operation journal; operations idempotent & resumable | L | `05` |
| **T-STATE-3** | Crash mid-activation leaves inconsistent `current` | DoS | Power loss | Atomic `current` swap (rename), journal replay on startup to committed generation; previous generation always recoverable | L | `05` |
| **T-STATE-4** | Garbage-collecting a path still referenced by an active generation | DoS (broken activation) | GC bug | Product-owned GC roots mirror active generations; GC must consult generation manifest, not just roots | L | `05` |

### 6.10 Concurrency / process model

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-CONC-1** | Background `update`/`upgrade` racing interactive `install` | Tampering/DoS | Concurrent ops | Writer lock; long ops hold a *lease* renewed via journal; conflict surfaced to user | L | `05` |
| **T-CONC-2** | Daemon killed mid-substitute leaves partial path referenced | DoS | Crash | Nix atomic store import; product verifies completeness on next op | L | `04` |

### 6.11 Logs & secrets

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-LOG-1** | Logs leak secrets (tokens, private paths, env) | Info disclosure | Verbose logging | Structured logs with **allowlist** redactor; secret denylist; logs default to product dir 0600; never log env/args wholesale | L | `10`,`06` |
| **T-LOG-2** | Logs used to inject/mislead support (log injection) | Tampering | User-controlled strings in logs | Escape control chars; quote attacker-influenced fields | L | `10` |
| **T-LOG-3** | Crash dumps contain sensitive memory | Info disclosure | Panic | Rust panic = no core by default; opt-in minidump with redaction; `RUST_BACKTRACE` default 0 in release | L | `10` |

### 6.12 Update / release compromise

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-REL-1** | Compromised release pipeline signs malicious channel/index/Nix | Tampering (forge) | CI/CD hijack | Release signing on **offline/HSM-backed** step; threshold signatures (TUF); release attestation (provenance); 2-person release approval | M | `10` |
| **T-REL-2** | Signing key exfiltration | Forge | Key theft | Offline root key; online keys least-privilege & rotated; revocation procedure (§9 / `10`) | M | `10` |
| **T-REL-3** | Managed-Nix tarball or Nixpkgs tarball swapped on CDN | Tampering | CDN account | Hash recorded in **signed** channel descriptor; client verifies before use; pinning independent of CDN | L | `02`,`07` |
| **T-REL-4** | Supply-chain compromise of a Rust dependency | Tampering | Crate malice | `cargo deny`, `cargo audit` in CI, vendored lockfile, dependency review gate, reproducible builds of the CLI | M | `10`,`09` |

### 6.13 Uninstall

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-UNINST-1** | Uninstall removes the user's **own** Nix install/profile | Destruction | Over-broad cleanup | Uninstall removes **only** paths it created & recorded in an asset manifest; refuses to touch `/nix` if an unmanaged Nix is detected; dry-run preview | L | `07` |
| **T-UNINST-2** | Uninstall leaves privileged residue (daemon, setuid) | Persistence | Incomplete cleanup | Asset manifest is authoritative; uninstall verifies zero privileged residue and reports; `doctor --post-uninstall` | L | `07` |
| **T-UNINST-3** | Partial uninstall corrupts shell PATH permanently | DoS | Bad edit | PATH edits via a managed snippet file `source`d from rc, not inline edits; removal = delete snippet + warn | L | `06`,`07` |

### 6.14 Compromised packages at runtime

| ID | Threat | STRIDE | Detail | Controls | Residual | Refs |
|----|--------|--------|--------|----------|----------|------|
| **T-RUN-1** | An installed package contains malware that executes when the user runs it | RCE-as-user | Malicious upstream or Nixpkgs recipe | **No technical prevention in v1.** Mitigations: full provenance (attribute, rev, build source, build time, cache key) visible via `pkg info`/`pkg history`; reproducible re-verification via `repair`; rapid revocation path via channel update pinning a new Nixpkgs rev; **honest disclosure** that Nixpkgs is not curated | **H (inherited; this is the core residual risk)** | `06`,`10` |
| **T-RUN-2** | A "good" package is later found vulnerable; users not notified | Info/DoS | No vuln feed | v1: `pkg outdated` reflects channel updates; v2: CVE/vuln feed integration (deferred). Document gap. | M | `12` |

> **📐 DECISION (honesty).** The product positions itself as **provenance + reproducibility +
> fast revocation of the *catalog***, **not** as a guarantee that installed software is safe.
> All user-facing docs (`06`, `10`) carry this statement. This is a deliberate scoping
> decision recorded in `12` (DR-009).

---

## 7. Cryptography & Key Management

### 7.1 Channel metadata: real TUF via `tough`
> **📐 DECISION (DR-002).** Use the **real TUF** specification implemented by the Rust
> `tough` crate for channel metadata. Rationale: TUF directly mitigates the four named
> threats in §6.5 (rollback, freeze, mix-and-match, endless-data) and supports **threshold
> signatures** and **key revocation** — exactly the properties this design needs — using a
> mature, audited implementation rather than a bespoke scheme. `[TUF]`, `[TOUGH]`
>
> Alternative considered: Sigstore (cosign/rekor). Not chosen for v1 because the offline /
> air-gapped and low-volume nature of the channel favors TUF's static metadata model; a
> Sigstore-based *release attestation* layer may layer on top later (deferred). `[SIGSTORE]`

**TUF roles in use:**
| Role | Purpose | Key custody | v1 threshold |
|------|---------|-------------|--------------|
| root | Trust root; authorizes other keys & rotations | **offline / HSM** | 1-of-1 (v1) with documented rotation; target 2-of-3 at GA |
| targets | Pins Nixpkgs rev/narHash, managed-Nix version, index hashes, allowed substituters/keys, systems, policy version | online release service | 1-of-1 (v1) |
| snapshot | Binds consistent versions of all metadata | online | 1-of-1 |
| timestamp | Short-lived anti-freeze/rollback marker | online, frequent | 1-of-1 |

### 7.2 Binary cache trust
> **ℹ️ FACT.** `cache.nixos.org` uses Ed25519 key
> `cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=`. `[NIX-MANUAL]`
> **📐 DECISION.** This key, and only this key (plus the product's own key if/when it
> self-hosts artifacts), is admitted via the signed channel descriptor's `substituters` /
> `trusted-public-keys`. User config is ignored.

### 7.3 State integrity
> **📐 DECISION.** State integrity uses a **tamper-evident** structure (generation Merkle
> root anchored to a field inside the last-applied signed channel `targets`), not an
> independent keyed MAC — so we do **not** need to store a secret key on the client to detect
> tampering. This avoids a client-side secret-management problem (a key on disk protects
> nothing against a local root anyway; the goal is **detection + fail-closed**, not
> secrecy). `05`

### 7.4 Key management lifecycle (release-side) — summary; full procedure in `10`
- Root key: generated offline, stored on HSM/air-gapped media; used only for root rotation.
- Online keys (targets/snapshot/timestamp): per-release-service, rotated quarterly, revocable.
- Revocation: new `root.json` / `targets.json` with the compromised key removed; short
  `timestamp` expiry bounds exposure; incident playbook in `10`.
- Loss-of-root recovery: pre-distributed offline backup quorum; documented break-glass.

---

## 8. Control Summary Matrix (threat → control → plan)

| Threat family | Primary Nix control | Primary product control | Owning plan(s) | Tested in |
|---------------|--------------------|-------------------------|---------------|-----------|
| Installer/helper (T-INST) | — | auth caller (uid), O_NOFOLLOW, fail-closed on unmanaged Nix, no setuid if avoidable, per-user uid-scoped state/roots (D-17) | `07` | `09` security lane |
| Daemon/IPC (T-DAEMON) | socket perms, trusted-users | strict serde, size caps, no expr/substituter passthrough | `01`,`04` | `09` contract+fuzz |
| Source eval (T-EVAL) | pure-eval, restrict-eval, allowed-uris | pin rev/narHash, env scrub, RLIMIT | `03`,`04` | `09` fault-injection |
| Index (T-IDX) | — | disposable + hash-in-channel + regen-on-mismatch | `03`,`02` | `09` e2e + determinism |
| Channel (T-CHAN) | — | **real TUF (tough)**, expiry, version monotonicity | `02` | `09` security lane |
| Cache (T-CACHE) | Ed25519 sig verify, trusted keys | pin substituters/keys via channel | `01`,`02` | `09` security lane |
| Local builds (T-BUILD) | sandbox, builder isolation | approval gate, RLIMIT/cgroups, binary-only macOS | `04`,`07` | `09` platform + fault |
| Path/symlink (T-PATH) | store is root-owned | 0700 dirs, O_NOFOLLOW, openat, store-relative symlinks | `05` | `09` security lane |
| State/concurrency (T-STATE/CONC) | — | atomic writes, flock+lease, journal, fail-closed | `05` | `09` fault-injection |
| Logs/secrets (T-LOG) | — | redactor, allowlist fields, 0600 | `06`,`10` | `09` security lane |
| Release (T-REL) | — | offline root, threshold, cargo deny/audit, attestation | `10` | `09` release gate |
| Uninstall (T-UNINST) | — | asset manifest, dry-run, never touch unmanaged Nix | `07` | `09` e2e |
| Runtime (T-RUN) | provenance/reproducibility | honest disclosure, revocation via channel | `06`,`10` | `10` incident drills |

---

## 9. Revocation & Incident Hooks (summary; procedures in `10`)

1. **Package-level badness discovered** → publish a new signed `targets.json` pinning a
   Nixpkgs rev that removes/fixes the attribute; clients see it on next `update`; v1 cannot
   *block* an attribute that still exists in the pinned rev without a re-pin (documented
   limitation).
2. **Channel key compromise** → TUF root rotation removing the key; short `timestamp` expiry
   bounds window; broadcast advisory.
3. **CLI/Nix runtime CVE** → emergency channel update bumping managed-Nix version; the
   managed-Nix tarball hash is signed in `targets`, so a swap is bounded by signature.
4. **Compromised cache.nixos.org (T-CACHE-2)** → v1 residual risk; mitigation is
   re-pinning Nixpkgs (closure reproducibility) + `repair`; full fix (second source) deferred.

---

## 10. Platform Differences (security-relevant)

| Aspect | Linux | macOS |
|--------|-------|-------|
| Privilege helper | polkit-gated service (preferred) or setuid w/ priv drop | launchd daemon + authorized client (no setuid) |
| Build sandbox | full `bwrap`/namespaces sandbox | limited; **no local build in v1** (binary-only) |
| Caller auth | `SO_PEERCRED` | `getpeereid` / launchd `Audit Token` |
| PATH integration | rc-snippet sourced by shell | rc-snippet + `/etc/paths.d` considered; doctor verifies |
| Root-owned store | yes (daemon model) | yes (daemon model) |

---

## 11. Honest Limitations & Residual Risk Statement

The product **deliberately** does **not** claim:

1. **"Installed packages are safe."** Nixpkgs is not audited/curated (T-RUN-1, T-IDX-4).
   We provide provenance and reproducibility, and a fast catalog-revocation lever. Runtime
   isolation of installed apps is **deferred** (DR-009 in `12`).
2. **"Immune to a compromised cache.nixos.org."** v1 trusts one binary source (T-CACHE-2).
3. **"Immune to local root."** A local root can always tamper with the store, state, and
   keys; we **detect** tampering and fail closed, we do not prevent root.
4. **"Index results are curated."** The index is convenience metadata; naming/typosquatting
   risk inside Nixpkgs is inherited (T-IDX-4).
5. **"Reproducibility = correctness."** Reproducible ≠ correct; a reproducible malicious
   build is still malicious.

These limitations are **features of the trust model**, not oversights. They are surfaced to
users in `06` (CLI copy / `doctor`), `10` (release notes), and tracked in `12`.

---

## 12. Implementation Checkpoints (PR-shaped; see `11` for full DAG)

- **CP-SEC-1** Channel crypto spike (TUF/tough fitness) → decision DR-002. (`11` spike S2)
- **CP-SEC-2** Path/symlink hardening in state/activation (`pkg-core`, `pkg-store`). (`11`)
- **CP-SEC-3** Authenticated helper + fail-closed unmanaged-Nix detection (`pkg-installer`). (`11`)
- **CP-SEC-4** Cache substitution hard-pinning + `restrict-eval` enforcement (`pkg-nix`). (`11`)
- **CP-SEC-5** Security test lane (tamper, replay, symlink, oversized, MITM fixtures) (`09`). (`11`)
- **CP-SEC-6** Release signing (offline root + threshold) and revocation rehearsal (`10`). (`11`)

---

## 13. Testable Acceptance Criteria

- **AC-S1** A signed channel descriptor with a `timestamp` older than its expiry is rejected
  with a user-facing "channel stale" message and `update`/`upgrade` are blocked (T-CHAN-2).
- **AC-S2** Replaying a valid-but-older `targets.json` is refused (version monotonicity)
  (T-CHAN-1).
- **AC-S3** A store path whose signature does not verify against the channel-trusted key set
  is **not** substituted (T-CACHE-1); `repair` re-verifies and restores integrity (T-CACHE-3).
- **AC-S4** A pre-created symlink in a product dir cannot redirect a state/activation write
  (T-PATH-1); a world-writable ancestor is rejected (T-PATH-4).
- **AC-S5** A second `pkg` instance is serialized via the writer lock; a crash mid-operation
  recovers to the last committed generation (T-STATE-2/3).
- **AC-S6** The privileged helper refuses unauthenticated callers and rejects any command
  outside its allowlist (T-INST-3/T-DAEMON-1).
- **AC-S7** On a host with an existing **unmanaged** Nix, install refuses with remediation
  text and **does not** modify or delete it (T-INST-4).
- **AC-S8** `cargo audit` + `cargo deny` are clean in the release gate (T-REL-4).
- **AC-S9** Logs contain no env/args/secret fields by default; redactor unit-tested with
  known-secret fixtures (T-LOG-1).
- **AC-S10** `pkg info <attr>` displays provenance (rev, narHash, substituter, build source)
  so T-RUN-1's residual risk is visible to users.
- **AC-S11** (Multi-user isolation, D-17) On a shared host, user A cannot read or modify
  user B's `<user-state>` (manifest/lock/generations/current/journal), and each user's GC
  roots live under their own `/nix/var/nix/gcroots/pkg/users/<uid>/` (T-INST-6).
- **AC-S12** (UID confusion) A process authenticated as uid A cannot cause the helper or
  daemon to create/repair GC roots (or touch state) under uid B's directories; the peer-cred
  uid is the only uid used (T-INST-3/T-DAEMON-1/T-INST-6).
- **AC-S13** (Full-closure cache preflight, macOS) An install whose closure has even one path
  without a `cache.nixos.org` binary fails preflight/acquire with `ACQUIRE_NO_BINARY` and
  **never** builds on macOS (D-11; plan 07 §16.4, plan 04 §5/§6).

---

## 14. Dependencies on Other Plans

| Depends on | Why |
|-----------|-----|
| `00` | Product goals/scope and the "Nix hidden" invariant define the threat surface. |
| `01` | Component layout, IPC contract, daemon socket model (T-DAEMON, T-INST). |
| `02` | Channel descriptor schema & signing model (T-CHAN, §7). |
| `03` | Index design & "disposable + narHash-pinned" (T-IDX). |
| `04` | Install pipeline purity/substitute/sandbox/approval controls (T-CACHE/T-BUILD/T-EVAL). |
| `05` | State atomicity, journal, GC roots, generations (T-STATE/T-PATH/T-CONC). |
| `06` | User-facing honesty copy, approval UX, `doctor` (T-RUN/T-IDX/T-BUILD-5). |
| `07` | Installer/helper/daemon/uninstall privilege model (T-INST/T-UNINST/T-PATH-3). |
| **Feeds** | `09` (security test lane), `10` (keys/incident/revocation), `11` (security PRs), `12` (DR/RISK). |

---

## 15. Primary Sources

- `[NIX-MANUAL]` Nix Reference Manual — https://nixos.org/manual/nix/stable/ (store model,
  binary cache security, sandboxed builds, daemon, `--verify`/`--repair`, `restrict-eval`).
- `[NIXPKGS-MANUAL]` Nixpkgs Manual — https://nixos.org/nixpkgs/manual/ (derivation model,
  review process, reproducibility caveats).
- `[NIX-SEC]` Nix manual Security chapter (binary caches, trusted users).
- `[NIXOS-SA]` NixOS security advisories — https://github.com/NixOS/nixpkgs/security and
  NixOS project security disclosures (supply-chain history).
- `[NIX-CVE]` Nix CVE history — https://github.com/NixOS/nix/security .
- `[HYDRA]` Hydra (build system behind cache.nixos.org) — https://nixos.org/hydra .
- `[TUF]` The Update Framework specification — https://theupdateframework.io/specification/latest/ .
- `[TOUGH]` `tough` Rust crate (TUF implementation, AWS Bottlerocket) —
  https://docs.rs/tough / https://github.com/awslabs/tough .
- `[SIGSTORE]` Sigstore (cosign/rekor) — https://www.sigstore.dev/ (considered, deferred).
- `[RB]` Reproducible Builds — https://reproducible-builds.org/ (per-derivation coverage).

---

## 16. Unresolved Questions (→ `12`)

- Q1 Threshold quorum size & rotation cadence for TUF root at GA (v1 = 1-of-1). (DR-002)
- Q2 Whether to ship a product-owned binary cache in v1 to reduce T-CACHE-2 (deferred to v2).
- Q3 Whether seccomp profile for builder isolation is mandatory on Linux (perf vs. hardening).
- Q4 Whether/when to add a vuln/CVE feed (T-RUN-2) — currently deferred.
- Q5 Runtime-isolation story (bubblewrap/firejail integration) for installed apps — deferred.
