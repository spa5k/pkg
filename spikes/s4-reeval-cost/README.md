# Spike S4 — Nixpkgs re-evaluation cost (PR-6 / DR-004)

> Links into the dated plan archive are legacy-plan context. They are not
> current design authority.

`pkg-spike-s4-reeval-cost` is an **evidence harness**, not production `pkg`
code. It measures **pinned-Nixpkgs re-evaluation cost** with a hardened,
deterministic, fail-closed `s4-runner` binary. It is a standalone Cargo
workspace (own `Cargo.toml`, `Cargo.lock`, `target/`) and is excluded from the
repo-root workspace lanes. Pin and sampling constants live in
[`benchmark.json`](benchmark.json); the harness embeds that file at compile time
and validates it ([`src/validate.rs`](src/validate.rs)).

> The harness and its reviewed Complete Real evidence are both preserved here.
> See [FINDINGS.md](FINDINGS.md) for the accepted measurements and budgets;
> Fake and Incomplete runs remain non-evidence.

## What it measures

Re-evaluation of one pinned Nixpkgs revision. Specifically:

- **Single-attribute** re-eval: `…#legacyPackages.<host>.ripgrep.drvPath`.
- **Index-meta** projection of each of the four systems: a bounded `--apply`
  expression over `…#legacyPackages.<system>` (embedded from
  [`nix/index-meta.nix`](nix/index-meta.nix), passed as one argv token).

It runs under a fixed `/usr/bin/time` wrapper to capture wall time and max RSS.
Each sample is a **fresh** `nix` subprocess (process-cold) over an
already-fetched flake source (source-warm).

It does **not** build, realize derivations, install, substitute from a binary
cache, or exercise any `pkg` managed-store / channel / generation behavior.
There is no `nix build`, no `--build`, no realization step — only `nix --version`,
`nix flake prefetch`, and `nix … eval`.

## Real vs Fake

The binary has two modes. They exist for completely different reasons; confusing
them invalidates any reading.

- **Fake** re-invokes the `s4-runner` binary itself as deterministic *fixture
  children* (`s4-fake-child` marker). No network, no Nix, no store. Its report is
  `mode=fake`, `completeness=fakeOnly`, `harnessOnly=true`, every sample labelled
  `fixture`, and carries **no detected Nix version**. Fake **only validates the
  harness plumbing** — capture, `/usr/bin/time` parsing, statistics, the report
  schema, atomic artifact writes, exit codes. **A Fake run is not and cannot be
  performance evidence.** Run it on every platform to confirm the harness works.
- **Real** invokes the real `nix` binary you point it at. Only a Real report can
  carry measured wall/RSS values, and only a **Complete** one (see below).

## Pin (from benchmark.json)

| Field | Value |
|---|---|
| Nix version (target, verified by probe) | `2.34.8` |
| Nixpkgs owner / repo | `NixOS` / `nixpkgs` |
| Rev | `a62e6edd6d5e1fa0329b8653c801147986f8d446` |
| Flake `narHash` | `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=` |
| Attribute | `ripgrep` |
| Systems | `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin` |

The flake `narHash` is the **verified trust input** and is what `nix flake
prefetch` output is checked against. It **intentionally differs** from
`rawArchive.sha256Sri` (`sha256-rXVGuq8bJfByJbOrrB3I++2MTsvZDcTo7C6UHXD5muE=`),
which is the hash of the *raw GitHub archive tarball*. They are different
domains: the flake NAR hash covers **canonical NAR content**, not the GitHub
archive bytes. Both are pinned in `benchmark.json` so they cannot be conflated.

Sampling, caps, and timeouts (validated ranges enforced in `validate.rs`):

- Sampling: single-attr `1 warmup + 5 measured`; each of the 4 index systems
  `1 + 3`.
- Caps (bounded child output before a bounded-overflow failure): single-attr
  stdout `1 048 576` B (1 MiB), index stdout `268 435 456` B (256 MiB), shared
  stderr `8 388 608` B (8 MiB).
- Timeouts: per single-attr command `300 s`, per index command `600 s`, overall
  run `3600 s`.

## Prerequisites

The harness **installs and configures nothing**. All of the following must
already be true on the host. macOS and Linux (the four systems above) are the
only supported platforms; the host target is fixed at compile time.

- **Rust exactly `1.96.1`.** The repo pins `channel = "1.96.1"` in the root
  [`rust-toolchain.toml`](../../rust-toolchain.toml); the spike declares
  `rust-version = "1.96"`, edition 2024. That toolchain is discovered upward
  from this directory, so `cargo` here uses 1.96.1.
- **Cached Cargo dependencies** for offline commands. The spike depends only on
  `serde`, `serde_json`, and `rustix` (locked in its own `Cargo.lock`).
  `cargo build --locked --offline` works once those crates are in the local
  registry cache; no network is needed to build.
- **`/usr/bin/time`.** The executor hard-codes `/usr/bin/time` (GNU `-v` on
  Linux, BSD `-l` on macOS) as the only child it spawns directly.
- **For Real only — an absolute `nix` binary at version `2.34.8`.** Pass it via
  `--nix-bin`. The version probe runs `nix --version` and requires the parsed
  version to equal the pin *exactly* (`2.34.8`); any mismatch fails the run.
- **For Real only — a working, already-configured Nix store/daemon/config** that
  the pointed-at `nix` uses, plus **network for exactly one** `nix flake
  prefetch`. The harness creates a private `HOME`/`XDG_CACHE_HOME`/
  `XDG_CONFIG_HOME` but **shares the configured Nix store** — it never clears
  the store or evaluator caches.
- **For an isolated Real run — an existing private store root.** It must be an
  absolute, real directory owned by the caller at exact mode `0700` (no
  symlink). Pass `--store-root`; the runner adds only `NIX_REMOTE=<root>` to the
  otherwise cleared child environment. Nix treats the absolute path as a local chroot
  store whose logical store directory remains `/nix/store`. This lets the
  native macOS lane avoid the service-only managed daemon socket.

## Running

A shipped executable wrapper, [`run.sh`](run.sh), is the recommended path. It
selects an already-installed rustup 1.96.1 toolchain when available; otherwise
`rustc` on `PATH` must be exactly 1.96.1. It never installs or downloads a
toolchain. It then runs the binary with `cargo run --locked --offline
--release`. It defaults `--out-dir` to `target/s4-fake` (Fake) /
`target/s4-real` (Real), **preserves the runner's exit status**, and echoes
`summary.md` under a fixed `--- summary.md ---` header if one was produced.

```sh
# Fake (no network, no Nix) — validates harness plumbing only:
./run.sh fake                       # out -> target/s4-fake
./run.sh fake custom/out

# Real (requires the prerequisites above):
./run.sh real /opt/nix/bin/nix      # out -> target/s4-real
./run.sh real /opt/nix/bin/nix custom/out
./run.sh real /opt/nix/bin/nix custom/out /absolute/local-store-root
```

You can also run the binary directly. Build it first (`--locked --offline`, once
deps are cached), then invoke it; `--out-dir` then defaults to `out`:

```sh
# Build (locked + offline, once deps are cached):
cargo build --locked --offline --release

# Fake (no network, no Nix) — validates harness plumbing only:
./target/release/s4-runner fake
./target/release/s4-runner fake --out-dir ./out

# Real (requires the prerequisites above):
./target/release/s4-runner real --nix-bin /opt/nix/bin/nix
./target/release/s4-runner real --nix-bin /opt/nix/bin/nix --out-dir ./out
./target/release/s4-runner real --nix-bin /opt/nix/bin/nix \
  --store-root /absolute/local-store-root --out-dir ./out

# Usage banner:
./target/release/s4-runner --help
```

During development the same thing via Cargo:

```sh
cargo run --locked --offline --release -- fake
cargo run --locked --offline --release -- real --nix-bin /opt/nix/bin/nix
cargo test --locked --offline     # unit + the black-box Fake e2e tests (no Nix, no network)
```

The grammar (closed parser, no `--flag=value`, no abbreviations):

```text
s4-runner fake [--out-dir PATH]
s4-runner real --nix-bin ABSOLUTE_PATH [--store-root ABSOLUTE_PATH] [--out-dir PATH]
s4-runner --help | -h
```

## Outputs and exit codes

**After a report is produced**, the harness writes two artifacts under
`--out-dir` (default `out`) via an atomic sibling-temp rename, so a crash never
leaves a partial file:

- `report.json` — the validated, machine-readable report (pretty JSON, one
  trailing newline). See `src/report.rs` for the schema.
- `summary.md` — the deterministic human-readable rendering of the same report.

This holds for a successful **Fake** run, a **Complete** Real run, and an
**Incomplete** Real run (the diagnostic record). It does **not** hold for
`EX_USAGE` (`64`) / `EX_SOFTWARE` (`70`) or any CLI-parse, internal,
pre-report, or artifact-write failure: those produce no report, write no files,
and any pre-existing contents of `--out-dir` may remain.

Exit codes (from `src/main.rs`):

| Code | Meaning |
|---|---|
| `0` | Success: `--help`, a Fake run, or a **Complete** Real run. Artifacts written. |
| `64` | `EX_USAGE`: CLI parse error (bad mode, missing/relative `--nix-bin`, etc.). No artifacts. |
| `69` | `EX_UNAVAILABLE`: Real run returned a validated **Incomplete** report. **Artifacts are still written** (the diagnostic record); no dynamic Nix output is printed. |
| `70` | `EX_SOFTWARE`: internal/software failure (could not resolve/build/run, private-home failure, or artifact write failure). For Real, pre-existing output-dir contents may remain. |

For normal CLI and status diagnostics, each stderr line is a single, fixed,
caller-data-free label (paths and child output are never echoed); child stdout
/ stderr is never echoed. (The hidden `s4-fake-child` fixture children that
**Fake** mode spawns are an intentional exception: they emit deterministic
fixture stderr to exercise the capture path.)

## Real semantics

A Real run is, in order:

1. **Version probe** (`nix --version`) — parsed version must equal `2.34.8`.
2. **One online, verified flake prefetch** (`nix … flake prefetch --json`) —
   fetches the flake tarball into the configured Nix store and verifies the
   reported NAR hash equals the pinned `narHash` and the `storePath` is a valid
   `/nix/store/<basename>`. This is the **only** network operation.
3. **Re-evaluation samples** — every eval command runs with `--offline` (pure
   local evaluation of the already-fetched pinned flake). One fresh `nix`
   process per iteration: **source-warm / process-cold** (`sourceWarmProcessCold`
   label). No builds, no realization.

Iteration plan (from `benchmark.json`): the host single-attr scenario runs
`1 warmup + 5 measured`; each of the four index systems runs `1 + 3`. Per-command
budget is `300 s` (single-attr) / `600 s` (index), overall `3600 s`. Child output
is bounded by the caps above. All wall/RSS figures are integer milliseconds /
KiB (no floats), so the report is byte-deterministic for a given input.

## Security and caveats

- **Fixed wrapper, exact path.** The only direct child is `/usr/bin/time`; the
  `nix` path you pass is handed to the OS verbatim as an argument. **No shell,
  no `PATH` lookup, no `NIX_PATH`** anywhere. `--nix-bin` must be absolute.
- **Minimal, fail-closed child environment.** `Command::env_clear()` is applied,
  then exactly: `LANG=C`, `LC_ALL=C`, and (Real) a private `HOME`,
  `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`; an explicit isolated-store run adds only
  `NIX_REMOTE=<absolute-store-root>`. Without `--store-root`, the configured Nix
  store is shared.
- **Bounded, fixed diagnostics.** Every error message is a fixed ASCII label
  with no caller-controlled text (paths and child output are never echoed); I/O
  failures are reduced to a stable `io::ErrorKind` token.
- **`#![forbid(unsafe_code)]`** across the crate root and the binary.
- **This is a spike, not the final runtime.** It is `publish = false`, carries
  no license/SPDX headers, and is not the embedded or managed `pkg` runtime. Do
  not wire it into production code paths.

## Complete vs Incomplete

A **Real** report is one of:

- **Complete** — the run finished cleanly: detected Nix version equals the pin,
  no recorded failures, a non-empty scenario set, every sample complete
  (non-skipped, exit 0, wall + RSS + output present), contiguous in-order
  indices, warmup-before-measured, and wall/RSS statistics that recompute
  exactly from the measured samples. Exit `0`. This is the only state that is
  evidence.
- **Incomplete** — the run did not finish cleanly (Nix missing or wrong
  version, a prefetch failure, an eval failure, or a per-scenario/overall
  timeout). It contains **only honestly captured samples** (a partial prefix may
  be preserved) and **must carry at least one recorded failure**. Exit `69`;
  both artifacts are still written as the diagnostic record.

**An Incomplete report is not performance evidence.** Treat its presence and
exit `69` as "the run did not complete — read the recorded failures; do not read
the captured samples as a measurement of Nixpkgs re-eval cost." An empty data set
cannot masquerade as unexplained incompleteness, because Incomplete requires a
recorded failure. A **Fake** report is `fakeOnly` and, likewise, is never
performance evidence.

## References

- [`benchmark.json`](benchmark.json) — the embedded, validated pin and sampling
  constants.
- [`FINDINGS.md`](FINDINGS.md) — accepted Complete Real evidence and the budgets
  derived from it.
- [`plans/archive/2026-08-22-custom-managed-nix-v1/11-pr-roadmap.md`](../../plans/archive/2026-08-22-custom-managed-nix-v1/11-pr-roadmap.md), DR-004 in
  [`plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md`](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md)
  — spike context and decision record.
- `src/report.rs`, `src/manifest.rs`, `src/validate.rs`, `src/real.rs`,
  `src/runner.rs`, `src/command.rs`, `src/execute.rs` — the implementation the
  behavior above is derived from.
