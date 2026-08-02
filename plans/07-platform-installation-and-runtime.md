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
| I5 | All privileged operations go through a single auditable helper/daemon path; the product binary itself is **not setuid**. |
| I6 | Uninstall is **explicit and total only** (removes product + managed Nix + state). There is no partial uninstall that leaves a half-managed Nix. |

## 4. Architecture & `system` detection

The product maps host to a Nix `system` triple (*confirmed* [^system]):

| Host | `system` | Build mode |
|------|----------|------------|
| x86_64 Linux | `x86_64-linux` | substitute; local build allowed (Linux) |
| aarch64 Linux | `aarch64-linux` | substitute; local build allowed |
| x86_64 macOS (Intel) | `x86_64-darwin` | substitute only (binary-only) |
| aarch64 macOS (Apple Silicon) | `aarch64-darwin` | substitute only; **Rosetta not used** for x86_64-darwin paths |

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
`<daemon-state>/nix.conf` referenced by the daemon. Minimal v1 contents:

```ini
build-users-group = nixbld            # Linux; macOS uses its own build users
sandbox = true                       # Linux builds; macOS binary-only so n/a
sandbox-fallback = false             # fail closed, never build unsandboxed
require-sigs = true
substituters = https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59...  # from descriptor (plan 02 §6.5/§7)
connect-timeout = 10
max-substitution-jobs = 4
# The following are deliberately ABSENT / not user-settable:
#   extra-substituters, trusted-substituters, builders-use-substitutes
```

The product **rewrites** this file atomically whenever the channel descriptor
changes (plan 02). Any user attempt to edit it is detected by `doctor`
(checksum) and the product refuses ops until reconciled (repair rewrites from
descriptor). This is the trust enforcement point (plan 02/08).

### 5.3 Daemon

The bundled Nix runs as a **daemon** (`nix-daemon`):

- **Linux:** a systemd service `pkg-nix-daemon.service` running as
  `root`/dedicated `pkg-nix` user, listening on the canonical Unix socket
  `/nix/var/nix/daemon-socket/socket` (plan 01 §9.1; configurable via
  `NIX_DAEMON_SOCKET_PATH`). Because the installer refuses any unmanaged Nix
  first (§8 / D-04), `/nix/var/nix` is exclusively ours, so the standard
  socket path is safe; the store-prefix spike (§6.2 deliverable #4) confirms
  co-location is collision-free. The product connects via
  `NIX_REMOTE=unix:///nix/var/nix/daemon-socket/socket` to **this** socket
  only. Build users: a `nixbld` group + `nixbld1..N` users created by the
  installer (*confirmed multi-user model* [^multi-user]).
- **macOS:** a `launchd` daemon
  (`system` domain `org.pkg.nix-daemon.plist`) for the same role. macOS has
  no build sandbox in the Linux sense; since v1 is **binary-only on macOS**
  the daemon substitutes only and never invokes a builder. (Sandbox-exec
  exists but is out of scope for v1 — Q7.4.)

The product's CLI always talks to **its own daemon socket**, configured via
generated `nix.conf` + env at the adapter call site (plan 04). It never relies
on a system `$PATH` `nix`.

### 5.4 Process & resource limits (recap from plan 04 §8)

Enforced via `nix.conf` + per-call flags: `max-jobs`, `cores`,
`max-silent-time`, `timeout`, `system-features`. The installer picks sane
defaults from host capabilities (CPU count, RAM).

## 6. Store, prefix, and the `/nix/store` spike (§9)

### 6.1 Store layout

- Store root: **`/nix/store`** (stock Nix; I2).
- Daemon state (profiles, gcroots, temproots, db): `/nix/var/nix/...` per
  multi-user layout [^multi-user], or a product-scoped equivalent if the spike
  allows; otherwise the standard paths.
- **Product GC roots (per-user, D-17/ARCH-INV-04):** the product creates one
  root per retained generation under
  `/nix/var/nix/gcroots/pkg/users/<uid>/gen-<id>` (root-owned tree; the
  authenticated root-helper writes a symlink scoped to the caller's uid —
  §7.4). The Nix collector scans `/nix/var/nix/gcroots` [^gc-roots]. Detail
  & topology in plan 05 §8.3.
- **Product state split (D-17/INV-10; canonical layout plan 01 §9.2/§9.3):**
  - **Machine-global SERVICE state** (root-owned, shared, read-only to users)
    under `/var/lib/pkg/` (Linux) / `/Library/Application Support/pkg/`
    (macOS): `channel/` (TUF + descriptor), `index/<seq>/`, `nixpkgs/<rev>/`,
    `cache/`, `log/`. This is the runtime/channel/index/source service —
    **not** package environment state.
  - **Per-user authoritative package state** `<user-state>` (owned by the
    invoking uid, mode 0700): `manifest.json`, `lock.json`, `generations/`,
    `current`, `journal/`, `cache/`, `log/`. Linux: `$XDG_DATA_HOME/pkg/`
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
2. Create `pkg-nix` service user/group and `nixbld` group + `nixbld1..16`
   users (multi-user build isolation) [^multi-user].
3. Create `/nix` (root:root `0755`) and `/nix/store`, `/nix/var/nix/...`.
4. Extract bundled Nix to `/opt/pkg/nix`; set up `pkg-nix-daemon.service`
   (systemd unit) pointing at our socket and `nix.conf`.
5. Write root-owned `nix.conf` from the **bundled channel descriptor** (plan
   02) — installer ships an initial descriptor+signature for bootstrapping.
6. Enable + start the daemon; `nix ping-store` health check [^ping-store].
7. Install the `pkg` binary to `/usr/local/bin/pkg` (or `/opt/pkg/bin`) and
   create the **root-owned service root** `/var/lib/pkg` (channel/index/source/cache/log).
   **Per-user authoritative state** `<user-state>` (D-17) is **not** created by
   the installer; each user's CLI lazily creates their own `<user-state>` (owned
   by that uid, mode 0700) on first run.
8. **PATH integration (§10):** write `/etc/profile.d/pkg.sh` and shell-rc
   snippets; print next-step instructions.
9. Write an **uninstall manifest** (`/opt/pkg/uninstall/manifest.json`) listing
   every path/user/unit created, for total uninstall (§11).

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
2. Create `/nix` (root:admin), `/nix/store`, `/nix/var/nix/...`. (Standard Nix
   on macOS still uses `/nix`.)
3. Extract bundled Nix to `/opt/pkg/nix`.
4. Install `org.pkg.nix-daemon.plist` into `/Library/LaunchDaemons` and
   `launchctl load`. Daemon runs as root, substitutes only (binary-only).
5. Write root-owned `nix.conf` (sandbox lines omitted; substituters/keys from
   descriptor).
6. Install `pkg` to `/usr/local/bin/pkg` (or `/opt/pkg/bin`).
7. PATH integration (§10); uninstall manifest.

**macOS binary-only enforcement:** the macOS daemon is configured with
`max-jobs = 0` and no builder users; any attempt to build returns an error the
product maps to `ACQUIRE_NO_BINARY` (plan 04/06). Because no build is possible,
install relies on plan 04's **full-closure cache preflight**: preflight
classifies **every** path in the recursive closure against `cache.nixos.org`
and only reports "binary available" if **all** closure paths are cache hits; a
single missing binary fails the op with `ACQUIRE_NO_BINARY` **before**
activation (plan 04 §5/§6). `pkg info`/`pkg install --dry-run` surface this up
front so macOS users never reach a partial activation.

### 7.4 Privilege & daemon protocol

- The **product CLI runs as the invoking user**; it connects to the daemon via
  the Unix socket. The daemon performs privileged store ops.
- Where the product needs to change root-owned config (`nix.conf`, install,
  uninstall, daemon restart) it shells to a **small privileged helper**
  (`pkg-root-helper`) invoked via `sudo`/polkit/AuthorizationServices with a
  fixed allowlist of subcommands — auditable in plan 08. The CLI binary is
  **not** setuid (I5).
- The daemon socket is mode `0660`, group `pkg-users` (the invoking user is
  added to this group at install). On single-user boxes this is the primary
  user.
- **UID-authenticated privilege boundary (D-17/ARCH-INV-06):** the daemon and
  root-helper authenticate the calling user via socket peer credentials
  (`SO_PEERCRED` on Linux; `getpeereid` / launchd `Audit Token` on macOS). The
  **only** privileged store-side write performed on a user's behalf is GC-root
  creation/repair under that user's own
  `/nix/var/nix/gcroots/pkg/users/<caller-uid>/` — never under another uid's
  dir, and never reading/mutating another user's `<user-state>` (plan 01 §8,
  plan 05 §8.3). Authoritative package state (manifest/lock/generations/
  activation/journal) is per-user and never globally shared (INV-10).

## 8. Unmanaged-Nix detection & refusal (V1 policy)

**Policy (plan 00):** V1 takes exclusive managed ownership; on detecting any
existing unmanaged Nix, **refuse** with manual remediation; **never
auto-delete.**

**Detection signals (any non-empty ⇒ refuse):**

| Signal | Linux | macOS |
|--------|-------|-------|
| `/nix` exists and not owned by our service user | ✓ | ✓ |
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

## 9. The store-prefix spike (consolidated)

(Specified in §6.2.) This is the **single highest-risk unknown** for the
substrate; it gates the installer design and is placed first on the PR
critical path in plan 11. Until it closes, all path choices here are
conditional.

## 10. Shell PATH integration

The active generation's activation tree exposes `bin/`, `sbin/`, `share/man`,
etc. (a Nix `buildEnv` tree, plan 04). PATH integration points each user's
shell at their own `<user-state>/current/bin` (the `current` symlink → the
per-user activation store path; plan 01 §9.3, plan 05).

**Per-user PATH (D-17):** each user's activation is their own
`<user-state>/current` (a Nix `buildEnv` store object; plan 01 §9.3, plan 04
§5.5), so PATH integration points at the **invoking user's** own
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
   plan 11 PR-12/27/28).
2. Refuses if any unmanaged-Nix signals are present that we didn't create
   (safety: don't touch a `/nix` someone else populated).
3. Stops & removes our daemon (systemd/launchd).
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
   (macOS), the `pkg` binary, profile.d snippets, users
   & groups (`pkg-nix`, `nixbld*`) created by us.
8. Prints a final summary.

**Partial uninstall is intentionally unsupported** (I6). If the user wants to
keep packages but remove the product, that's not a V1 path (they'd lose the
manager) — documented as such.

**Uninstall does not** touch: Homebrew, the user's dotfiles beyond our managed
snippets, or any foreign `/nix`.

## 12. Security considerations (full model: plan 08)

- Installer & root helper are the highest-privilege code paths — minimal,
  audit-targeted, no network in the helper, fixed allowlist of subcommands.
- `nix.conf` is root-owned and checksummed; tampering ⇒ refuse ops.
- Build users (`nixbld*`) are created with no login shell, no password,
  confined home; sandbox on (Linux).
- The daemon socket is group-restricted; membership granted only to intended
  users at install.
- No `setuid` binaries anywhere (I5).
- macOS binary-only removes a whole class of build-time risk on that OS.
- Uninstall is conservative about `/nix` to avoid destroying foreign data.

## 13. Failure matrix (selected)

| Scenario | Behavior |
|----------|----------|
| Unmanaged Nix present at install | Refuse (exit 74), print remediation, change nothing. |
| `/nix` not writable by daemon | Installer fails preflight; no partial state. |
| Daemon socket unreachable at runtime | `doctor` fails that check; mutating verbs exit 77. |
| User edits `/etc/nix/nix.conf` | `doctor` detects checksum mismatch; ops exit 78 until `repair` rewrites from descriptor. |
| `nix.conf` trust knobs overridden via env | Ignored; `doctor` warns if env set. |
| Unsupported arch | Installer refuses with support-matrix link. |
| Rosetta-only Mac | Reported as `aarch64-darwin`; no x86_64-darwin fallback. |
| Uninstall with foreign paths in `/nix` | Leaves `/nix`, prints notice; removes only our files. |
| Daemon crash mid-op | Nix store is durable; product recovers via journal (plan 04/05). |
| macOS path without `aarch64-darwin` binary | `ACQUIRE_NO_BINARY` (plan 04); never builds. |

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
  fixture → refuse.
- **PR-P4 — macOS installer (launchd, `.pkg`, paths).** *Acceptance:* clean
  install on fresh macOS VM; binary-only enforced (`max-jobs=0`).
- **PR-P5 — Privileged root helper (fixed allowlist).** *Acceptance:* every
  subcommand unit-tested; no network in helper; plan-08 audit checklist met.
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
4. Linux local builds run sandboxed under `nixbld*` users; macOS never builds
   (`max-jobs=0`) and **any** closure path missing a `cache.nixos.org` binary
   yields `ACQUIRE_NO_BINARY` (full-closure preflight, plan 04 §5/§6).
5. After install, a new login shell has `<user-state>/current/bin` on PATH on both
   OSes; `pkg doctor` confirms and warns if shadowed.
6. `pkg self-uninstall --yes` on a sole-manager host leaves no `pkg` files,
   units, users, or `/nix` (when empty of foreign paths); on a host with
   foreign paths it leaves `/nix` and prints a notice.
7. Editing `/etc/nix/nix.conf` is detected by `doctor`; ops refuse (78) until
  `repair` rewrites it from the descriptor.
8. `NIX_SUBSTITUTERS`/`NIX_TRUSTED_PUBLIC_KEYS` set in the environment are
   ignored by the product and reported by `doctor`.
9. Architecture detection correctly reports `aarch64-darwin` on Apple Silicon
   and does not substitute `x86_64-darwin` paths.

## 17. Unresolved questions / spikes

- **Q7.1 Store-prefix spike outcome** (§6.2/§9). *Blocking. Default if
  inconclusive: depend on `/nix/store`, multi-user install.*
- **Q7.2 Single-user mode.** Defer; multi-user chosen for uniformity/sandbox.
  *(Plan 12.)*
- **Q7.3 musl/Linux-static.** v1 targets glibc `*-linux`; musl is a spike
  (affects bundled-Nix build and `system`). *(Default: defer.)*
- **Q7.4 macOS sandbox-exec.** Out of scope (binary-only makes it moot).
  *(Default: not used.)*
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
