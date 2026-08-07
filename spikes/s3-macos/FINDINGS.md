# S3 / PR-7 / DR-003 — Evidence Ledger

This is an **evidence ledger**, not a result and not marketing. It records what
was actually verified in this environment, what was *not* (and why), and what
must still be measured. **No coverage, capability, build, or signing outcome
appears here as a value.** Every result cell below is `Pending` until a reviewed
Complete real run exists for that lane. Nothing is invented.

- **Spike:** S3 — macOS binary coverage + signing/notarization evidence harness.
  Harness: [`pkg-spike-s3-macos`](README.md).
- **PR:** [PR-7 — SPIKE S3: macOS binary coverage + signing](../../plans/11-pr-roadmap.md)
  (M0.5). Owns this directory and DR-003.
- **Decision:** [DR-003 — macOS build security + signing/notarization](../../plans/12-open-decisions-and-risks.md)
  — **Status: Proposed** (pending S3 / PR-7).
- **Downstream gates:** [PR-28](../../plans/11-pr-roadmap.md) (macOS lane) and
  [PR-36](../../plans/11-pr-roadmap.md) (notarized installer/runtime) are **direct
  macOS evidence gates on S3**. [PR-26](../../plans/11-pr-roadmap.md) (shared
  local-build engine) is **not** simply gated on S3: its shared mechanism is gated
  on [S5]/[DR-005], while `plans/12` lists S3 as blocking/informing PR-26's Darwin
  *policy*. All remain gated until S3 (and, for PR-26, S5/DR-005) is satisfied.
- **Recorded (host UTC):** 2026-08-07. **Host lane of this session:** macOS,
  ProductVersion 26.6, Darwin kernel 25.6.0, `aarch64` (`aarch64-darwin`).

---

## 1. Harness evidence (what the spike proves today)

Full suite run through the installed `rustup` `1.96.1` toolchain (invoked
explicitly via `rustup run 1.96.1 …`), so the resolved `rustc` and `cargo`
were both `1.96.1` (`rustup run 1.96.1 rustc --version` →
`rustc 1.96.1 (31fca3adb 2026-06-26)`; `rustup run 1.96.1 cargo --version` →
`cargo 1.96.1`). The repo-root
[`rust-toolchain.toml`](../../rust-toolchain.toml) (channel `1.96.1`) and the
`RUSTUP_TOOLCHAIN` variable steer **rustup-aware** tooling only (e.g. `rustup`
shims, or `rustup run 1.96.1 …`); a standalone or Homebrew `cargo`/`rustc` on
`PATH` ignores them, so `RUSTUP_TOOLCHAIN=1.96.1 cargo …` does **not** by itself
select `1.96.1` unless the `PATH`-resolved `cargo`/`rustc` are rustup shims. The
[`run.sh`](run.sh) wrapper's only toolchain gate is exact: it runs
`rustc --version` and exits `70` unless it prints exactly `rustc 1.96.1 …`
(verified this session: a `PATH` resolving to Homebrew `rustc 1.97.1` exits `70`
even with `RUSTUP_TOOLCHAIN=1.96.1` set, while `PATH=…/.cargo/bin …
RUSTUP_TOOLCHAIN=1.96.1 ./run.sh fake …` selects `1.96.1` and exits `0`).

| Binary | passed | failed |
|---|---|---|
| `src/lib.rs` unit tests | 275 | 0 |
| `src/main.rs` unit tests | 4 | 0 |
| `tests/fake_e2e.rs` (black-box) | 19 | 0 |
| **Total** | **298** | **0** |

Commands (run from `spikes/s3-macos`, all via `rustup run 1.96.1 …`):
`rustup run 1.96.1 rustc --version`; `rustup run 1.96.1 cargo fmt --check`;
strict clippy `rustup run 1.96.1 cargo clippy --locked --offline --all-targets
--all-features -- -D warnings`;
`rustup run 1.96.1 cargo test --locked --offline --all-targets --all-features`;
and `rustup run 1.96.1 cargo build --locked --offline --release --all-targets`
— **all clean** under Rust 1.96.1.

Two black-box lanes are load-bearing for the ledger:

- `fake_mode_writes_valid_artifacts_and_exits_zero` — exercises the **Fake**
  lane end-to-end and confirms it writes a validated report that is `mode=fake`,
  `harnessOnly=true`, with the Fake lane `Complete` (`EvidenceSource=fixture`)
  and every other lane `Pending`/`NotSelected`. This validates the harness
  plumbing only: report schema, deterministic rendering, atomic artifact writes,
  and exit codes.
- `preflight_missing_nix_bin_exits_69_with_both_valid_artifacts` — drives the
  **Preflight** lane with a caller-supplied absolute `--nix-bin` path asserted
  **not to exist**, and confirms the harness returns a validated **Incomplete**
  report (exit `69`) carrying exactly one `Stage::Preflight` /
  `FailureKind::NixMissing` failure, **still writes** the `report.json` /
  `summary.md` diagnostic artifacts, and **starts no child process** (the spawn
  fails `NotFound` at the version stage before any Nix exec). Companions
  (`preflight_missing_nix_bin_artifacts_leak_no_secrets_or_paths`,
  `preflight_missing_nix_bin_report_is_deterministic_across_runs`) confirm no
  argv/store path/version string leaks and byte-determinism across runs.

> **Neither of these is coverage, capability, build, or signing evidence.** A
> Fake run validates harness plumbing; the missing-Nix Preflight run validates
> the fail-closed `NixMissing` path (no child started). No coverage row,
> capability flag, identity count, build outcome, or signing result from either
> may be read as macOS evidence. See
> [README.md § Fake vs Observed vs Designed](README.md) and
> [README.md § Complete vs Incomplete vs Pending](README.md).

The **`sign_plan_report`** library helper is unit-tested and validates, but it
is a **`Designed`-only** design artifact (`executed == false`, targets
`[Runtime, Installer]`); it records only the intended Runtime/Installer target
shape, is **not** observed signing evidence, **proves neither that signing is
feasible nor that any signing/notarization was executed**, and has **no CLI mode**
(it is exercised only in the unit tests).

---

## 2. Official-source contract review (independently checked this session)

The Preflight lane drives fixed, byte-exact Nix 2.34.8 probe contracts. The
exact **argv** and **JSON output contracts** for each probe were independently
checked against the official **Nix 2.34.8** source and docs:

- **`nix --version`** — the parser requires the single line `nix (Nix) 2.34.8`
  (the pinned release), parsed exactly.
- **`nix flake prefetch --json`** — the global feature argv
  `--extra-experimental-features nix-command flakes` precedes the subcommand;
  the fixed eval-hardening `--option accept-flake-config false` triplet is
  emitted (the universal stable `--option <name> <value>` form generated by Nix
  2.34.8, **not** the non-contract `--accept-flake-config=false` single token);
  the parser requires a `hash` field equal to the pinned flake `narHash` and a
  `storePath` of the form `/nix/store/<base>` (nix-base32 name).
- **`nix store info --store https://cache.nixos.org/`** — feature args precede
  the subcommand; success requires only a normal exit 0 (no output is parsed).
  (`--json` is supported by Nix 2.34.8 but intentionally not emitted, since the
  output is unused.)
- **`nix derivation show`** (per `(system, attr)` cell) — feature args + the
  `accept-flake-config false` **and** `allow-import-from-derivation false`
  triplets (Preflight is build-free, so IFD is disabled) precede the subcommand;
  the parser requires a top-level `version == 4` document with exactly one
  derivation whose map key is a validated store-path **basename** ending `.drv`
  (e.g. `<hash>-hello.drv`, **never** an absolute `/nix/store/…` key), inner
  `version == 4`, a nonempty `name`, the requested canonical `system`, and an
  input-addressed `outputs.out.path` base.
- **`nix path-info --json --json-format 2`** (nonrecursive, then recursive on an
  output hit) — `--json-format 2` is emitted **only** here (never for prefetch
  or derivation show); a zero-exit v2 document with the queried base mapped to
  an `info` entry is a **HIT**, a zero-exit document mapping it to `null` is a
  clean **MISS**, and a nonzero exit with the exact closed diagnostic
  `path '/nix/store/<base>' is not valid` is also a clean **MISS**. Everything
  else is a `CacheQueryFailed` failure.

These contracts, the store-path nix-base32 alphabet (32 chars, excluding
`e`/`o`/`t`/`u`), the `StorePath::MaxPathLen` (211) name bound, the
`--option` form, and the fixed per-probe caps/timeouts (all ≤ the 180 s
`MAX_TIMEOUT` ceiling) were all reviewed against official Nix 2.34.8 source/docs
in this session. The pin itself is the **same canonical pin**
[S4](../s4-reeval-cost/README.md) measures (Nix 2.34.8, `NixOS/nixpkgs` rev
`a62e…d446`, `narHash`
`sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=`), scoped to the two
Darwin systems and three attrs.

The **Detect** lane's `nixbld` build-user group probe is likewise pinned to
the official Nix 2.34.8 installer source (the primary source for this spike's
pin): `scripts/install-multi-user.sh` hard-codes
`readonly NIX_BUILD_GROUP_NAME="nixbld"` and writes
`build-users-group = $NIX_BUILD_GROUP_NAME`; `scripts/install-darwin-multi-user.sh`
sets `NIX_BUILD_USER_NAME_TEMPLATE="_nixbld%d"` and creates those users as
members of the `nixbld` group. The macOS build-user group is therefore
`nixbld` (the probe reads `/Groups/nixbld`), and the macOS build users are
`_nixbld1..N` (the group is **never** `_nixbld`). A live Detect has not been
run (see §5.2), so this is a contract/source review, **not** observed evidence.

> **Runtime behavior of these probes remains unobserved here.** The contracts
> were checked against the source/docs; no real Nix 2.34.8 was executed in this
> environment (see §3), so this is a contract review, **not** measured
> Preflight evidence.

---

## 3. Environment limitation (what was NOT produced)

There is **no usable Nix `2.34.8` installation** in this implementation
environment — `nix` is absent from `PATH` and the common install paths, and no
daemon/store is configured. Consequently this ledger records **no Complete real
Preflight run, no coverage rows, no live Detect, no real native build, and no
signing/notarization validation.**

Only two black-box lanes actually ran against the real `s3-probe` binary:

- the **Fake** lane (no network/Nix/keychain); and
- the **missing-absolute-binary Preflight** lane, where the caller-supplied
  absolute `--nix-bin` does not exist. That path **starts no child process**
  (the spawn fails `NotFound` at the version stage before any Nix exec), touches
  no network and no `/nix` store, and produces only a bounded
  `Incomplete` / `FailureKind::NixMissing` report (exit `69`).

None of the following ran in this environment:

- a **real Preflight** (a version-verified Nix 2.34.8 prefetch + coverage
  matrix);
- a **live Detect** or **`security` keychain probe** (the read-only
  `/usr/bin/security find-identity` identity-count probes);
- a **package build**, **profile activation**, **signing**, **notarization**,
  or **Apple submission**;
- any **`/nix` mutation** or network fetch.

Because no Complete real evidence exists:

- **DR-003 remains `Proposed`.**
- **PR-28 and PR-36 remain directly gated** on S3 macOS evidence; **PR-26's shared
  engine remains gated on S5/DR-005**, with its Darwin policy informed by S3.
- **No architecture, capability, build, or signing decision may be accepted
  from the current evidence.**
- **No numbers/capabilities/identities are invented.** Every result cell below
  is `Pending`.

This is a deliberate non-result, not a gap to paper over. The report schema for
BuildProbe and the `sign_plan_report` helper exist so the lanes can be filled
later; they must **not** be read as build-security, cap, approval, network-
denial, or signing evidence today.

---

## 4. Pin, systems, attrs, cache, and the run command

Exact pin ([`fixtures.json`](fixtures.json)):

| Field | Value |
|---|---|
| Pin schema version | `1` |
| Nix version (Preflight target; probe requires exact equality) | `2.34.8` |
| Nixpkgs owner / repo | `NixOS` / `nixpkgs` |
| Rev | `a62e6edd6d5e1fa0329b8653c801147986f8d446` |
| Flake `narHash` | `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=` |
| Systems | `x86_64-darwin`, `aarch64-darwin` |
| Attrs | `hello`, `ripgrep`, `git` |
| Cache store URL | `https://cache.nixos.org/` |

The six canonical Preflight coverage cells, in fixed system-major order:
`(x86_64-darwin, hello)`, `(x86_64-darwin, ripgrep)`, `(x86_64-darwin, git)`,
`(aarch64-darwin, hello)`, `(aarch64-darwin, ripgrep)`, `(aarch64-darwin, git)`.

**Run command to use later (Complete real Preflight), after the prerequisites in
[README.md § Prerequisites](README.md) are met** (absolute Nix `2.34.8`,
configured store, network for prefetch):

```sh
# From this directory (spikes/s3-macos), rustc 1.96.1 on PATH:
cargo build --locked --offline --release
./target/release/s3-probe preflight --nix-bin /opt/nix/bin/nix
#   exit 0  -> Complete (the only Preflight state that is coverage evidence)
#   exit 69 -> Incomplete (diagnostic only; NOT evidence)
# equivalent wrapper (requires rustc 1.96.1 already on PATH; builds
# locked+offline; defaults --out-dir to target/s3-preflight; prints an effect
# warning; echoes summary.md; preserves the runner's exit status):
./run.sh preflight /opt/nix/bin/nix
```

**Run command for a real Detect** (macOS host only; reads default-keychain
identity metadata/counts plus `nixbld` build-group/`_nixbld*` member metadata; no
credentials/writes/signing):

```sh
./target/release/s3-probe detect
#   exit 0  -> Complete (capability presence OR absence; both are evidence)
#   exit 69 -> Incomplete (internal probe failure / off-macOS; NOT evidence)
./run.sh detect
```

Artifacts are written under `--out-dir` (the binary defaults to `.`; the
`run.sh` wrapper defaults to `target/s3-{mode}`): `report.json` and `summary.md`,
via an atomic sibling-temp rename.

---

## 5. Pending evidence matrices

**Every cell is `Pending`.** Do not transcribe a `true`/`false`/count that was
not produced by a reviewed Complete real run.

### 5.1 Preflight coverage matrix (six canonical cells)

For each cell: is the **output** path available on `cache.nixos.org`, and is its
**closure** available? (`closure` may be `true` only when `output` is `true`.) A
`false` row is honest evidence of a miss, never a harness failure.

| System | Attr | output available | closure available |
|---|---|---|---|
| `x86_64-darwin` | `hello` | Pending | Pending |
| `x86_64-darwin` | `ripgrep` | Pending | Pending |
| `x86_64-darwin` | `git` | Pending | Pending |
| `aarch64-darwin` | `hello` | Pending | Pending |
| `aarch64-darwin` | `ripgrep` | Pending | Pending |
| `aarch64-darwin` | `git` | Pending | Pending |

Also pending: the two Preflight gates that make the matrix meaningful —
`nixVersionExact` (detected Nix == `2.34.8`) and `flakePrefetchVerified`
(pinned `narHash` matched).

### 5.2 Detect capabilities / build-user / toolchain

| Capability | Value |
|---|---|
| `nixPresent` (optional `--nix-bin`, existence-only; never executed) | Pending |
| `toolCapabilities.codesign` (`/usr/bin/codesign`) | Pending |
| `toolCapabilities.xcrun` (`/usr/bin/xcrun`) | Pending |
| `toolCapabilities.notarytool` (via `xcrun --find notarytool`) | Pending |
| `toolCapabilities.stapler` (`/usr/bin/stapler`) | Pending |
| `toolCapabilities.productbuild` (`/usr/bin/productbuild`) | Pending |
| `toolCapabilities.productsign` (`/usr/bin/productsign`) | Pending |
| `toolCapabilities.pkgbuild` (`/usr/bin/pkgbuild`) | Pending |
| `toolCapabilities.spctl` (`/usr/sbin/spctl`) | Pending |
| `toolCapabilities.security` (`/usr/bin/security`) | Pending |
| `xcodeSelection` (Absent / CommandLineTools / FullXcode, via `xcode-select -p`) | Pending |
| `applicationIdentityCount` ("Developer ID Application" identities) | Pending |
| `installerIdentityCount` ("Developer ID Installer" identities) | Pending |
| `nixbldGroupPresent` (`nixbld` group, via `dscl . -read /Groups/nixbld`) | Pending |
| `nixbldUserCount` (`nixbld` group member count; `_nixbld*` users) | Pending |

These are **read-only detections** (identity *counts* and tool *presence*; never
identity names, paths, or credentials). Pending until a live Detect runs on a
managed macOS host.

### 5.3 BuildProbe (native sandboxed Darwin build) — `Pending` in every run

BuildProbe has **no CLI and no orchestrator** in this spike, so it is
`Pending`/`NotSelected` in every report this spike can produce. It stays that
way until a managed macOS / [S5](../../plans/11-pr-roadmap.md) real harness
performs an actual native build. The rows below describe what that future lane
would record; none are observed today.

| Row | Value |
|---|---|
| `builtSystem` (`x86_64-darwin` / `aarch64-darwin`) | Pending |
| `sandboxEnforced` (`sandbox=true`) | Pending |
| `sandboxFallbackDisabled` (`sandbox-fallback=false`) | Pending |
| `buildUsersReady` (`_nixbld` build-time readiness) | Pending |
| `networkDenied` (sandbox blocks network during build) | Pending |
| `approvalRecorded` (explicit single-operation approval) | Pending |
| `resourceCapsEffective` (ledger-only; no current schema field; S5/DR-005/PR-26 evidence) | Pending |

> **Cap/approval/network-denial effectiveness is owned jointly by [S5]
> / [DR-005] / [PR-26](../../plans/11-pr-roadmap.md).** The current BuildProbe
> **schema** (booleans for sandbox/fallback/users/network/approval) is a
> data-contract placeholder and **must not be read as evidence that resource
> caps, approval gates, or network denial are *effective*** — that requires the
> S5 managed-build harness on both Linux and macOS, which has not run here. The
> `resourceCapsEffective` table row is **ledger-only**: no such schema field
> exists today (it is tracked here precisely because cap *effectiveness* is
> S5/DR-005/PR-26 evidence, not something this spike's report can produce).

### 5.4 Signing / notarization validation — `Pending`

| Row | Value |
|---|---|
| Installer/runtime signing validated (codesign) | Pending |
| Notarization validated (notarytool submission + staple) | Pending |

The `sign_plan_report` helper is a **`Designed`** plan (`executed == false`,
targets `[Runtime, Installer]`) that records only the intended Runtime/Installer
target shape; it is **not** observed signing evidence and proves **neither that
signing is feasible nor that any signing/notarization was executed**.
Signing/notarization evidence requires real credentials, an Apple submission, and
a managed macOS lane — none of which this environment provides.

---

## 6. Evidence acceptance checklist

`EvidenceSource` is a **classification** label that `Report::validate`
cross-checks for lane/source consistency, **not** a provenance attestation. It
does not prove who produced a report, whether the runner/binary/host is genuine,
or that a value labeled `observed` was truly observed. The public
`preflight_report` / `detect_report` builders (see [`README.md`](README.md))
accept any `dyn CommandRunner` / `dyn ProbeRunner`; a unit or custom runner can
return fabricated observations that still carry the `observed` label and still
validate. Only the `s3-probe` CLI wires the built-in `RealRunner`, so an
injected-runner report — including every unit-test report — is a **simulation,
never admissible evidence**, even when its schema label is `observed`.
`report.json` and `summary.md` are unsigned spike artifacts (see
[README.md § Why classification is not attestation](README.md)).

Before any value from a real Detect/Preflight run may be cited for DR-003 /
PR-26 / PR-28 / PR-36, the produced report must satisfy **all** of the
following. Treat this as a **process requirement, not cryptographic attestation**:
meeting it makes a report *candidate* evidence, not proof.

- [ ] Harness internal validation passes and the run exits `0` (Complete), not
      `69` (Incomplete) and not `64`/`70`.
- [ ] The run was a **reviewed `s3-probe` CLI execution** (not a library/unit
      call) using the built-in **`RealRunner`**; `report.json` and `summary.md`
      were reviewed by a human before any value was cited.
- [ ] For Preflight, a **trusted, provenance-checked Nix `2.34.8` binary and
      host**; for Detect, a **trusted macOS host**. (`--nix-bin` provenance and
      host trust are the caller's responsibility — see
      [README.md § Security and caveats](README.md).)
- [ ] `report.mode` is `detect` or `preflight` (not `fake`); for the inactive
      lanes, every non-active lane is `Pending`/`NotSelected`.
- [ ] The active lane's `observation.source` is `observed` (not `fixture`, not
      `designed`). This is a classification check only — it does not prove the
      value was genuinely observed (see the intro above).
- [ ] `report.harnessOnly` is `false`.
- [ ] **Exact** environment recorded: the embedded pin matches
      [`fixtures.json`](fixtures.json) (Nix `2.34.8`, rev `a62e…d446`,
      `narHash` `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=`, the two
      Darwin systems, the three attrs, `https://cache.nixos.org/`); for Preflight
      `nixVersionExact == true` and `flakePrefetchVerified == true`.
- [ ] For Preflight: **all six** canonical coverage cells are present in
      system-major order, each with an honest `output`/`closure` boolean; for
      Detect: every capability flag and count is present.
- [ ] No recorded failures; no skipped samples; the report round-trips through
      serde and **re-validates**.
- [ ] Attachments preserved with the decision: `report.json`, `summary.md`, and
      host context (OS/version, arch, Nix version/path, Xcode selection, whether
      the store/evaluator cache was warm).
- [ ] Ideally **repeated runs** and stability recorded across runs. For
      **Preflight**, a single host with a trusted exact Nix `2.34.8` already
      produces all six coverage cells spanning *both* Darwin **target** systems —
      do **not** require running on both macOS host architectures merely to
      validate the cache query (host context is still recorded). For **Detect**
      (and BuildProbe), host architecture is intrinsic to the observation, so
      evidence is recorded per Darwin host.

For BuildProbe and signing/notarization, **additionally** (these cannot be
satisfied from this spike alone):

- [ ] A real native sandboxed Darwin build under `_nixbld*` build users (BuildProbe) from a
      managed macOS / [S5](../../plans/11-pr-roadmap.md) harness, with
      `sandbox=true`/`sandbox-fallback=false` and **effective** network denial,
      approval, and resource caps (cap/approval/network effectiveness co-owned
      with [S5] / [DR-005] / [PR-26](../../plans/11-pr-roadmap.md)).
- [ ] A notarized installer/runtime that builds, signs (codesign), submits
      (notarytool), and staples successfully end-to-end.

---

## 7. Decision

**No macOS build-security or signing/notarization decision may be accepted from
the current evidence.** What exists today is harness-correctness evidence (the
Fake plumbing and the fail-closed `NixMissing` path) and an official-source
contract review of the exact Nix 2.34.8 probe argv/JSON — there are **zero**
Complete real coverage rows, **zero** live Detect capability/identity readings,
**zero** native builds, and **zero** signing validations, by environment
limitation, not by choice.

**Next actions:** (1) run a real Preflight against a real Nix `2.34.8` on a
managed macOS host and fill the coverage matrix in §5.1 from reviewed
`report.json` outputs; (2) run a live Detect on the same host and fill §5.2;
(3) defer BuildProbe and signing/notarization to the managed macOS / [S5] lane
and fill §5.3/§5.4; then satisfy the checklist in §6 and **update this file and
[DR-003](../../plans/12-open-decisions-and-risks.md)** with reviewed values.
Until that happens, [DR-003](../../plans/12-open-decisions-and-risks.md) stays
`Proposed`; the direct S3 gates [PR-28](../../plans/11-pr-roadmap.md) and
[PR-36](../../plans/11-pr-roadmap.md) stay gated on S3 macOS evidence, while
[PR-26](../../plans/11-pr-roadmap.md) (shared local-build engine) stays gated on
[S5]/[DR-005] for its shared mechanism (its Darwin policy informed by S3).

---

## References

- [README.md](README.md) — spike harness documentation (Fake vs Observed vs
  Designed, Complete vs Incomplete vs Pending, prerequisites, effect boundaries,
  exit codes).
- [`fixtures.json`](fixtures.json) — embedded, validated pin.
- [`run.sh`](run.sh) — executable wrapper (effect warnings; closed grammar).
- [DR-003 in `plans/12-open-decisions-and-risks.md`](../../plans/12-open-decisions-and-risks.md)
  — the decision record this evidence feeds.
- [PR-7 in `plans/11-pr-roadmap.md`](../../plans/11-pr-roadmap.md) — spike owner.
  Downstream macOS evidence gates: PR-28/PR-36 direct on S3; PR-26 (shared
  local-build engine) gated on S5/DR-005 with Darwin policy informed by S3.

[S5]: ../../plans/11-pr-roadmap.md
[DR-005]: ../../plans/12-open-decisions-and-risks.md
