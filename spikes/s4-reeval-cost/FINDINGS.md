# S4 / PR-6 / DR-004 — Evidence Ledger

This is an **evidence ledger**, not marketing. It records the reviewed Complete
Real measurements, their limits, and the budgets derived from them.

- **Spike:** S4 — Nixpkgs re-evaluation cost. Harness: [`pkg-spike-s4-reeval-cost`](README.md).
- **PR:** [PR-6 — SPIKE S4: single-attribute reevaluation cost](../../plans/11-pr-roadmap.md)
  (M0.5). Owns this directory and DR-004.
- **Decision:** [DR-004 — Resolve UX & index strategy gated on reevaluation cost](../../plans/12-open-decisions-and-risks.md)
  — **Status: Accepted 2026-08-09**.
- **Downstream gates:** [PR-14](../../plans/11-pr-roadmap.md) (disposable index),
  [PR-16](../../plans/11-pr-roadmap.md) (resolver), [PR-32](../../plans/11-pr-roadmap.md)
  (perf bench + budget gate) — the S4 gate is cleared; each PR retains its own
  implementation and validation gates.
- **Recorded (host UTC):** 2026-08-09. Native host lanes: macOS
  `aarch64-darwin` and Docker Desktop Linux VM `aarch64-linux`, both on the same
  10-core Apple Silicon machine.

---

## 1. Harness evidence (what the spike proves today)

Full suite run with the pinned toolchain (rustc `1.96.1`, the channel in the
repo-root `rust-toolchain.toml`; the host's active default rustc `1.95.0` is
rejected by the crate's `rust-version = "1.96"`):

| Binary | passed | failed |
|---|---|---|
| `src/lib.rs` unit tests | 449 | 0 |
| `src/main.rs` unit tests | 7 | 0 |
| `tests/fake_e2e.rs` (black-box) | 5 | 0 |
| doc-tests | 0 | 0 |
| **Total** | **461** | **0** |

Command: `cargo +1.96.1 test` (run from this directory).

Two black-box tests are load-bearing for the ledger:

- `fake_run_writes_validated_fixture_only_report_and_summary` — exercises the
  **Fake** lane end-to-end and confirms it writes a validated report that is
  `mode=fake`, `completeness=fakeOnly`, `harnessOnly=true`, with every sample
  labelled `fixture` and **no detected Nix version**. This validates the harness
  plumbing only: capture, `/usr/bin/time` parsing, statistics, report schema,
  atomic artifact writes, exit codes.
- `real_mode_missing_nix_is_diagnostic_incomplete_exit_69` — drives the **Real**
  lane with no `nix` available and confirms the harness returns a validated
  **Incomplete** report (exit `69`) and still writes the `report.json` /
  `summary.md` diagnostic artifacts carrying a recorded failure.

> **Neither of these is performance evidence.** A Fake run validates harness
> plumbing; an Incomplete Real run validates fail-closed behavior. No wall/RSS
> values from either may be read as a measurement of Nixpkgs re-eval cost. See
> [README.md § Real vs Fake](README.md) and
> [README.md § Complete vs Incomplete](README.md).

---

## 2. Source-integrity finding (independently re-verified this session)

The pin lives in [`benchmark.json`](benchmark.json). I fetched the raw GitHub
archive over HTTPS (no Nix required) and re-derived size + sha256:

| Artifact | Pinned (benchmark.json) | Re-verified this session | Match |
|---|---|---|---|
| Raw archive byte size | `38,667,882` | `38,667,882` | ✅ |
| Raw archive sha256 (hex) | `ad7546baaf1b25f07225b3abac1dc8fbed8c4ecbd90dc4e8ec2e941d70f99ae1` | identical | ✅ |
| Raw archive sha256 (SRI) | `sha256-rXVGuq8bJfByJbOrrB3I++2MTsvZDcTo7C6UHXD5muE=` | identical | ✅ |
| Flake `narHash` | `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=` | — | see below |

The raw-archive SRI **intentionally differs** from the pinned flake `narHash`.
These are distinct hash domains: the raw SRI covers the **GitHub tarball bytes**,
while the flake `narHash` covers **canonical NAR content** after Nix imports the
source. Equality is not expected and would be suspicious. This is exactly why
both are pinned separately in [`benchmark.json`](benchmark.json).

The harness's Real path verifies the `narHash` itself: the one online
`nix flake prefetch --json` checks the reported NAR hash against the pinned
`narHash` and the `storePath` against `/nix/store/<basename>`
([README.md § Real semantics](README.md)). Both Complete Real lanes re-ran this
verification successfully before sampling.

---

## 3. Environment and isolation

Two Complete Real runs were produced with exact Nix `2.34.8`:

- **macOS / `aarch64-darwin`:** native binary at
  `/nix/var/nix/profiles/default/bin/nix`, with the new explicit
  `--store-root /private/tmp/pkg-s4-macos-store`. The runner passes this only as
  `NIX_REMOTE`, selecting a user-owned local chroot store and never connecting
  to the product's service-only daemon socket.
- **Linux / `aarch64-linux`:** native Docker Desktop Linux VM using
  `nixos/nix:2.34.8`; the harness binary was built independently with Rust
  `1.96.1` on `rust:1.96-alpine`. The container's disposable `/nix` store was
  used directly.

Both lanes were source-warm/process-cold: the verified prefetch populated the
source once, then every measured eval used a fresh `nix` subprocess with
`--offline`. Neither lane built or realized package outputs. The preserved
artifacts are under [`evidence/`](evidence/).

The Linux evidence is native arm64, not the originally suggested native
x86_64 reference runner. It is accepted for the architecture decision because
the decision is based on two native OS lanes and deliberately wide budgets;
native x86_64 remains a PR-32 baseline-expansion task before GA, not a reason to
invent or use QEMU timings here.

---

## 4. Pin, sampling, caps, timeouts, and the run command

Exact pin ([`benchmark.json`](benchmark.json)):

| Field | Value |
|---|---|
| Nix version (target; probe requires exact equality) | `2.34.8` |
| Nixpkgs owner / repo | `NixOS` / `nixpkgs` |
| Rev | `a62e6edd6d5e1fa0329b8653c801147986f8d446` |
| Flake `narHash` | `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=` |
| Attribute | `ripgrep` |
| Systems | `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin` |

Harness sampling / caps / timeouts (validated ranges enforced in `src/validate.rs`):

| Item | Value |
|---|---|
| Warmup per scenario | `1` |
| Single-attr measured iterations | `5` |
| Index-meta measured iterations (per system) | `3` |
| Single-attr stdout cap | `1,048,576` B (1 MiB) |
| Index stdout cap | `268,435,456` B (256 MiB) |
| Shared stderr cap | `8,388,608` B (8 MiB) |
| Per-command timeout — single-attr | `300 s` |
| Per-command timeout — index | `600 s` |
| Overall run timeout | `3600 s` |

Sampling shape: each sample is a **fresh** `nix` subprocess (process-cold) over
an already-fetched flake source (source-warm); labelled
`sourceWarmProcessCold`. All wall/RSS figures are integer ms / KiB (no floats),
so a Complete report is byte-deterministic for a given input.

**Run command to use later (Complete Real), after the prerequisites in
[README.md § Prerequisites](README.md) are met** (Nix `2.34.8` at an absolute
path, configured store, network for exactly one prefetch):

```sh
# From this directory (spikes/s4-reeval-cost), rustc 1.96.1 on PATH:
cargo build --locked --offline --release
./target/release/s4-runner real --nix-bin /opt/nix/bin/nix
#   exit 0  -> Complete (the only state that is evidence)
#   exit 69 -> Incomplete (diagnostic only; NOT evidence)
# equivalent wrapper (requires rustc 1.96.1 already on PATH; builds
# locked+offline; defaults --out-dir to target/s4-real; echoes summary.md;
# preserves the runner's exit status):
./run.sh real /opt/nix/bin/nix
```

Artifacts are written under `--out-dir` (the binary defaults to `out`; the
`run.sh` wrapper defaults to `target/s4-real`): `report.json` and `summary.md`,
via an atomic sibling-temp rename.

---

## 5. Accepted measurement table

Five canonical scenarios were captured per lane: one host-only single-attr
scenario (`1 + 5`) and one index-meta scenario per system (`1 + 3`). The
single-attr scenario evaluates the host system; index-meta evaluates the named
system regardless of host.

| Host lane | Scenario | System (eval target) | Measured | median wall | p95 wall | max wall | max RSS |
|---|---|---|---|---|---|---|---|
| macOS arm64 | `single-attr:ripgrep` | `aarch64-darwin` | 5 | 438 | 453 | 453 | 224,800 |
| macOS arm64 | `index-meta:x86_64-linux` | `x86_64-linux` | 3 | 3,789 | 3,804 | 3,804 | 1,442,032 |
| macOS arm64 | `index-meta:aarch64-linux` | `aarch64-linux` | 3 | 3,672 | 3,678 | 3,678 | 1,385,968 |
| macOS arm64 | `index-meta:x86_64-darwin` | `x86_64-darwin` | 3 | 4,686 | 4,696 | 4,696 | 1,879,328 |
| macOS arm64 | `index-meta:aarch64-darwin` | `aarch64-darwin` | 3 | 4,443 | 4,444 | 4,444 | 1,687,648 |
| Linux arm64 | `single-attr:ripgrep` | `aarch64-linux` | 5 | 265 | 325 | 325 | 138,512 |
| Linux arm64 | `index-meta:x86_64-linux` | `x86_64-linux` | 3 | 3,691 | 3,717 | 3,717 | 1,404,556 |
| Linux arm64 | `index-meta:aarch64-linux` | `aarch64-linux` | 3 | 3,594 | 3,623 | 3,623 | 1,349,316 |
| Linux arm64 | `index-meta:x86_64-darwin` | `x86_64-darwin` | 3 | 4,678 | 4,700 | 4,700 | 1,853,420 |
| Linux arm64 | `index-meta:aarch64-darwin` | `aarch64-darwin` | 3 | 4,367 | 4,379 | 4,379 | 1,651,672 |

Wall units: integer milliseconds (ms). RSS units: integer KiB. Both as emitted by
the validated report; do not transcribe floats or invent precision.

---

## 6. Evidence acceptance checklist

Before any number from a Real run may be cited for DR-004 / PR-14 / PR-16 /
PR-32, the produced report must satisfy all of the following. Treat this as a
gate, not a rubber stamp.

- [x] Harness internal validation passes and the run exits `0` (Complete), not
      `69` (Incomplete) and not `64`/`70`.
- [x] `report.mode` is `real` (not `fake`).
- [x] `report.completeness` is `complete` (not `incomplete`/`fakeOnly`).
- [x] `report.harnessOnly` is `false`.
- [x] **Exact** environment recorded: detected Nix version `2.34.8` equals the
      pin; the rev, `narHash`, attribute, and **host system** all match
      [`benchmark.json`](benchmark.json).
- [x] All **five** canonical scenarios are present with the declared measured
      counts (`single-attr:ripgrep` = 5; each `index-meta:<system>` = 3) and the
      `1` warmup each.
- [x] No recorded failures; no skipped samples; samples contiguous and in order
      (warmup before measured).
- [x] Wall/RSS statistics **recompute exactly** from the measured samples (the
      harness already enforces this for Complete reports; re-check on review).
- [x] Attachments preserved with the decision: `report.json`, `summary.md`, and
      host context (OS/version, arch, Nix version/path, CPU/RAM, whether the
      store/evaluator cache was warm).
- [ ] Repeated native runs on fixed PR-32 reference hosts remain desirable for
      baseline stability; this is not required to choose the v1 architecture.

---

## 7. Decision

**DR-004 is accepted.** The data supports the existing architecture:

- Search/list/info use a disposable precomputed index. A full per-system
  meta-eval costs roughly 3.6–4.7 seconds p95 and up to 1.88 GiB RSS here, so it
  is inappropriate on every interactive query.
- Install re-evaluates the exact pinned attribute. The observed p95 was 325 ms
  on Linux arm64 and 453 ms on macOS arm64, leaving ample room for a visible
  progress step and a conservative absolute budget.
- Publisher-side precompute remains preferred. Client self-build is recovery,
  not the permanent source of truth.

Frozen PR-32 absolute budgets on its fixed Real reference hosts:

| Operation | Absolute p95 budget | Peak RSS budget | Evidence headroom |
|---|---:|---:|---|
| single pinned-attr resolve | `< 1.5 s` | `< 512 MiB` | 3.3× time, 2.3× memory vs worst observed |
| one-system full index meta-eval | `< 10 s` | `< 2.5 GiB` | 2.1× time, 1.3× memory vs worst observed |
| four-system sequential publisher meta-eval | `< 30 s` | `< 2.5 GiB` peak | 1.7× time vs worst observed summed p95 |

The regression gate is **25% over the pinned reference baseline**, while the
absolute ceilings above remain hard backstops. Crossing either fails the perf
lane. PR-14, PR-16, and PR-32 are now ungated by S4, but retain their own test
and review requirements.

---

## References

- [README.md](README.md) — spike harness documentation (Real vs Fake, Complete vs
  Incomplete, prerequisites, semantics, exit codes).
- [`benchmark.json`](benchmark.json) — embedded, validated pin and
  sampling/caps/timeouts constants.
- [DR-004 in `plans/12-open-decisions-and-risks.md`](../../plans/12-open-decisions-and-risks.md)
  — the decision record this evidence feeds.
- [PR-6 in `plans/11-pr-roadmap.md`](../../plans/11-pr-roadmap.md) — spike owner
  and downstream gates (PR-14 / PR-16 / PR-32).
