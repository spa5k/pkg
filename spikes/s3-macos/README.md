# Spike S3 — macOS binary coverage + signing/notarization evidence harness (PR-7 / DR-003)

`pkg-spike-s3-macos` is an **evidence harness**, not production `pkg` code. It
builds a closed, five-lane evidence model for the macOS binary-coverage and
signing/notarization questions behind [DR-003](../../plans/12-open-decisions-and-risks.md),
and drives them through a hardened, fail-closed `s3-probe` binary. It is a
standalone Cargo workspace (own `Cargo.toml`, `Cargo.lock`, `target/`) and is
excluded from the repo-root workspace lanes — the same isolation model as
[`spikes/s2-tough/`](../s2-tough/) and [`spikes/s4-reeval-cost/`](../s4-reeval-cost/).
The pin lives in [`fixtures.json`](fixtures.json); the harness embeds that file at
compile time and validates it ([`src/validate.rs`](src/validate.rs)).

> **This spike is primarily an evidence harness.** A reviewed Complete real
> Detect run was added on 2026-08-10; see
> [`evidence/2026-08-10-detect-aarch64-darwin/RUN.md`](evidence/2026-08-10-detect-aarch64-darwin/RUN.md)
> and the current addendum in [`FINDINGS.md`](FINDINGS.md). Cache Preflight and
> real Developer-ID signing/notarization remain Pending.
>
> The harness itself is not a result. It is the mechanism that produces evidence.
> See [Fake vs Observed vs Designed](#fake-vs-observed-vs-designed) and
> [Complete vs Incomplete vs Pending](#complete-vs-incomplete-vs-pending). No
> coverage, capability, build, or signing outcome lives in this repo. The existing
> [`FINDINGS.md`](FINDINGS.md) ledger records only the harness proof, the
> official-source contract review, and the environment limitation today; it is
> updated with a conclusion for a lane only *after* a reviewed Complete real run
> for that lane exists.

## ⚠ Read this before running effectful modes

Three modes are wired to the binary; two of them have host/network/store
**effects** when explicitly invoked. Understand the boundary first:

- **`detect`** runs the **read-only** macOS host Detect lane. When explicitly
  invoked it reads **default-keychain identity metadata/counts** (via
  `/usr/bin/security find-identity`) **plus `nixbld` build-group / `_nixbld*`
  member metadata**
  (filesystem presence of fixed Apple tools, `xcode-select -p`, `xcrun --find
  notarytool`, `dscl … /Groups/nixbld`). It **never** accepts credentials,
  **never** unlocks/signs/notarizes, and **never** writes keychain data. It only
  records booleans, a closed Xcode enum, and bounded counts — never an identity
  name, tool path, hostname, username, or profile.
- **`preflight`** runs the Preflight cache-coverage lane. It is **build-free and
  activation-free but NOT read-only.** It **executes the caller-supplied absolute
  Nix binary** (its exact Nix 2.34.8 version is verified at runtime). Its probes
  are: `nix --version`, then `nix flake prefetch --json` (which **may fetch the
  pinned GitHub source and write normal Nix store/fetch/eval state**), then
  `nix store info --store <cache>` and `nix path-info …` **availability
  queries that target `cache.nixos.org`**. Within the `s3-probe` probes there is
  **no shell, no `PATH` lookup, no package build, no profile activation, and no
  signing** (the `run.sh` wrapper resolves `cargo`/`rustc` on `PATH` itself; see
  [Security and caveats](#security-and-caveats)).
- **`fake`** is pure-harness: no network, no Nix, no keychain. It validates the
  harness plumbing only. It is **not** evidence.

## What it models

Five lanes, one report per run, one ACTIVE lane selected by the mode:

- **Fake** — pure-harness lane. Exercises the report schema, atomic artifact
  writes, deterministic rendering, and exit codes with deterministic fixtures.
  No network, no Nix, no keychain. Never evidence.
- **Detect** — read-only macOS host capability detection: the macOS
  signing/notarization/packaging tool capabilities, the active Xcode
  classification, Developer ID identity *counts*, `nixbld` build-user group
  presence/count (`_nixbld*` members), and optional Nix presence (existence check only).
- **Preflight** — `cache.nixos.org` binary-coverage availability matrix for the
  pinned attrs/systems, gated by an exact-Nix-version check and a verified
  pinned flake prefetch.
- **BuildProbe** — a real native sandboxed Darwin build. **Has no CLI and no
  orchestrator in this spike**; it stays `Pending` until a managed macOS / [S5]
  real harness. Its report schema is defined but unused here.
- **SignPlan** — the Apple signing/notarization *plan*. Exposed only as the
  library helper [`sign_plan_report`](src/runner.rs) — **`Designed`-only,
  `executed == false`, no CLI, no credentials, and not signing evidence**.

Each observation carries an explicit `EvidenceSource`. This is a
**classification** label the validator cross-checks against the lane mode (a
Fake lane's source must be `Fixture`, a SignPlan lane's must be `Designed`,
every other lane's must be `Observed`), **not** an attestation of provenance or
truth. It does not prove who produced the report, that the runner/binary/host is
genuine, or that values labeled `Observed` were genuinely observed — see
[Fake vs Observed vs Designed](#fake-vs-observed-vs-designed) and
[Evidence admission](#evidence-admission).

## Fake vs Observed vs Designed

Every observation carries an `EvidenceSource`. This is a **classification** label
that [`Report::validate`](src/report.rs) cross-checks for lane/source consistency
(each mode admits exactly one source), **not** a provenance attestation. It does
not establish who produced a report, whether the runner/binary/host is genuine,
or whether a value labeled `Observed` was in fact observed.

- **`Fixture`** — synthetic, deterministic fixture-driven value. Admitted only in
  the **Fake** lane. A Fake report is `harnessOnly: true` and proves the harness
  plumbing (capture, report schema, atomic writes, exit codes), **nothing else**.
- **`Observed`** — a value *classified* as observed on the host/cache/toolchain.
  Admitted only in **Detect** / **Preflight** / **BuildProbe** lanes. The label
  is declared by the report builder, not proven by the validator, so a Complete
  `Observed` lane is *necessary* for evidence but **not sufficient** on its own
  (see [Evidence admission](#evidence-admission)).
- **`Designed`** — a planned, never-executed value. Admitted only in the
  **SignPlan** lane. `sign_plan_report` carries `executed == false` and the fixed
  target order `[Runtime, Installer]`. It records only the *intended*
  Runtime/Installer target shape; it is **not** observed signing evidence and
  proves neither that signing is feasible nor that any signing/notarization was
  executed.

### Why classification is not attestation

`Report::validate` enforces lane/state/source **consistency** only — it never
authenticates the runner, the `s3-probe` binary, or the host, and it never checks
that a value labeled `Observed` was truly observed. Two consequences matter:

- **Injected-runner reports are simulations.** The public
  [`preflight_report`](src/runner.rs) and [`detect_report`](src/runner.rs)
  builders accept any `dyn CommandRunner` / `dyn ProbeRunner`. A unit test or a
  custom runner can return fabricated observations, and those still carry the
  `Observed` label and still pass validation. The `s3-probe` CLI is the only
  path that wires the built-in [`RealRunner`](src/command.rs); an
  injected-runner report — including every unit-test report — is a simulation,
  **never admissible evidence**, even when its schema label is `Observed`.
- **`report.json` / `summary.md` are unsigned spike artifacts.** Neither file is
  signed, sealed, or attested. A caller-supplied Preflight `--nix-bin` is a
  caller-supplied absolute executable (see
  [Security and caveats](#security-and-caveats)); a malicious or compromised
  binary can fabricate output or act (spawn, write, exfiltrate) *before* it
  returns any text, so the version check is a contract gate, not a trust gate.

## Evidence admission

Because `EvidenceSource` is classification, not attestation, a Complete
`Observed` report is at best **candidate** evidence. Treat the following as a
**process requirement, not cryptographic attestation** — only a run that meets
*all* of these may be considered candidate evidence for DR-003:

1. a **reviewed `s3-probe` CLI execution** (not a library/unit call) that uses
   the built-in **`RealRunner`**;
2. a **trusted, provenance-checked Nix binary and host** for Preflight, and a
   trusted macOS host for Detect;
3. **recorded host/run context** (OS/version, arch, Nix version/path, Xcode
   selection, cache warmth) attached alongside `report.json` / `summary.md`;
4. **`Complete` validation** — the run exits `0`, the report re-validates, no
   failures, and (Preflight) all six canonical coverage cells are present;
5. **artifact review** of `report.json` and `summary.md` by a human before any
   value is cited.

This gate is conservative on purpose: none of it is proven by `Report::validate`,
and meeting it makes a report *candidate* evidence, not proof.

## Complete vs Incomplete vs Pending

A lane's data is one of three states:

- **`Pending`** — not exercised this run (the natural state of every inactive
  lane; `BuildProbe` is `Pending` in *every* run this spike can produce). Requires
  a closed `PendingReason`. Never evidence.
- **`Incomplete`** — attempted but not finished cleanly. Requires ≥1 closed
  `Failure` (enum `stage` + enum `kind` only — never raw child output, path, or
  message). Both artifacts are **still written** as the diagnostic record. The
  CLI exits `69`. An Incomplete report **is not evidence.**
- **`Complete`** — finished with a recorded observation. Requires an observation,
  no reason, no failures. The CLI exits `0`. For **Detect**/**Preflight**, a
  Complete `Observed` lane is *necessary* for evidence but, on its own, not
  sufficient (see [Evidence admission](#evidence-admission)); for **Fake**,
  Complete is harness-plumbing only; for **SignPlan**, Complete is
  `Designed`-only and not evidence.

For **Detect**, capability *absence* (no Xcode, zero identities, missing tools,
no Nix) is a **Complete** observation — an honest negative. Only an internal
probe failure (timeout, cap overflow, malformed output, a nonzero `security`
exit, or a live Detect off-macOS) makes the lane **Incomplete**.

For **Preflight**, a cache **MISS** (output or closure unavailable) is an
availability observation (a `false` coverage row), **never** a failure. Only an
internal failure (Nix missing/wrong version, prefetch failure, malformed output,
store-info/path-info protocol failure, timeout) makes the lane **Incomplete**.

## Pin (from fixtures.json)

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

This is the **same canonical pin** [S4](../s4-reeval-cost/README.md) measures
(Nix 2.34.8, `NixOS/nixpkgs` rev `a62e…d446`), scoped to the two Darwin systems
and three fixture attributes S3 covers, plus the single v1 cache store URL. The
flake `narHash` is the **verified trust input** for Preflight's `nix flake
prefetch`. It **intentionally does not duplicate S4's raw-archive hash**: that
hash concerns GitHub tarball bytes and is irrelevant to S3's
substitution/build/sign questions.

The six canonical Preflight coverage cells, in fixed system-major order, are the
product of (systems × attrs):
`(x86_64-darwin, hello)`, `(x86_64-darwin, ripgrep)`, `(x86_64-darwin, git)`,
`(aarch64-darwin, hello)`, `(aarch64-darwin, ripgrep)`, `(aarch64-darwin, git)`.
A Complete Preflight observation carries exactly these six cells in exactly this
order; a partial (Incomplete) observation is a prefix of this sequence.

## Prerequisites

The harness **installs and configures nothing.** All of the following must
already be true on the host. **Detect** (and a real native **BuildProbe**)
require a macOS host (`x86_64-darwin`/`aarch64-darwin`); the host target is fixed
at compile time. **Preflight** only *queries* the fixed Darwin *target* systems
— it can technically run on any supported host that has a trusted exact Nix
`2.34.8`, a configured store/daemon, and network, so neither macOS host
architecture is required merely to validate the cache query (the matrix already
spans both Darwin target systems from a single host). Host context is still
recorded: Detect records its detected `hostSystem`, and the run/ledger records
host OS/version/arch/Nix.

- **Rust exactly `1.96.1`.** The repo pins `channel = "1.96.1"` in the root
  [`rust-toolchain.toml`](../../rust-toolchain.toml); the spike declares
  `rust-version = "1.96"`, edition 2024. `rust-toolchain.toml` and the
  `RUSTUP_TOOLCHAIN` variable are honored **only by rustup-aware tooling**
  (e.g. `rustup` shims, or an explicit `rustup run 1.96.1 …`); a standalone or
  Homebrew `cargo`/`rustc` on `PATH` **ignores** them, so you must ensure the
  `PATH`-resolved `cargo`/`rustc` really resolve to the `1.96.1` toolchain. The
  [`run.sh`](run.sh) wrapper enforces that by requiring `rustc --version` to
  print exactly `rustc 1.96.1 …` and exiting `70` otherwise; it does not trust
  `rust-toolchain.toml`/`RUSTUP_TOOLCHAIN` on their own.
- **Cached Cargo dependencies** for offline commands. The spike depends only on
  `serde`, `serde_json`, and `rustix` (locked in its own `Cargo.lock`).
  `cargo build --locked --offline` works once those crates are in the local
  registry cache; no network is needed to build.
- **For Detect only — a real macOS host** with the fixed Apple tool paths it
  probes (`/usr/bin/security`, `/usr/bin/xcode-select`, `/usr/bin/xcrun`,
  `/usr/bin/dscl`, etc.). A live Detect reads the **default keychain** via
  `/usr/bin/security find-identity`. Off-macOS the lane is forced **Incomplete**
  (a `Detect/Unknown` failure); no host system value is invented.
- **For Preflight only — an absolute `nix` binary at version `2.34.8` plus a
  configured Nix store/daemon**, passed via `--nix-bin`. The version probe runs
  `nix --version` and requires the parsed version to equal the pin *exactly*
  (`2.34.8`); any mismatch fails the lane. `nix flake prefetch` then reaches the
  network to fetch the pinned source and **writes ordinary Nix-managed state**;
  the `nix store info`/`nix path-info` availability queries target
  `cache.nixos.org`. **Trust:** `--nix-bin` is a caller-supplied absolute
  executable, not a pin — the version check verifies only the returned text, not
  binary provenance, hash, signature, or safety. Run Preflight only against a
  trusted Nix installation (see [Security and caveats](#security-and-caveats)).

## Running

A shipped executable wrapper, [`run.sh`](run.sh), is the recommended path. It
requires `rustc 1.96.1` **already resolved on `PATH`**, then runs the binary
with `cargo run --locked --offline --release --bin s3-probe`. The wrapper
**itself** never invokes `rustup` or any installer and downloads nothing. Its
only toolchain gate is exact: it runs `rustc --version` and exits `70` unless it
prints exactly `rustc 1.96.1 …`. `RUSTUP_TOOLCHAIN=1.96.1` and the repo-root
[`rust-toolchain.toml`](../../rust-toolchain.toml) steer **rustup-aware** tools
only (e.g. `rustup` shims, or `rustup run 1.96.1 …`); a standalone or Homebrew
`cargo`/`rustc` on `PATH` ignores them and is caught by that exact-version check
rather than silently selected. When the `PATH`-resolved `cargo`/`rustc` are
`rustup` shims, `rust-toolchain.toml` (channel `1.96.1`) can cause `rustup` to
obtain the toolchain if it is not already installed; `cargo --offline`
constrains crate/dependency access only and does **not** prevent `rustup`
toolchain acquisition. Callers who require zero network must preinstall or
preselect the `1.96.1` toolchain and configure their toolchain manager
themselves (the wrapper adds no installation step). It defaults `--out-dir` to
`target/s3-{fake,detect,preflight}`, **preserves the runner's exit status**, and
echoes `summary.md` under a fixed `--- summary.md ---` header if one was
produced. It prints a one-line **effect warning to stderr** before the Detect
and Preflight modes. The wrapper's Detect does **not** accept an optional
`--nix-bin`; to pass one, run the binary directly (see below).

```sh
# Fake (no network, no Nix, no keychain) — validates harness plumbing only:
./run.sh fake                       # out -> target/s3-fake
./run.sh fake custom/out

# Detect (read-only host lane; reads default-keychain identity metadata/counts
# plus nixbld build-group/_nixbld* member metadata; no credentials/writes/signing):
./run.sh detect                     # out -> target/s3-detect
./run.sh detect custom/out

# Preflight (executes the supplied absolute Nix binary; exact 2.34.8 verified;
# NOT read-only: prefetch may write Nix state, availability queries target
# cache.nixos.org; no build/activation/signing):
./run.sh preflight /opt/nix/bin/nix   # out -> target/s3-preflight
./run.sh preflight /opt/nix/bin/nix custom/out
```

You can also run the binary directly. Build it first (`--locked --offline`, once
deps are cached), then invoke it; `--out-dir` then defaults to `.`:

```sh
# Build (locked + offline, once deps are cached):
cargo build --locked --offline --release

# Fake (no network, no Nix, no keychain) — validates harness plumbing only:
./target/release/s3-probe fake
./target/release/s3-probe fake --out-dir ./out

# Detect (read-only host lane):
./target/release/s3-probe detect
./target/release/s3-probe detect --out-dir ./out
# Detect with an OPTIONAL absolute Nix binary (existence check only; never
# executed, never PATH-searched):
./target/release/s3-probe detect --nix-bin /opt/nix/bin/nix --out-dir ./out

# Preflight (requires an absolute Nix binary; executes it):
./target/release/s3-probe preflight --nix-bin /opt/nix/bin/nix
./target/release/s3-probe preflight --nix-bin /opt/nix/bin/nix --out-dir ./out

# Usage banner:
./target/release/s3-probe --help
```

During development the same thing via Cargo:

```sh
cargo run --locked --offline --release -- fake
cargo test --locked --offline     # unit + the black-box Fake + missing-Nix e2e tests
```

The grammar (closed parser, no `--flag=value`, no abbreviations, no positionals):

```text
s3-probe --help|-h
s3-probe fake     [--out-dir PATH]
s3-probe detect   [--out-dir PATH] [--nix-bin ABSOLUTE_PATH]
s3-probe preflight --nix-bin ABSOLUTE_PATH [--out-dir PATH]
```

The parser **rejects**: duplicate flags, `--flag=value` equals forms,
abbreviations, positional tokens, recognized flags before the mode keyword,
`--nix-bin` in `fake` mode, a relative or empty `--nix-bin`, a missing
`--nix-bin` in `preflight` mode, and **every signing credential-shaped option**
(`--identity`, `--keychain`, `--password`, `--team-id`, …). `--help`/`-h` must be
standalone; any trailing token is a closed, bounded error that never echoes the
token.

## Outputs and exit codes

**After a report is produced**, the harness writes two artifacts under
`--out-dir` (the binary defaults to `.`; the `run.sh` wrapper defaults to
`target/s3-{mode}`) via an atomic sibling-temp rename (`create_new` +
`fsync` + rename, with collision-bounded retries). This does not crash with a
partial file and **does not open/follow a pre-existing *final-artifact*
symlink**: `create_new` fails on the uniquely-named temp and `rename` replaces
the final directory entry in place. It is **not** production path-hardening:
`create_dir_all` and the path walk can still follow an `--out-dir` or ancestor
symlink, so a planted out-dir symlink is not defended against. Treat
`--out-dir` as trusted:

- `report.json` — the validated, machine-readable report (pretty JSON, one
  trailing newline). See [`src/report.rs`](src/report.rs) for the schema.
- `summary.md` — the deterministic human-readable rendering of the same report.

This holds for a successful **Fake** run, a **Complete** Detect/Preflight run,
and an **Incomplete** Detect/Preflight run (the diagnostic record). It does
**not** hold for `EX_USAGE` (`64`) / `EX_SOFTWARE` (`70`) or any CLI-parse,
internal, pre-report, or artifact-write failure: those produce no report, write
no files, and any pre-existing contents of `--out-dir` may remain.

Exit codes (from [`src/main.rs`](src/main.rs)):

| Code | Meaning |
|---|---|
| `0` | Success: `--help`, a Fake run, or a **Complete** Detect/Preflight run. Artifacts written. |
| `64` | `EX_USAGE`: CLI parse error (bad mode, missing/relative `--nix-bin`, signing option, non-standalone `--help`, etc.). No artifacts. |
| `69` | `EX_UNAVAILABLE`: Detect/Preflight run returned a validated **Incomplete** report (e.g. Nix missing → `FailureKind::NixMissing`). **Artifacts are still written** (the diagnostic record); no dynamic child output is printed. |
| `70` | `EX_SOFTWARE`: internal/software/artifact-write failure. For Detect/Preflight, pre-existing output-dir contents may remain. |

Each stderr line is a single, fixed, caller-data-free label (paths, argv, store
paths, hashes, and child output are **never** echoed). The hidden
`s3-probe-fixture-child` fixture children spawned by the integration tests are an
intentional exception: they emit deterministic fixture output to exercise the
capture path. The binary **never** prints a real Nix identity name, store path,
version string, or hash.

## Effect boundaries (what each lane actually does)

- **Fake** — no process spawn for evidence (only the hidden fixture children in
  tests), no network, no Nix, no keychain, no `/nix` mutation, no signing.
- **Detect** — fixed absolute macOS tool paths only (`/usr/bin/security`,
  `/usr/bin/xcode-select`, `/usr/bin/xcrun`, `/usr/bin/dscl`, and the nine fixed
  packaging/signing tool paths). It **reads default-keychain identity
  metadata/counts** via `/usr/bin/security find-identity` and reads
  `nixbld` build-group / `_nixbld*` member metadata. It **never** accepts credentials,
  **never** unlocks/signs/notarizes, **never** writes keychain data, and
  **never** executes the optional `--nix-bin` (existence check only). No `/nix`
  mutation. The only recorded values are booleans, a closed Xcode enum, and
  bounded counts.
- **Preflight** — **executes** the caller-supplied absolute `nix` binary
  (version-verified to exactly `2.34.8`) for **build-free** probes only:
  `nix --version`, `nix flake prefetch --json` (may **fetch the pinned GitHub
  source** and **write normal Nix store/fetch/eval state**), and
  `nix store info` / `nix path-info` **availability queries that target
  `cache.nixos.org`**. There is **no package build, no profile activation, and no
  signing**, and **no shell or `PATH` lookup in any `s3-probe` probe** (the
  wrapper resolves `cargo`/`rustc` on `PATH` itself). Build-free/activation-free
  does **not** mean read-only or mutation-free.
- **BuildProbe** — **no CLI, no orchestrator, never executed in this spike.**
  Its report schema is defined and unit-tested but every run leaves it
  `Pending`/`NotSelected`. It stays that way until a managed macOS / [S5] real
  harness performs an actual native sandboxed Darwin build.
- **SignPlan** — the **`sign_plan_report`** library helper only. It is
  `Designed`-only, `executed == false`, with the fixed target order
  `[Runtime, Installer]`. It takes **no runner, performs no process spawn, no
  Nix execution, no build, no signing, no notarization, no Apple submission, and
  no credential lookup.** It is a plan, **not** signing evidence.

## Security and caveats

- ⚠ **Executable trust is the caller's responsibility (Preflight).** `--nix-bin`
  is a caller-supplied **absolute executable**, not a pin. The first
  `nix --version` output check proves only that the returned text matches the
  pinned `2.34.8` contract — **not** binary provenance, hash, code signature, or
  safety. A malicious binary can act (spawn, write, exfiltrate) *before* it
  returns any version text, so the version check is a contract gate, not a trust
  gate. Run Preflight only against a **trusted** Nix installation you control.
  Production managed-Nix provenance is **outside this spike** and is owned by the
  managed installer/runtime lanes.
- **Absolute program paths in the probes; the wrapper uses `PATH` by design.**
  Detect probes fixed absolute Apple program paths with exact argv; Preflight
  probes the caller-supplied absolute `nix` path (`--nix-bin`, handed to the OS
  verbatim as the program, which must be absolute) with exact argv. In any
  `s3-probe` child/probe there is **no shell, no `command -v`, no `PATH` lookup,
  and no `NIX_PATH`**. The `run.sh` wrapper **does** resolve `cargo` and
  `rustc` on `PATH` via `command -v`; it pins neither executable's
  identity/provenance even though it enforces exactly `rustc 1.96.1 …`, installs
  nothing, and downloads nothing. `RUSTUP_TOOLCHAIN=1.96.1` and the repo-root
  `rust-toolchain.toml` steer **rustup-aware** tools only; a standalone or
  Homebrew `rustc`/`cargo` on `PATH` ignores them, and a `PATH`-resolved
  `rustc`/`cargo` may be a `rustup` shim whose `rust-toolchain.toml` can cause
  `rustup` to acquire the `1.96.1` toolchain if it is not already installed (see
  [Running](#running)); `cargo --offline` constrains crates, not toolchain
  acquisition. The wrapper's real guarantee is the exact `rustc --version`
  check, not those selection controls. That `command -v` resolution is the
  harness's own PATH lookup, not a probe.
- **Minimal, fail-closed child environment.** `Command::env_clear()` is applied,
  then exactly `LANG=C`, `LC_ALL=C` — nothing else is inherited.
- **Bounded, fixed diagnostics.** Every error message is a fixed ASCII label
  with no caller-controlled text (paths, argv, store paths, hashes, and child
  output are never echoed); I/O failures are reduced to a stable
  `io::ErrorKind` token. The internal [`StorePath`](src/preflight.rs) type has
  redacted `Display`/`Debug` and is deliberately not `Serialize`/`Deserialize`,
  so it can never leak into a report or log.
- **Credential-free by construction.** The CLI rejects every signing
  credential-shaped option before any value is inspected, and the report model
  has no field for an identity name, profile, password, team-id, or timestamp.
  A Detect observation records only **counts**, never identities.
- **`#![forbid(unsafe_code)]`** across the crate root and the binary. Process
  groups are killed via safe `rustix` (no `libc`/`unsafe`).
- **This is a spike, not the final runtime.** It is `publish = false`, carries
  no license/SPDX headers, and is not the embedded or managed `pkg` runtime. Do
  not wire it into production code paths.

## References

- [`fixtures.json`](fixtures.json) — the embedded, validated pin (the same
  canonical pin [S4](../s4-reeval-cost/README.md) measures, scoped to Darwin +
  the three attrs + the cache URL).
- [`FINDINGS.md`](FINDINGS.md) — the evidence ledger; today it records the
  harness proof, the official-source contract review, and the explicit
  environment limitation, with every result cell `Pending`.
- [`run.sh`](run.sh) — the executable wrapper (effect warnings before Detect/
  Preflight; closed grammar).
- [DR-003 in `plans/12-open-decisions-and-risks.md`](../../plans/12-open-decisions-and-risks.md)
  — the decision record this evidence feeds.
- [PR-7 in `plans/11-pr-roadmap.md`](../../plans/11-pr-roadmap.md) — spike owner.
  Downstream macOS evidence gates: **PR-28** and **PR-36** are direct S3 gates;
  **PR-26** (shared local-build engine) is gated on S5/DR-005 for its shared
  mechanism, with its Darwin *policy* informed/blocked by S3 (see `plans/12`).
- `src/report.rs`, `src/runner.rs`, `src/manifest.rs`, `src/validate.rs`,
  `src/cli.rs`, `src/detect.rs`, `src/preflight.rs`, `src/command.rs`,
  `src/main.rs` — the implementation the behavior above is derived from.

[S5]: ../../plans/11-pr-roadmap.md
