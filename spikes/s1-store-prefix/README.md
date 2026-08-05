# Spike S1 — Nix store-prefix & coexistence (`spikes/s1-store-prefix/`)

PR-4 / Spike S1. Determines whether `pkg` V1 can use a private/nonstandard Nix store or
coexist with an unmanaged Nix installation while keeping **stock Nix**, **native package
execution**, and **standard `cache.nixos.org` reuse** on Linux **and** macOS — and
specifies a **fail-closed, read-only, install/preflight-only detector** for an existing
unmanaged Nix.

**Read [`findings.md`](findings.md)** for the full analysis, evidence, and the DR-001
status. This README is an index + safety/usage note.

## Contents

| File | Purpose |
|---|---|
| [`findings.md`](findings.md) | Question, requirements, methods, exact environment, executed + documented evidence, results (the three store-prefix distinctions), V1 decision, security implications, limitations, reproducible commands, citations. |
| [`detect-unmanaged-nix.sh`](detect-unmanaged-nix.sh) | The **unprivileged early read-only** install/preflight detector (POSIX `sh`, shebang `#!/bin/sh`). Scans `/nix`, `/etc/nix`, daemon socket/db, systemd units (incl. unreadable `*.wants` ⇒ ambiguous), launchd plists, `synthetic.conf`/`fstab`, APFS mount, `nixbld`/`_nixbld` users+groups (full `getent`; query failure ⇒ ambiguous), exported `NIX_*`/`IN_NIX_SHELL` env, profiles (POSIX-glob home enumeration), PATH binaries, and the pkg ownership marker. **Any positive or ambiguous artifact — including a lone marker — ⇒ `REFUSE` (exit 2)**; advisory refusal only — it never authorizes install. There is no runtime/mode recognition and no `PKG_PROBE_*` env bypass. |
| [`build-fixtures.sh`](build-fixtures.sh) | Library that materializes named fake-root fixtures (clean, existing-install-linux/macos, linux-service, macos-launchd, macos-apfs-synthetic-fstab, symlink-mount, nix-on-path, db-and-socket, profile-only with ONLY a SPACE in the user dir, product-marker-only, ambiguous/marker/group-unreadable). Enforces a **process-local capability** (canonical suite root + per-run token): fixture functions can only mutate case directories directly beneath a verified `mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"` suite root, and a hand-planted constant sentinel cannot authorize mutation. |
| [`run-tests.sh`](run-tests.sh) | Fixture-driven test harness: builds each fixture in a private `mktemp` scratch dir (capability-marked; cleanup re-verifies canonical root + token before any `chmod`/`rm`), runs the detector, asserts exit code + output signal, exercises env/JSON-hostile/parser-backed cases, the fixture-guard + detector safety guards, the fixture-suite capability regressions, the spaced case-root, the traversal-alias rejection, and the split remediation. |

## Safety contract

- The **detector** is **read-only**: it only stat/read/grep/list. It never calls `sudo`,
  never installs Nix or any package, and never `rm`/`mkdir`/`chown`/`chmod` on the target
  host, never creates/deletes/mutates `/nix`, `/etc/nix`, `/etc/fstab`,
  `/etc/synthetic.conf`, any service unit/plist, or any user account, and never
  stop/start a service or mount/unmount a volume. It never touches an existing Nix
  installation. Optional corroborators (`systemctl`/`launchctl`/`mount`/`getent`/`dscl`/
  `diskutil apfs list`) are restricted to read-only list/status queries, guarded behind
  `command -v`, scoped to the applicable OS, and run only on a real host (`/`).
- The **fixture harness** is **intentionally not read-only**: it creates/chmod/symlinks
  files and removes them on cleanup, but **only inside its own verified `mktemp` scratch
  tree** (`mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"`), gated by a **process-local
  capability** (canonical suite root + a per-run token written into the sentinel). Every
  primitive re-checks: capability initialized, case root is a real non-symlink dir,
  sentinel content == token, and the case root's canonical parent == the suite root — so
  a hand-planted constant sentinel cannot authorize mutation (NOT cryptographic; it
  prevents accidents/path confusion inside the 0700 mktemp tree). `build-fixtures.sh`
  refuses `/`, `/nix`, `/etc`, `/Library`, `/var`, `/Users`, symlink roots, relative roots,
  missing/non-dir roots, arbitrary repo/home roots, non-empty dirs, and wrong temp
  naming/parent **before any `mkdir`/`chmod`/write/symlink**. `fx_init_suite` also
  **fails closed** if `find` is unavailable or exits nonzero (it refuses to initialize
  a suite whose emptiness it cannot verify, before writing the sentinel), and
  allowlists the **canonical TMPDIR parent** to exactly `/tmp`, `/private/tmp`,
  `/var/tmp`, `/private/var/tmp`, or a macOS per-user root
  `/private/var/folders/<one>/<two>/T` (two nonempty slash-free components — a `case`
  glob alone would let `*` span slashes and accept deeper descendants); a
  user-controlled TMPDIR outside these canonical roots is refused before any
  sentinel is written. (The earlier blanket claim that "all scripts never call
  `mkdir`/`chmod`/`rm`" was false and is removed.)
- The detector rejects unsafe roots (`/nix`, `/etc/nix`, …), relative paths, missing
  paths, any non-`/` symlink root, and roots containing `.`/`..` segments or whose
  canonical target is an unsafe scanned/system subtree; it accepts only the literal `/`
  as a symlink. A normal macOS `/var/folders` temp root is NOT rejected (the `/var` →
  `/private/var` resolution is handled). Home enumeration is **platform-aware**: on a
  real macOS host (`ROOT=/` + `Darwin`) the OS-standard `/home` firmlink is skipped ONLY
  after `is_standard_macos_home_firmlink` VERIFIES that `/home` is a symlink whose
  canonical target resolves to EXACTLY `/System/Volumes/Data/home` (an OS-level firmlink,
  not user-controlled and not a Nix artifact), so it is not mis-reported as
  `HOME_ROOT_SYMLINK`; normal macOS homes (`/Users`, `/var/root`) are scanned. If `/home`
  is absent, a real directory, or a symlink to a different/unresolvable target, it is
  scanned normally (and a symlink is still refused). On Linux and macOS **fake roots**
  the full set incl. `/home` is scanned, so a test-controlled `/home` symlink is still
  refused (`HOME_ROOT_SYMLINK`). Current-`$HOME` coverage is consistent: when the verified
  `/home` link is skipped, a custom `$HOME` under `/home/*` or `/root/*` is still scanned
  (only `/Users`/`/var/root` were enumerated); otherwise the standard-root dedup applies.
- There is **no `--force`** and **no `--mode`** in V1; any ambiguous/unreadable state ⇒
  refuse. There are **no `PKG_PROBE_*` env bypass knobs**: a user-controlled environment
  cannot disable a fail-closed corroborator (live corroborators run only at literal `/`).

## Run

```sh
# Fixture-driven suite (private mktemp scratch under TMPDIR; capability-gated; cleans up).
sh spikes/s1-store-prefix/run-tests.sh

# Scan the real host (read-only). This is the UNPRIVILEGED early scan: advisory refusal
# only — it never authorizes install. As a non-root user on macOS it REFUSES on the
# unreadable /var/root with an ambiguity-only advisory (no removal instructions; a
# privileged read-only recheck before mutation is the only thing that can authorize).
# A Nix-free host yields CLEAN only in the privileged installer context; that privileged
# macOS CLEAN is unvalidated in this spike. See findings.md §5.2/§5.7.
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root /

# Help / exit codes.
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --help
```

Exit codes: `0` clean · `2` refuse (unmanaged or ambiguous finding, incl. a lone pkg
ownership marker) · `64` usage error (unknown arg; bare `--root` with no value;
relative/missing/non-dir/unsafe root; a non-`/` symlink root; the removed `--mode`).

## Environment & output

The detector flags **any exported** `NIX_*` variable (empty-valued included) and an
exported `IN_NIX_SHELL` as a refusal. Detection is **presence-only**: it does **not**
parse, count, or echo variable **names** (the POSIX `env` output is line-oriented, so a
value containing a newline can introduce an indistinguishable `NIX_FOO=...` line; echoing
such an extracted "name" would leak value-derived text). It emits a single fixed,
generic, redacted message when one-or-more `NIX_*` entries are conservatively detected;
**values** (and value-derived text) are never persisted or reflected in any
finding/output — the environment is queried with two direct `env | grep` pipelines and is
never captured into a variable — there is no debug flag for values, and no `eval` is used.
If `env` itself fails, the detector fails closed (`ENV_QUERY_FAILED`) rather than report
clean. Honest residual: a non-`NIX` variable whose
multiline value contains a `NIX_SOMETHING=...` line can cause a conservative
false-positive refusal — inherent to line-oriented env serialization, and preferred over
any value-derived leak. Machine-readable JSON carries only signal IDs and fixed/redacted
messages (no env names, counts, values, file contents, symlink targets, or resolved
paths) and is defensively JSON-escaped, so hostile inputs cannot produce invalid JSON.
There is no install-time `/opt/pkg/**` PATH whitelist: any Nix binary reachable on PATH
before installation is a refusal.

## Portability

POSIX `sh` (shebang `#!/bin/sh`). Verified on `dash`, macOS system `bash` 3.2, `zsh 5.9`,
and busybox `ash` (Alpine). No arrays, no bashisms, no `local`, no non-POSIX
`find -maxdepth`; `awk` (POSIX) is used only for JSON string escaping. Home enumeration
is whitespace-safe via POSIX quoted globs (handles spaces and newlines in user/home-root
names); no `ls` parsing and no unquoted command substitution. When the harness is
invoked under `zsh` it emulates POSIX `sh` for the sourced fixture library (the detector
itself always runs under `/bin/sh` via its shebang).

`shellcheck` was **absent** on the development host — this is a **recorded validation
gap**, not "clean by construction". Run it locally if available:
`shellcheck spikes/s1-store-prefix/*.sh`.

## Status

**DR-001 is `Proposed`.** The spike's technical recommendation and evidence are complete
(see `findings.md` §9), but the DR remains **Proposed, not Accepted**: per
`plans/11` §2 / `CONTRIBUTING` §5 a spike DR is `Accepted` only after the spike owner and
the affected area owners (F, E, and A) sign off. That recorded sign-off has not happened,
so the AC-D1 gate is **not** cleared by this spike. Two honesty caveats remain: (1) no
human F/E/A sign-off is claimed; (2) no real-Nix install / privileged installer run /
real-Nix'd host was executed — what was validated, exactly: Nix-free macOS non-root
safe ambiguity/refusal; root Alpine CLEAN; positive artifacts fixture-driven; privileged
macOS CLEAN not run.
Cross-file authoritative ownership: only `spikes/s1-store-prefix/**` and the DR-001 entry
in `plans/12-open-decisions-and-risks.md` are touched by this spike.
