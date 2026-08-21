# Spike S1 — Nix store-prefix & coexistence (PR-4 → DR-001)

| | |
|---|---|
| **Spike** | S1 (PR-4) — *Is `/nix/store` viable for exclusive managed use, and how do we detect / safely refuse an unmanaged Nix?* |
| **Decision it feeds** | DR-001 (`plans/12-open-decisions-and-risks.md`). **Accepted 2026-08-09 after F/E/A review; see §9 and the acceptance limits in DR-001.** |
| **Owner (spike)** | This directory only: `spikes/s1-store-prefix/**`. |
| **Safety** | The **detector is read-only**; the **fixture harness intentionally creates/mutates/cleans only its own verified mktemp scratch tree**. No production code, no `sudo`, no install, no mutation of `/nix`/`/etc`/services. See `detect-unmanaged-nix.sh` header for the safety contract. |
| **Evidence labels** | **(a)** actually executed · **(b)** official docs/source inspected · **(c)** inference (clearly marked). F/E/A sign-off was recorded on 2026-08-09; no privileged clean-host installer authorization is claimed. |

---

## 1. Question

Can `pkg` V1 use a **private/nonstandard Nix store** or **coexist with an unmanaged
Nix installation** while retaining, on **both Linux and macOS**:

1. **stock Nix** (a pinned upstream build, unmodified — per DR-014 `pkg` does not
   compile/patch Nix),
2. **native execution** of installed packages on the host (activation symlinks point
   into the store and the user runs them directly — `plans/00` D-12/D-16, `plans/05`),
3. **standard binary-cache reuse** (`cache.nixos.org` is the *only* artifact cache in
   V1 — `plans/00` D-10, DR-006)?

And, separately: how must a V1 installer **detect** an existing unmanaged Nix and
**fail closed** (never auto-delete — `plans/00` D-04, `plans/07` I1, `plans/08` T-INST-4)?

The phrase in earlier plans — *"stock Nix is not relocatable at all"* — is **too broad**.
This spike corrects it precisely (§7) and tests whether any supported option actually
meets all three V1 requirements above.

---

## 2. Requirements (the bar an option must clear)

An acceptable store layout for V1 must satisfy **all** of:

- **R1 Stock Nix.** Works with a pinned, unmodified upstream Nix tarball (DR-014). No
  custom patch, no custom compile-time store-dir.
- **R2 Cross-platform.** Identical model on `x86_64-linux`, `aarch64-linux`,
  `x86_64-darwin`, `aarch64-darwin` (`plans/00` D-14).
- **R3 Native execution.** Installed binaries execute on the host via the standard
  activation symlink tree into the store (`plans/05`; `plans/07` §6.1). The product
  must not require wrapping every invocation in a `chroot`/container.
- **R4 Standard cache reuse.** Paths substitute verbatim from `cache.nixos.org`
  (Ed25519-verified NARs whose store paths are `/nix/store/<hash>-<name>`) — D-10/DR-006.
- **R5 Exclusive & safe.** `pkg` exclusively owns its store; it never silently shares
  `/nix` with a foreign Nix, and it never deletes a foreign install (D-03/D-04, T-INST-4).

---

## 3. Methods

1. Inspect the **current official Nix documentation** for the local store, multi-user
   mode, and installation (rendered pages, exact versions recorded in §6).
2. Inspect the **Nix source** at a pinned release tag/commit (§6): the installer scripts
   (authoritative for the on-disk layout an unmanaged Nix creates) and the Meson build
   option that establishes the compile-time store-dir.
3. Build a **read-only, fail-closed, install/preflight-only detector**
   (`detect-unmanaged-nix.sh`) that scans for every artifact an unmanaged Nix leaves
   behind, plus ambiguous/unreadable state. It has one purpose: prove a host is
   Nix-free before a privileged installer runs. Any Nix artifact — including a lone
   pkg ownership marker — is a refusal.
4. Drive the detector with **fake-root fixtures** covering clean / existing-install /
   ambiguous-unreadable / marker-only / Linux-service / macOS-launchd / APFS-synthetic-fstab
   / env / symlink / spaced-profile / unreadable-group cases (`build-fixtures.sh`,
   `run-tests.sh`).
5. Run the detector against the **real host** `/` on this machine (read-only), and
   inside a **real Linux container**, to obtain executed (not simulated) evidence on
   both platforms.

**Honesty about mutation.** The detector is read-only by construction (it only
stat/read/grep/list; it never calls `sudo`/install/`rm`/service-stop and never mutates
`/nix`, `/etc`, services, or accounts). The **fixture harness is intentionally not
read-only**: it creates/chmod/symlinks files and removes them on cleanup, but **only
inside its own verified `mktemp` scratch tree** (`mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"`),
gated by a **process-local capability** (canonical suite root + a per-run token written
into the sentinel) so fixture functions can mutate only case directories directly
beneath that root, and a hand-planted constant sentinel cannot authorize mutation
(`build-fixtures.sh` §"Capability model", §4; **not cryptographic** — it prevents
accidents/path confusion inside the 0700 mktemp tree). The earlier blanket claim that
"all scripts never call `mkdir`/`chmod`/`rm`" was false and is removed.

`shellcheck` was **absent** on this host; this is a **recorded validation gap**, not
"clean by construction". POSIX `sh -n` and cross-shell runs (§4/§5) are the executed
substitute. Run shellcheck locally if available.

---

## 4. Exact environment (executed)

| Item | Value |
|---|---|
| Host OS / arch | macOS (Darwin 25.6.0, `arm64`, Apple Silicon) |
| Shells exercised | `dash`, macOS system `bash` **3.2.57**, `zsh` 5.9, busybox `ash` (Alpine) |
| JSON parsers exercised | `jq` 1.7.1, `python3` 3.11 (macOS lane); **none** on the Alpine lane (parser-only cases SKIP there) |
| Nix on host | **None** (`nix`, `nix-daemon`, `nix-store` absent; no `/nix`, no `/etc/nix`, no `_nixbld`/`nixbld`, no `NIX_*`/`IN_NIX_SHELL` env, no launchd `org.nixos.*`) — confirmed read-only in §5.1 |
| Linux container | `alpine:3.20` on `Linux aarch64` (Docker 29.4.1); Nix-free. Post-hardening Alpine lane executed with `--pull=never` (the `alpine:3.20` image was present locally for this final verification; this run did not download). |
| Network | Used **only** to fetch official docs/source for citation (read-only GETs) |
| `shellcheck` | **Absent** (recorded gap) |
| `getent`/`systemctl` | Absent on macOS (scoped out of real-host corroborators by OS detection) |

---

## 5. Executed evidence (a)

### 5.1 This host is a genuinely Nix-free Mac (read-only probe)

```
$ for p in /nix /nix/store /nix/var/nix /etc/nix /etc/nix/nix.conf /etc/synthetic.conf /etc/fstab; do
    { [ -e "$p" ] || [ -L "$p" ]; } && echo "PRESENT $p" || echo "absent  $p"
  done            # -> all "absent"
$ grep -cE '_nixbld' /etc/passwd          # -> 0
$ grep -cE '_nixbld|nixbld' /etc/group    # -> 0
$ command -v nix nix-daemon nix-store      # -> none
$ env | grep -E '^(NIX_|IN_NIX_SHELL)'    # -> none
```

Note: `/var/root` (root's home) exists but is **not readable** to a non-root user
(mode `0700`, root-owned). This matters for the detector below.

### 5.2 Detector against the real host `/` as a non-root user — REFUSE (exit 2)

```
$ sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root /
Scanning root "/" (mode=install)
  [ambiguous] HOME_ROOT_UNREADABLE any   a home root is not readable; cannot enumerate
                                         user Nix profiles. pkg refuses rather than guess.
summary: 1 finding(s): 0 unmanaged, 1 ambiguous, 0 marker (mode=install)
RESULT: REFUSE
$ echo $?        # -> 2
```

The single finding is the unreadable `/var/root` (root's home), not a Nix artifact.
This is **correct fail-closed behavior** and an **improvement** over the prior detector,
which used `find … -maxdepth 2 2>/dev/null` and *silently ignored* unreadable home
roots — exactly the "silently clean" failure the reviewer flagged. The host is still
Nix-free (§5.1); the detector simply cannot enumerate root's home as a non-root user,
so it refuses rather than guess. This is an **ambiguity-only** result, so the detector
prints the **advisory refusal** that explicitly does NOT instruct any removal and that
demands a full read-only **privileged** preflight recheck before mutation (§5.7). A
Nix-free host would yield CLEAN only in that privileged context; **that privileged
macOS CLEAN was not executed in this spike (inference (c))** — no `sudo`/privileged run
is claimed.

**macOS standard `/home` firmlink skip (VERIFIED target, not assumed).** On this host
`/home` is the standard macOS firmlink → `/System/Volumes/Data/home` (verified:
`readlink /home` → `/System/Volumes/Data/home`; an OS-level firmlink from the system
firmlinks list, not user-controlled and not a Nix artifact). A prior build of the
detector treated this OS-standard symlink as `HOME_ROOT_SYMLINK`, which made **every**
real macOS host unscannable past that point — so even a privileged CLEAN was
unreachable. The detector now skips `/home` ONLY when `is_standard_macos_home_firmlink`
CONFIRMS — on a real macOS host (`ROOT="/"` + `Darwin`) — that `/home` is a symlink AND
its canonical target, resolved with the existing POSIX `resolve_canon` (`cd -P` + `pwd
-P`), is EXACTLY `/System/Volumes/Data/home`. This REPLACES the earlier blind "every
real-host Darwin `/home` is the standard link" assumption: if `/home` is ABSENT, a REAL
DIRECTORY, a symlink to a DIFFERENT or UNRESOLVABLE target, or this is not a real macOS
host, `/home` is scanned normally and a symlink `/home` is still recorded
`HOME_ROOT_SYMLINK` (fail-closed). On Linux (real `/home` dir) and on macOS **fake
roots** (test-controlled `/home` symlink) the predicate returns 1, so `/home` is still
scanned and a fake-root `/home` symlink is still refused (`HOME_ROOT_SYMLINK`; proven
end-to-end by the `home_symlink_root_refused` case, with the broken/unresolvable-
target branch pinned by `home_broken_symlink_refused`). The verified skip itself is pinned
by the `darwin_home_firmlink_skip_realhost` regression: the read-only `--root /` scan on
this Darwin host does NOT emit `HOME_ROOT_SYMLINK` (it still REFUSES exit 2 on the
unreadable `/var/root`, which is the expected ambiguity here). **Current-`$HOME`
coverage (made consistent):** when the verified `/home` link is skipped, only `/Users`
and `/var/root` were enumerated, so a custom current `$HOME` under `/home/*` or `/root/*`
is STILL scanned (only `/Users`/`/var/root` and their children are de-duplicated); in
every other case the standard-root de-dup is retained, and a symlinked custom `$HOME` is
still refused there.

**Corroborator honesty (this host).** The real-host macOS corroborators all ran
successfully and found **no** Nix on this host: `launchctl list` (exit 0, no
`nixos.`/`nix-daemon` match), `diskutil apfs list` (exit 0, no `Nix Store` volume),
`dscl . -list /Users` and `/Groups` (exit 0, no `_nixbld`/`nixbld`), and `mount` (no
`/nix` mount). They therefore add **no ambiguity here**. This is host-specific: on a
host where `launchctl`/`diskutil`/`dscl` are restricted or return nonzero, the detector
treats that as an ambiguous refusal (e.g. `LAUNCHCTL_QUERY_FAILED`), which would be
recorded explicitly rather than presented as a single-finding result.

### 5.3 Detector against a real Linux `/` (Alpine container, runs as root) — CLEAN (exit 0)

```
$ docker run --pull=never --rm -v "$PWD/spikes/s1-store-prefix:/s:ro" alpine:3.20 \
    sh -c 'sh /s/detect-unmanaged-nix.sh --root / >/dev/null 2>&1; echo exit=$?; uname -sm'
exit=0
Linux aarch64
```

As root in the container, every home root is readable and the host is Nix-free, so the
result is CLEAN — demonstrating the fail-closed logic resolves to clean when state is
fully inspectable.

**Latent `set -e` fix surfaced by this lane.** Re-running this lane post-hardening
initially produced `exit=1` (a crash), not `exit=0`: `check_profiles` ended with
`[ "$pd_hit" -eq 1 ] && record …`, so on a clean host that has a non-Nix `/etc/profile.d`
(Alpine has `20locale.sh`, `README`, …) the function returned spurious nonzero and the
standalone `check_profiles` call aborted under `set -e`. (This did not manifest on the
macOS dev host, whose `/etc/profile.d` is absent.) The tail was converted to
`if/then/fi` so the function cannot return spurious nonzero; Alpine then correctly
returns `exit=0`. This is a spike-internal correctness fix in `detect-unmanaged-nix.sh`
(no production code); it is the only behavior change beyond the six accepted issues.

### 5.4 Fixture-driven test suite

macOS lane (executed under `sh`, `dash`, `/bin/bash` 3.2, and `zsh` — identical
results):

```
$ sh spikes/s1-store-prefix/run-tests.sh
… 63 cases: clean; existing-install-linux/macos; linux-service; macos-launchd;
   macos-apfs-synthetic-fstab; symlink-mount; nix-on-path; db-and-socket; profile-only
   (ONLY a spaced user dir, no ordinary profile); product-marker-only; ambiguous/marker/
   group unreadable; a CASE ROOT whose path itself contains a space; NIX_CONFIG /
   NIX_REMOTE_SYSTEMS / NIX_FUTURE_VARIABLE (set + empty) / IN_NIX_SHELL env refusals;
   the env PRESENCE-ONLY regression (a NON-NIX var whose multiline value injects a fake
   `NIX_SECRET_FROM_VALUE=...` line: refused, but neither the injected NAME nor the
   secret VALUE appears, in text AND JSON); JSON smoke; hostile-env + hostile-fs JSON
   (parser-backed); usage guards (unsafe/relative/nonexistent roots, bare --root,
   --mode install/runtime/bare, unknown arg, symlink root, dot-dot traversal alias);
   fixture-library guard (/ + relative + missing); fixture-suite capability regressions
   (fx_init_suite refuses /, /etc, a repo dir, a wrong-prefix temp dir; no sentinel
   created at /; a hand-planted constant sentinel — no-capability, tampered-token, and
   nested-dir — all refused; fx_init_suite / fx_cleanup_suite missing-argument
   regressions (exit 64 / nonzero return, no unbound crash under set -u); a missing-HOME
   regression (env -i PATH-only clean fake root scans CLEAN, no unbound error));
   fx_init_suite FAIL-CLOSED when `find` is stubbed to fail (a shell-function `find`
   override; exit 64 before any sentinel, suite dir left untouched); the TMPDIR macOS
   exact-component unit (accepts precisely /private/var/folders/<one>/<two>/T, rejects
   deeper descendants); an EXECUTION-GUARD regression (a differently-named symlink to the
   detector created in the scratch tree still runs main and rejects --root /nix at 64 —
   the removed basename guard would have silently exited 0); the macOS standard /home
   firmlink skip as a Darwin real-host regression (the read-only --root / scan does NOT
   emit HOME_ROOT_SYMLINK; is_standard_macos_home_firmlink VERIFIES the canonical target
   is exactly /System/Volumes/Data/home, rather than assuming every real-host Darwin
   /home is the standard link); a BROKEN (unresolvable) home-root symlink that the old
   `[ -d ]`-only gate would have silently skipped is now admitted by the directory-OR-
   symlink presence gate and refused as HOME_ROOT_SYMLINK without traversal
   (home_broken_symlink_refused, alongside home_symlink_root_refused for a symlink to
   an existing target); remediation split (ambiguity-only has no uninstall +
   names a privileged read-only recheck; definite evidence gives bounded
   vendor-uninstall guidance).
RESULT: 63 passed, 0 failed, 0 skipped.       # parser-backed JSON cases RUN (jq present)
```

Alpine container (busybox `ash`, runs as root): **executed.** The post-hardening Alpine
lane was run with `--pull=never`, which proves this run did not download the image (the
`alpine:3.20` image was present locally for this final verification):

```
$ docker run --pull=never --rm -v /Users/pacific/Developer/pkg/spikes/s1-store-prefix:/s:ro alpine:3.20 sh /s/run-tests.sh
…
RESULT: 55 passed, 0 failed, 10 skipped.
   #  6 SKIP: root/unreadable ambiguity (running as root: unreadable not meaningful) —
   #          ambiguous_unreadable, marker_unreadable, group_unreadable,
   #          root_uninspectable, root_uninspectable_json, remediation_ambiguity_only
   #  3 SKIP: parser-backed JSON cases (no jq/python3 on busybox; macOS lane executes
   #          them) — env_multiline_no_name_leak_json + json_hostile_env (parse) +
   #          json_hostile_fs (parse)
   #  1 SKIP: darwin_home_firmlink_skip_realhost (non-Darwin; macOS-only standard-firmlink predicate)
```

The macOS lane — re-executed this pass under `sh`, `dash`, `/bin/bash`, and `zsh` with
identical results — is the authoritative post-hardening evidence: **63 passed / 0 failed
/ 0 skipped**. The container SKIPs are exactly the cross-platform allowances:
root/unreadable ambiguity is not meaningful as root, parser-only validation is explicitly
SKIPped where no parser exists (the **macOS lane executes** the parser-backed cases with
`jq`, including the presence-only JSON regression), and the Darwin-only firmlink test
SKIPs on Linux (non-Darwin).

### 5.5 JSON output: parser-backed validation with hostile inputs (executed on macOS)

```
$ tmp=$(mktemp -d)
$ env 'NIX_HOSTILE=pre<TAB>x<NL>y"z\w<TAB>' NIX_PATH='$(rm -rf /)' IN_NIX_SHELL=1 \
    sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$tmp" --json > "$tmp/o.json"
$ jq -e '.summary.result=="refuse" and (.findings|length>0)' "$tmp/o.json"   # -> valid
$ grep -c 'rm -rf' "$tmp/o.json"   # -> 0   (hostile payload did not leak)
$ jq -r '.findings[]|select(.id=="ENV_NIX_VAR")|.message' "$tmp/o.json"
one or more NIX_* environment variables are present (presence-only; names, counts, and values redacted); a Nix shell/env is configured.
$ grep -cE 'NIX_HOSTILE|NIX_PATH' "$tmp/o.json"   # -> 0  (no variable NAMES in JSON)
```

Both `jq` and `python3` parse the output. Detection is **presence-only**: no env-var
**names**, **counts**, or **values** (nor value-derived text) appear in text or JSON.
Robust JSON escaping (backslash, quote, control chars) is applied defensively even
though no untrusted dynamic text is included in JSON messages. The presence-only design
is what closes the overwatch finding: a value containing a newline can introduce an
indistinguishable `NIX_FOO=...` line, so any name extracted from line-oriented `env`
output could be value-derived text — the detector therefore refuses on PRESENCE and
emits a fixed message (see the `env_multiline_no_name_leak` regression in §5.4).

### 5.6 Fixture-library guard + detector safety guards (executed)

`make_existing_install_linux /` exits `64` **before any `mkdir`/`chmod`/write/symlink**;
a post-run check confirms `/nix/store` was not created. The detector rejects a
non-`/` symlink `--root` (`64`), a bare `--root` with no value (`64`), any
`--mode`/unknown argument (`64`), and a dot-dot traversal alias such as
`$suite/clean/../clean` (`64`, no real `/nix` touched). The fixture-suite capability
(§5.4) refuses `fx_init_suite /`, `fx_init_suite /etc`, a normal repo-style directory,
and a wrong-prefix temp dir **before writing anything**, creates **no sentinel at `/`**
(checked absent before and after; a pre-existing one would be left untouched), and a
**hand-planted constant sentinel** is refused in all three forms: no capability
initialized, the suite sentinel overwritten with a constant (per-run token mismatch),
and a nested dir under the suite carrying a planted constant (canonical-parent
mismatch). Post-hardening additions (all executed in §5.4): **(a)** `fx_init_suite`
**fails closed** when `find` is unavailable/nonzero — a stubbed failing `find` (now a
shell-function `find() { return 1; }` override in the subshell, replacing the former
second-temp-dir executable stub) yields `exit 64` before any sentinel is written and the
suite dir is left empty/untouched (the former `find … || true` was fail-open); **(b)** the macOS per-user TMPDIR root is
matched with an **exact two-component** check (`fx_is_macos_tmproot`: accepts precisely
`/private/var/folders/<one>/<two>/T`, rejects deeper descendants that a `case` glob
`*/*/T` would accept because `*` spans slashes); **(c)** env detection is
**presence-only** — the environment is queried with two direct `env | grep` pipelines
and is NEVER captured into a variable (the former `ce_env=$(env …)` is removed); a
non-NIX var whose multiline value injects a fake `NIX_SECRET_FROM_VALUE=...` line is
refused but neither the injected name nor the secret value appears in text or JSON, and
if `env` itself fails the detector FAILS CLOSED (`ENV_QUERY_FAILED`) rather than report
clean; **(d)** the macOS standard `/home` firmlink is skipped ONLY after
`is_standard_macos_home_firmlink` VERIFIES its canonical target is exactly
`/System/Volumes/Data/home` (replacing the blind "every real-host Darwin `/home` is the
standard link" assumption) — pinned by the `darwin_home_firmlink_skip_realhost`
Darwin-only regression, with the fake-root arbitrary `/home` symlink refusal still
proven by `home_symlink_root_refused` (and the broken/unresolvable-target branch by
`home_broken_symlink_refused`); **(e)** the **basename execution guard is
removed** — `main` runs unconditionally, so a differently-named symlink/renamed
executable behaves identically to the canonical name (overwatch: the old
`basename == "detect-unmanaged-nix.sh"` guard made a renamed copy silently exit 0),
pinned by the `symlink_invocation_runs_main_{sh,exec}` regressions; **(f)** current-`$HOME`
coverage is consistent with the verified `/home` skip (a custom `$HOME` under
`/home/*` or `/root/*` is still scanned on real macOS when `/home` is skipped). See §12.

### 5.7 Two-phase preflight contract + split remediation (executed)

The detector is the **unprivileged early read-only scan**: it can REFUSE (advisory) but
can never AUTHORIZE installation on its own. What authorizes proceeding is a FULL
read-only **privileged** preflight re-run by the signed installer/helper **immediately
before any mutation**. This two-phase contract closes the unprivileged permission gap
(`/var/root` is unreadable as non-root) and shrinks the TOCTOU window to the moment
before mutation. The spike did **not** execute that privileged pass, so the macOS CLEAN
result remains **unvalidated (inference (c))**.

Remediation is split by result (executed): an **ambiguity-only** REFUSE (`N_UNMANAGED=0`,
`N_MARKER=0`, `N_AMBIG>0` — e.g. the real-host `/var/root` finding in §5.2, and the
`ambiguous_unreadable` fixture) prints an advisory that **contains no uninstall/removal
instructions** and **does** name the privileged read-only recheck; a **definite**
unmanaged/marker REFUSE (e.g. `existing_install_linux`) still provides **bounded
vendor-uninstall guidance**. The `remediation_ambiguity_only` and
`remediation_definite_unmanaged` assertions pin both branches (§5.4).

---

## 6. Official documentation & source evidence (b)

Versions and pins are recorded exactly. **Note on versioning:** the nix.dev manual is
served under a `/2.34/` URL alias whose rendered title reads **"Nix 2.34.9"** — but
there is **no `2.34.9` git tag**; the latest `2.34.x` release tag is **`2.34.8`**. The
latest Nix release overall is **`2.35.1`** and the `/latest/` manual renders "2.35.2"
(**as of 2026-08-05**). Source is pinned at tag **`2.34.8`**, commit
**`f3f1c3c5b8ad91850e0f7c590cf177f7ab022024`** (annotated-tag object
`b6769c588f60b3e762f73d3a8cf60294df078ccd`). All script/source citations below are from
that commit.

### 6.1 Local store — the three distinctions (load-bearing)

Source: *Local Store*, Nix Reference Manual —
https://nix.dev/manual/nix/2.34/store/types/local-store.html (rendered page title
"Local Store - Nix **2.34.9** Reference Manual"). Paraphrased below; the only
near-verbatim phrase retained from the manual is "not recommended" (2 words), so the
total verbatim text drawn from this source is well under 25 words.

- **Store URL `local[,root=…]`** — the `local` store accepts an optional absolute
  `root` prepended to every store, state, and log path; the bare URL `local` implies a
  root of `/`. Any root other than `/` makes it a **chroot store**.
- **(Distinction 2 — chroot store)** The logical store directory stays `/nix/store`,
  but programs in such a store can only be built and run from inside a chroot rooted at
  the given root, and this mode is Linux-only because it depends on mount and user
  namespaces. The physical store lives at `<root>/nix/store`.
  (Example: `nix run --store /tmp/root …`.)
- **(Distinction 1 — alternate logical store dir)** Nix permits pointing the logical
  store elsewhere via `local?store=…`, but the manual calls this "not recommended"
  because a non-standard logical prefix cannot substitute from the default binary cache
  (`cache.nixos.org`). (Example: `local?store=/tmp/my-nix/store&state=…&log=…`.)

**Distinction 3 — compile-time store-dir** is established by the Nix **Meson** build
option **`-Dlibstore:store-dir=…`** (defined in `src/libstore/meson.options`, consumed in
`src/libstore/meson.build`, at the pinned commit). Building Nix with a non-`/nix/store`
store-dir bakes a different logical prefix into the binary: this is a **source-built
custom Nix** (cache-incompatible, same consequence as Distinction 1) and is forbidden by
DR-014, which pins an **unmodified** upstream build. (The obsolete `./configure
--storedir=…` wording referred to Nix's pre-Meson autotools build and no longer applies.)

### 6.2 Multi-user mode & daemon socket

Source: *Multi-User Mode*, Nix Reference Manual, rendered at
`https://nix.dev/manual/nix/2.34/installation/multi-user.html` ("2.34.9"). Paraphrased:
the store and database are owned by a privileged user (usually `root`); builds run under
dedicated user accounts (`nixbld1`, `nixbld2`, …); store actions are forwarded to a
**Nix daemon**; clients set `NIX_REMOTE=daemon`; access is gated by permissions on
**`/nix/var/nix/daemon-socket`**, whose Unix socket is
**`/nix/var/nix/daemon-socket/socket`**.

### 6.3 Installation guidance

Source: *Installation*, Nix Reference Manual —
https://nix.dev/manual/nix/2.32/installation/index.html ("2.32.9"). The current
recommended option on both Linux and macOS is **multi-user**; single-user mode is not
offered on macOS.

### 6.4 Store-info command

Source: `nix store info`, Nix Reference Manual, rendered at
`https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-store-info.html` ("2.35.2",
**experimental**, as of 2026-08-05). It tests whether a store can be accessed;
`nix store info --store daemon` is the documented daemon-up probe (the modern equivalent
of `nix ping-store` referenced in `plans/07` §5.3/§18).

### 6.5 Installer scripts & ownership/mode invariants (source, pinned at `2.34.8`, commit `f3f1c3c5…`)

Authoritative for the artifact set an unmanaged Nix lays down (what the detector must find):

- **`scripts/install-multi-user.sh`** — `NIX_BUILD_GROUP_NAME="nixbld"` (Linux) /
  `_nixbld` (macOS); `NIX_ROOT="/nix"`; `sudo chown -R 'root:nixbld' '/nix'` (Linux);
  `/nix/store` is `install -dv -g nixbld -m 1775`; `/etc/nix` is `install -dv -m 0555`.
  Uninstall list enumerates `/etc/nix`, `/nix`, `~/.nix-profile`, `~/.nix-defexpr`,
  `~/.nix-channels`, `~/.local/state/nix`, `~/.cache/nix`.
- **`scripts/install-systemd-multi-user.sh`** — units `nix-daemon.service` +
  `nix-daemon.socket` in `/etc/systemd/system/…`; tmpfiles `/etc/tmpfiles.d/nix-daemon.conf`.
- **`scripts/install-darwin-multi-user.sh`** — `NIX_BUILD_USER_NAME_TEMPLATE="_nixbld%d"`;
  plist `/Library/LaunchDaemons/org.nixos.nix-daemon.plist`; label `org.nixos.nix-daemon`.
- **`scripts/create-darwin-volume.sh`** — `NIX_ROOT=/nix`; `NIX_VOLUME_LABEL="Nix Store"`;
  `/etc/synthetic.conf` grepped for `^nix$`; `/etc/fstab` for `/nix apfs rw`.

**Ownership/mode invariant (recorded for DR-001):** `/nix` is **root-owned and not
world-writable**. `/nix/store` is **mode `1775`** = sticky bit (`1`) + owner `rwx` (`7`)
+ group `rwx` (`7`) + others `r-x` (`5`); it is **group-writable by the build-users
group but NOT world-writable** (others get read+execute, no write). On **Linux** the
group is `nixbld`, sourced directly from `install-multi-user.sh`
(`install -dv -g nixbld -m 1775`). On **macOS** the build users are `_nixbld1..` and
the group `nixbld`, and `/nix` lives on the synthetic APFS "Nix Store" volume; the
*exact* store mode/group on Darwin is **inference (c)** — the pinned `install-multi-user.sh`
proves the Linux `1775`/`nixbld` contract, while the macOS path runs through
`install-darwin-multi-user.sh`/`create-darwin-volume.sh`, whose concrete `install -m`/
`chown` for `/nix/store` was not separately pinned in this spike. The
platform-independent statement that holds on both: root-owned, not world-writable, and
the store is group-writable by the build-users group.

---

## 7. Results — the three distinctions vs. V1 requirements

| # | Option | What it is (evidence) | Native exec? | Platforms | `cache.nixos.org` reuse? | Meets V1? |
|---|---|---|---|---|---|---|
| **0** | **Standard `/nix/store`** (stock) | Default logical store; `local` root `/`. (§6.1) | ✅ native | ✅ Linux **and** macOS | ✅ verbatim | ✅ **Yes** |
| **1** | **Alternate logical store dir** | `local?store=/opt/…/store&state=…&log=…`. (§6.1) | ✅ native | ✅ both | ❌ a non-standard logical prefix cannot substitute from the default cache (`cache.nixos.org`) | ❌ No (fails R4) |
| **2** | **Chroot store** | `--store /opt/pkg/root`; logical store stays `/nix/store`; physical at `/opt/pkg/root/nix/store`. (§6.1) | ❌ only inside a chroot | ❌ Linux only | ⚠️ logical paths match so NAR hashes match in principle, but programs won't run natively and macOS is unsupported | ❌ No (fails R2, R3) |
| **3** | **Compile-time store-dir** | Build Nix with a non-`/nix/store` prefix via Meson `-Dlibstore:store-dir=`. (§6.1) | ✅ native | ✅ both | ❌ same as #1 (cache paths mismatch) | ❌ No (fails R1 [DR-014] and R4) |

**Correction of the prior over-broad claim.** "Stock Nix is not relocatable at all" is
wrong as stated. The precise facts:

- Stock Nix **is** "relocatable" in the **chroot** sense on Linux: a different *physical*
  root with the *logical* store still `/nix/store` (`--store <root>`). But its programs
  can only be **built and run by chroot-ing**, and it is **Linux-only**.
- Stock Nix **can** change the **logical** store dir (`local?store=…`), but this is
  "not recommended" and **breaks `cache.nixos.org` substitution**.
- What stock Nix **cannot** do is *any one of*: (a) relocate the logical store while
  still consuming `cache.nixos.org`, (b) run a chroot store on macOS, or (c) execute
  chroot-store programs natively on the host. Each of (a)/(b)/(c) is a hard V1
  requirement, so **no non-standard option clears the bar**.

### 7.1 Why "coexistence with a foreign Nix" is not viable in V1

A second Nix on one host fails R5 regardless of prefix choice. Some of this is reasoned
from the layout in §6.2/§6.5 (**inference (c)** unless noted), not a directly-sourced
Nix guarantee:

- **Shared `/nix/store` is a single physical database.** The store, its db
  (`/nix/var/nix/db`), gcroots, and profiles all live under one `/nix`. **Two daemons
  managing one store/database is unsupported and carries concrete collision and
  GC-contention risk; outright corruption is *not* guaranteed** but is a
  documented-class hazard. **(c)**
- Reusing an **existing foreign daemon/store is technically possible in some
  configurations** but is **intentionally unsupported** because it violates the pinned
  **managed-runtime / version / config / trust / exclusive-ownership** invariants
  (DR-014/I3; `plans/00` D-10/D-11; `plans/07` I1/I4). This is a product policy choice,
  not a claim that all coexistence is physically impossible.
- **Two `/nix/store`s cannot both reuse `cache.nixos.org`** unless both are the standard
  `/nix/store` — which is the collision above. A non-standard logical prefix fails R4.
- **The daemon socket is a single well-known path** (`/nix/var/nix/daemon-socket/socket`).
  A foreign daemon already bound there collides with `pkg`'s daemon. `NIX_REMOTE`/a custom
  socket can separate the *clients*, but not the shared `/nix` substrate. **(c)**
- **Trust ambiguity.** `pkg` pins Nix, substituters, and keys in a signed channel and
  ignores user overrides (`plans/00` D-10/D-11, `plans/07` I4). A foreign Nix brings its
  own (possibly attacker-controlled) trust inputs; silently sharing a store crosses trust
  domains (T-INST-4).

### 7.2 Product-marker semantics (the load-bearing fact: it REFUSES)

A `pkg` ownership marker (e.g. `/var/lib/pkg/.managed-nix`, macOS
`/Library/Application Support/pkg/.managed-nix`) is **one corroborating signal**. The
fixture `product-marker-only` proves the load-bearing fact: **the detector REFUSES**
(exit 2) — the marker never authorizes takeover, never implies the store is safe, and is
never used to auto-remove anything. Earlier wording that "a lone marker is ambiguous"
was imprecise: the detector *records* it as a `marker` finding, and in install/preflight
**any finding is a refusal**.

**Runtime/`doctor` recognition is out of scope for this spike** and is explicitly
deferred to **PR-9/PR-12**. When designed, recognizing an already-present `/nix` tree as
pkg-owned must require an **authenticated/validated ownership receipt** PLUS verification
of the **complete expected managed-artifact set** — never a path or a marker alone. This
spike does not design or implement that.

---

## 8. Decision — V1 layout (justified)

| Decision | Choice | Justification (evidence) |
|---|---|---|
| **Product exe/state/config/logs location** | **Outside `/nix`.** Binary `/usr/local/bin/pkg` (or `/opt/pkg/bin`); **machine-global** service state `/var/lib/pkg/` (Linux) and **`/Library/Application Support/pkg/`** (macOS, root-owned, leading slash = the machine-global `/Library`, distinct from per-user `~/Library`) — **distinct from** the fixed alpha per-user roots `$HOME/.local/share/pkg/` and `~/Library/Application Support/pkg/`, where HOME is the authenticated uid's system/passwd home. This supersedes the spike's earlier `$XDG_DATA_HOME/pkg/` wording (D-17/INV-10). | §6.5 shows Nix owns all of `/nix` (incl. `/nix/var/nix`); `plans/07` §5.1/§6.1 place the bundled Nix under `/opt/pkg/nix`. Nothing in §6.1–§6.4 requires product files inside `/nix/store`. |
| **Store** | **Standard logical `/nix/store`, stock Nix.** | §7: only Option 0 meets R1–R5. Options 1–3 each fail at least one hard requirement. |
| **Single- vs multi-user** | **Multi-user with daemon** on both OSes. | §6.3: multi-user is the current recommended option on Linux **and** macOS; single-user is unsupported on macOS. Daemon is required for sandboxed multi-user builds (`nixbld`/`_nixbld`, §6.2/§6.5). Matches `plans/07` §7.1. |
| **Daemon socket** | **Standard socket path `/nix/var/nix/daemon-socket/socket`** *because* the installer first proves exclusive ownership. `pkg` connects with `NIX_REMOTE=unix:///nix/var/nix/daemon-socket/socket` (or `daemon`) to **only** its own daemon. | §6.2/§6.5: that path is canonical; choosing it is collision-free *only* because preflight already refused any foreign Nix. A product-specific socket would *reduce* collision risk with an unknown foreign daemon but cannot make shared `/nix` safe; it is a defense-in-depth option for v2, not a substitute for exclusive ownership. (See §10/§7.1.) |
| **Ownership model** | **Exclusive.** `pkg` exclusively owns `/nix`. On detecting any unmanaged or ambiguous Nix artifact, refuse with remediation; **never auto-delete**. | R5; §7.1 (coexistence unsupported); D-03/D-04; T-INST-4. |
| **What the privileged preflight proves immediately before mutation** | This detector is the **unprivileged early read-only scan**: advisory refusal only, it never authorizes install. The **signed privileged installer/helper re-runs the FULL read-only preflight immediately before any mutation** and only a CLEAN privileged preflight authorizes proceeding. That privileged pass checks: (i) host is Nix-free per the same signal set (zero findings); (ii) supported arch/system; (iii) privilege available; (iv) `/nix` creatable/writable by the installer. Only then may the privileged helper create `/nix`, build users/groups, the daemon unit/plist, and the root-owned `nix.conf`. This closes the unprivileged permission gap and shrinks the TOCTOU window to the moment before mutation. | §5.2/§5.7 (two-phase contract; privileged macOS CLEAN is unvalidated inference); `plans/07` §7.2/§7.3 step 1. |

These match the **default** already recorded in DR-001/DR-007. This spike upgrades the
basis from "default pending spike" to **evidence-backed** for the store/socket/ownership
conclusions, and leaves coexistence explicitly deferred to v2.

---

## 9. Decision record status (DR-001)

The documented **success criteria** for S1 (`plans/12` §2) are: *"Concrete layout +
detection method + refusal text validated on Linux & macOS; go/no-go on alternative
prefix."* Against those, **the technical recommendation and evidence are complete**:

- ✅ Concrete layout decided (§8) and grounded in primary-source evidence (§6).
- ✅ Detection method implemented and validated **by execution** on a real macOS host
  (§5.2) **and** a real Linux container (§5.3), plus fixture-driven cases on both (§5.4).
- ✅ Refusal text present and non-destructive (`detect-unmanaged-nix.sh` →
  `print_remediation`; no `--force`, never `rm`/service-stop).
- ✅ Go/no-go on alternative prefix: **no-go** for any non-standard option (§7).

**DR-001 status is `Proposed`, not Accepted.** Per `plans/11` §2 / `plans/12` §7 /
`CONTRIBUTING` §5, a spike DR is `Accepted` only after the **spike owner and the
affected area owners (F, E, and A for DR-001) sign off**. That recorded sign-off has not
happened; this document records only the *technical* basis. Therefore **the AC-D1 gate
(`plans/12` §8) is NOT cleared by this spike**, and the dependent PRs (PR-9, PR-12,
PR-27, PR-28) must not merge on this basis until the DR is Accepted. The crisp
technical recommendation stands: **standard `/nix/store`, stock Nix, exclusive managed
ownership, multi-user daemon, standard socket behind fail-closed preflight, product
state outside `/nix`, no alternative prefix for V1.** The **pending gate** is exactly:
recorded F/E/A sign-off (and, downstream, the real-Nix validation in PR-9/PR-12/PR-27/PR-28).

**Cross-plan note (scoped).** On Acceptance, DR-001 **supersedes** the stale, over-broad
relocatability/socket statements in **E-owned `plans/07` §6.2** — specifically *"stock
Nix is not relocatable to an arbitrary prefix"* (precise facts in §7) and spike
deliverable #4 (*"prefer product prefix"* for the daemon socket), which the DR resolves
to the **standard socket behind exclusive ownership**. `plans/07` is E-owned and **must
be reconciled by its owner in PR-9/PR-12**; this spike does **not** edit `plans/07`, and
no tracking issue is claimed.

> What PR-9 / PR-12 / PR-27 / PR-28 may rely on from this spike **once the DR is
> Accepted**: the **install/preflight detector contract** (signal IDs; exit 0=clean /
> 2=refuse / 64=usage; `--root`/`--json`; fail-closed-on-ambiguity; marker is
> corroborating only and is itself a refusal), the **layout invariants** (standard
> `/nix/store`; product files outside `/nix`; multi-user daemon; standard socket behind
> exclusive ownership), and the **refusal copy** in `print_remediation`. They may **not**
> rely on any runtime/mode behavior — there is none in this spike; runtime/`doctor`
> recognition is deferred (§7.2).

---

## 10. Security implications

- **Fail-closed on ambiguity.** Unreadable `/nix`, `/etc/nix/nix.conf`, the marker,
  `/etc/passwd`, `/etc/group`, `/etc/fstab`, `/etc/synthetic.conf`, service/plist/profile
  roots, or any unreadable home root — all yield **ambiguous ⇒ refuse**. `pkg` never
  guesses. (T-INST-4.) The real-host scan (§5.2) demonstrates this: an unreadable
  `/var/root` is a refusal.
- **Never auto-delete.** No code path removes `/nix`, stops/disables a foreign
  `nix-daemon.{service,socket}`/`org.nixos.nix-daemon.plist`, deletes the APFS "Nix Store"
  volume, removes `nixbld`/`_nixbld` users, or unmounts anything. Remediation is copy
  only. (D-04; T-UNINST-1.)
- **No `--force`.** There is no flag that bypasses detection in V1.
- **Read-only detector.** The optional `systemctl`/`launchctl`/`mount`/`getent`/`dscl`/
  `diskutil apfs list` corroborators are restricted to read-only list/status queries,
  guarded behind `command -v`, **scoped to the applicable OS**, and run only on a real
  host (`/`). A present-but-failing corroborator is treated as ambiguous (refuse) where
  practical; a tool absent on the other OS is never a finding. `diskutil` is never used
  mutatingly. There are **no `PKG_PROBE_*` env bypass knobs**: a user-controlled
  environment cannot disable a fail-closed corroborator. Live corroborators run only at
  the literal `/`; tests use fake roots to avoid them.
- **Environment hygiene.** Any exported `NIX_*` variable (empty-valued included) and an
  exported `IN_NIX_SHELL` are refusals; detection is **presence-only** — **no** variable
  names, counts, or values are ever parsed, persisted, or reflected in any finding/output
  (the environment is queried with two direct `env | grep` pipelines and is never captured
  into a variable; a value containing a newline can introduce an indistinguishable
  `NIX_FOO=...` line, so any name extracted from line-oriented `env` output could be
  value-derived text). A fixed, generic, redacted message is emitted when one-or-more
  `NIX_*` entries are conservatively detected; if `env` itself fails the detector FAILS
  CLOSED (`ENV_QUERY_FAILED`). Honest residual: a non-NIX variable whose multiline value
  contains a `NIX_SOMETHING=...` line can cause a conservative false-positive refusal,
  inherent to line-oriented POSIX env serialization and preferred over any value-derived
  leak. No `eval` is used and there is no debug flag for values.
- **No install-time PATH whitelist.** Any Nix binary reachable on `PATH` before
  installation is a refusal (there is no `/opt/pkg/**` exception).
- **Output safety.** JSON carries only signal IDs and fixed/redacted messages — no env
  names, counts, values, file contents, symlink targets, or resolved paths — and is
  defensively JSON-escaped; hostile inputs cannot produce invalid JSON (§5.5).
- **Marker hygiene.** A marker alone is a refusal; it never authorizes anything.
- **Two-phase preflight (TOCTOU/permission-gap control).** This detector is the
  **unprivileged early read-only scan** — advisory refusal only; it can never authorize
  installation. Only a full read-only **privileged** preflight re-run by the signed
  installer/helper **immediately before mutation** can authorize proceeding. This is the
  load-bearing control that closes the unprivileged permission gap (e.g. `/var/root`
  unreadable as non-root) and shrinks the TOCTOU window to the instant before mutation
  (§5.7). The privileged pass was **not executed** in this spike.
- **Fixture-suite capability (accident/path-confusion control, NOT cryptographic).** The
  fixture harness mutates only a `mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"` suite whose
  canonical parent is TMPDIR, whose name matches `pkg-s1.*`, which is empty (verified by
  `find`; if `find` is unavailable/nonzero `fx_init_suite` **fails closed** before any
  sentinel), and whose canonical TMPDIR parent is allowlisted to exactly `/tmp`,
  `/private/tmp`, `/var/tmp`, `/private/var/tmp`, or a macOS per-user root
  `/private/var/folders/<one>/<two>/T` (exact two components). A user-controlled TMPDIR
  outside these canonical roots is refused before any sentinel is written. A per-run
  token (PID/time/path) is written into the sentinel; every primitive re-checks
  capability + token + canonical-parent, so a hand-planted constant sentinel cannot
  authorize mutation. This prevents accidents inside the 0700 mktemp tree; it is not a
  defense against an attacker with write access to TMPDIR.
- **Residual (RISK-04).** If a future v2 revisits a product-specific socket or a separate
  state dir to ease side-by-side presence, the *shared `/nix/store`* hazard (§7.1) still
  dominates: exclusive ownership remains the load-bearing control. This spike does **not**
  weaken that invariant.

---

## 11. Limitations / unvalidated items

- **No real Nix executed.** Cache-path native execution and two-daemon collision are
  inference (c) from §6, not measured. Deferred to Real-Nix CI (PR-36) and installer PRs.
- **No privileged validation.** The detector was run as a non-root user on macOS; the
  real-host scan refuses on the unreadable `/var/root` with an ambiguity-only advisory
  (§5.2/§5.7). A **privileged macOS CLEAN is inference and was NOT executed**; the
  two-phase privileged preflight (§5.7/§10) is the unvalidated gate. No real Nix-positive
  or privileged host validation is claimed. Validated by execution: a real **Nix-free
  macOS host produced a safe ambiguity/refusal as non-root**, a **root Alpine container
  produced CLEAN**, and **all positive-platform artifacts are fixture-driven**.
- **Detector is a spike, not production.** It is POSIX `sh`; the production detector
  (PR-9, `crates/pkg-nix/src/managed/detect.rs`) should re-implement these signals with
  the same contract and add: real `systemctl`/`launchctl`/`diskutil`/`apfs` queries,
  `dscl`-based OpenDirectory user/group scans on macOS, and socket-peer-credential
  cross-checks. The fixture set here is the acceptance baseline.
- **Container Linux check used Alpine (glibc-free).** It proves the *detector* behaves
  identically on Linux; it is not a glibc-Linux Nix validation. (R3 for a real
  glibc-Linux install remains inference.)
- **`shellcheck` not run** (absent on host) — a **recorded validation gap**, not
  "clean by construction". POSIX `sh -n` + cross-shell (`dash`/`bash`/`zsh`/`ash`) runs
  are the executed substitute.
- **macOS APFS volume detection is fixture-driven** (synthetic.conf/fstab/symlink) plus
  the read-only `diskutil apfs list`/`mount` corroborators; a real "Nix Store" APFS
  volume was not present on this host to corroborate against.
- **macOS `/home` skip is host-verified by canonical-target match, not Apple-documented.**
  The `is_standard_macos_home_firmlink` predicate (§5.2) skips `/home` only after
  confirming — on a real macOS host — that `/home` is a symlink whose canonical target
  resolves to EXACTLY `/System/Volumes/Data/home`. On THIS host that target is verified
  (`readlink`/`cd -P`/`pwd -P`), but it is not sourced from an Apple contract. Any other
  `/home` (absent, a real directory, or a symlink to a different/unresolvable target) is
  scanned normally, so a non-standard or user-controlled `/home` is still fail-closed
  (`HOME_ROOT_SYMLINK`); the `$HOME` branch still covers custom homes, and a custom
  current `$HOME` under `/home/*` or `/root/*` is still scanned when `/home` is skipped.
  The residual custom-home case (a symlinked `$HOME` outside the standard roots) is still
  refused (`HOME_ROOT_SYMLINK`).
- **Latent `set -e` crash fixed in-spike.** The Alpine re-run (§5.3) surfaced that
  `check_profiles` could return spurious nonzero on a clean host with a non-Nix
  `/etc/profile.d`, crashing under `set -e`; fixed by converting the `&& record` tail to
  `if/then/fi` (spike-internal; no production code). This is the only behavior change
  beyond the six accepted review issues, and it is a pure correctness fix (clean →
  CLEAN, not crash).
- **Windows/musl/SELinux**: out of scope (V1 = glibc `*-linux` + Darwin, per `plans/00`
  §2 / `plans/07` Q7.3); SELinux-disabled is a documented multi-user prerequisite (§6.3).

---

## 12. Reproducible commands

All detector invocations are read-only and safe to re-run. The fixture harness mutates
**only** its own verified `mktemp` scratch tree. From the repo root:

```sh
# 0. POSIX syntax/portability (also try dash, /bin/bash, zsh).
for f in spikes/s1-store-prefix/*.sh; do sh -n "$f" && echo "OK $f"; done

# 1. Run the fixture-driven suite (private mktemp scratch; validates sentinel; cleans up).
sh spikes/s1-store-prefix/run-tests.sh

# 2. Scan the real host (read-only). As a non-root user on macOS this REFUSES on the
#    unreadable /var/root (correct fail-closed); CLEAN only when all state is readable.
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root /

# 3. Build one fixture directly. ALWAYS initialize the suite with fx_init_suite
#    (it verifies the name, the empty dir, canonical parent == TMPDIR, refuses
#    protected roots, and writes the per-run capability token). NEVER hand-create
#    the sentinel: a hand-planted constant sentinel must not authorize mutation.
. ./spikes/s1-store-prefix/build-fixtures.sh
suite=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX")
fx_init_suite "$suite"
cdir="$suite/mac"; mkdir -p "$cdir"
make_existing_install_macos "$cdir"
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$cdir"        # -> exit 2, REFUSE
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$cdir" --json
make_existing_install_linux /                                          # -> refused (exit 64), no mutation
fx_cleanup_suite "$suite"

# 4. (Optional) real Linux clean-host check, if the Alpine image is already present.
docker run --pull=never --rm -v "$PWD/spikes/s1-store-prefix:/s:ro" alpine:3.20 \
    sh -c 'sh /s/detect-unmanaged-nix.sh --root / >/dev/null 2>&1; echo exit=$?; uname -sm'
docker run --pull=never --rm -v "$PWD/spikes/s1-store-prefix:/s:ro" alpine:3.20 sh /s/run-tests.sh

# 5. Parser-backed JSON check (jq or python3 if present). fx_init_suite, not a manual sentinel.
. ./spikes/s1-store-prefix/build-fixtures.sh
d=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"); fx_init_suite "$d"; c="$d/x"; mkdir -p "$c"
make_existing_install_linux "$c"
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$c" --json | jq .
fx_cleanup_suite "$d"

# 6. Path-confusion guards + the macOS /var/folders CLEAN, on a LIVE verified suite
#    (step 3's $suite was already cleaned by fx_cleanup_suite, so create a fresh one).
#    A dot-dot traversal alias is rejected at 64 before any scan (no real /nix touched);
#    a clean case dir under TMPDIR scans CLEAN. On macOS $TMPDIR is /var/folders/.../T
#    (canonical /private/var/folders/<one>/<two>/T), so this IS the /var/folders CLEAN
#    demonstration (not /var -> /private/var is not over-rejected) — run on both OSes.
. ./spikes/s1-store-prefix/build-fixtures.sh
s6=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"); fx_init_suite "$s6"
mkdir -p "$s6/clean"
fx_canon "${TMPDIR:-/tmp}"                                   # -> /tmp on Linux; /private/var/folders/<a>/<b>/T on macOS
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$s6/clean"               # -> exit 0, CLEAN (the /var/folders demonstration)
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --root "$s6/clean/../clean"; echo "traversal -> $?"   # -> 64
fx_cleanup_suite "$s6"

# 7. Detector help / exit codes. Note: --root with no value -> 64; --mode -> 64.
sh spikes/s1-store-prefix/detect-unmanaged-nix.sh --help
```

Exit codes: `0` clean · `2` refuse (unmanaged or ambiguous finding, incl. a lone marker)
· `64` usage error (unknown arg; bare `--root`; relative/missing/non-dir/unsafe/symlink
root other than literal `/`; the removed `--mode`).

---

## 13. Citations

- **[LOCAL-STORE]** *Local Store*, Nix Reference Manual, "Nix 2.34.9" rendered page —
  https://nix.dev/manual/nix/2.34/store/types/local-store.html (chroot store; logical
  store-dir change; `local?store=…` example; cache-incompatibility warning).
- **[MULTI-USER]** *Multi-User Mode*, Nix Reference Manual, "2.34.9" —
  https://nix.dev/manual/nix/2.34/installation/multi-user.html (`root`-owned store/db;
  `nixbld1..` build users; `NIX_REMOTE=daemon`; socket `/nix/var/nix/daemon-socket/socket`).
- **[INSTALL-2.32]** *Installation*, Nix Reference Manual, "2.32.9" —
  https://nix.dev/manual/nix/2.32/installation/index.html (multi-user recommended on
  Linux+macOS; "Single-user is not supported on Mac").
- **[STORE-INFO]** `nix store info`, Nix Reference Manual, "2.35.2" (experimental,
  as of 2026-08-05) —
  https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-store-info.html
  (`--store daemon` daemon-up probe).
- **[NIX-SRC]** Nix source, tag **`2.34.8`**, commit **`f3f1c3c5b8ad91850e0f7c590cf177f7ab022024`**
  (annotated-tag object `b6769c588f60b3e762f73d3a8cf60294df078ccd`):
  `scripts/install-multi-user.sh`, `scripts/install-systemd-multi-user.sh`,
  `scripts/install-darwin-multi-user.sh`, `scripts/create-darwin-volume.sh`; and the
  compile-time store-dir Meson option `-Dlibstore:store-dir=` defined in
  `src/libstore/meson.options` and consumed in `src/libstore/meson.build`.
  https://github.com/NixOS/nix . Latest release tag at spike time (as of 2026-08-05):
  **`2.35.1`**.
- **[PLANS]** `plans/00-overview-and-decisions.md` (D-03/04/10/11/14/17, INV-02, §6.1);
  `plans/07-platform-installation-and-runtime.md` (I1/I3/I4, §5–§8, §18; §6.2 stale
  statements superseded by DR-001 on Acceptance — see §9);
  `plans/08-security-model.md` (T-INST-4, T-UNINST-1);
  `plans/12-open-decisions-and-risks.md` (DR-001, DR-007, DR-014, RISK-04, §8 AC-D1).

Files in this spike: `README.md`, `findings.md` (this file), `detect-unmanaged-nix.sh`,
`build-fixtures.sh`, `run-tests.sh`.
