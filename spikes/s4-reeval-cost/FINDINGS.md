# S4 / PR-6 / DR-004 — Evidence Ledger

This is an **evidence ledger**, not a result and not marketing. It records what
was actually verified in this environment, what was *not* (and why), and what
must still be measured. **No performance numbers appear here.** Any cell that
would hold a measurement is `Pending` until a reviewed Complete Real run exists.

- **Spike:** S4 — Nixpkgs re-evaluation cost. Harness: [`pkg-spike-s4-reeval-cost`](README.md).
- **PR:** [PR-6 — SPIKE S4: single-attribute reevaluation cost](../../plans/11-pr-roadmap.md)
  (M0.5). Owns this directory and DR-004.
- **Decision:** [DR-004 — Resolve UX & index strategy gated on reevaluation cost](../../plans/12-open-decisions-and-risks.md)
  — **Status: Proposed** (pending S4 / PR-6).
- **Downstream gates:** [PR-14](../../plans/11-pr-roadmap.md) (disposable index),
  [PR-16](../../plans/11-pr-roadmap.md) (resolver), [PR-32](../../plans/11-pr-roadmap.md)
  (perf bench + budget gate) — all remain **gated** on S4 numbers.
- **Recorded (host UTC):** 2026-08-06. **Host lane of this session:** macOS,
  Darwin 26.6, `aarch64` (`aarch64-darwin`).

---

## 1. Harness evidence (what the spike proves today)

Full suite run with the pinned toolchain (rustc `1.96.1`, the channel in the
repo-root `rust-toolchain.toml`; the host's active default rustc `1.95.0` is
rejected by the crate's `rust-version = "1.96"`):

| Binary | passed | failed |
|---|---|---|
| `src/lib.rs` unit tests | 444 | 0 |
| `src/main.rs` unit tests | 7 | 0 |
| `tests/fake_e2e.rs` (black-box) | 5 | 0 |
| doc-tests | 0 | 0 |
| **Total** | **456** | **0** |

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
([README.md § Real semantics](README.md)). **This prefetch verification was not
re-run in this session** because no usable Nix is available (see §3); the
`narHash` row above is therefore recorded as pinned-but-not-independently-verified
here, not as measured.

---

## 3. Environment limitation (what was NOT produced)

There is **no usable Nix `2.34.8` installation** in this implementation
environment — `nix` is absent from `PATH` and from the common install paths, and
no daemon/store is configured. Consequently this ledger records **no Complete
Real run, no timing samples, no RSS samples, and no budgets.**

Because no Complete Real evidence exists:

- **DR-004 remains `Proposed`.**
- **PR-14, PR-16, PR-32 remain gated** (they consume S4 budgets).
- **No architecture or performance-budget decision may be accepted from the
  current evidence.**
- **No numbers are invented.** Every result cell below is `Pending`.

This is a deliberate non-result, not a gap to paper over. The roadmap is
explicit: *"No perf budgets set before S4 (PR-6). Otherwise budgets are fiction
and the perf gate (PR-32) misfires."*
([plans/11-pr-roadmap.md § risks](../../plans/11-pr-roadmap.md)).

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

## 5. Pending measurement table

Five canonical scenarios, as built by `src/runner.rs` `descriptors()`: one
**host-only** single-attr scenario (`single-attr:ripgrep`, `1 + 5`) plus one
**index-meta** scenario per system (`index-meta:<system>`, `1 + 3`), in manifest
order. **Every result cell is `Pending`.** Minimum coverage is two host lanes —
**macOS** (`aarch64-darwin`) and **Linux** (`x86_64-linux`); additional host
lanes (`x86_64-darwin`, `aarch64-linux`) are welcome. Note: the single-attr
scenario evaluates the **host** system of the lane; the index-meta scenarios are
pure eval over the pinned flake for the named system regardless of host.

| Host lane | Scenario | System (eval target) | Measured | median wall | p95 wall | max wall | max RSS |
|---|---|---|---|---|---|---|---|
| macOS | `single-attr:ripgrep` | `aarch64-darwin` | 5 | Pending | Pending | Pending | Pending |
| macOS | `index-meta:x86_64-linux` | `x86_64-linux` | 3 | Pending | Pending | Pending | Pending |
| macOS | `index-meta:aarch64-linux` | `aarch64-linux` | 3 | Pending | Pending | Pending | Pending |
| macOS | `index-meta:x86_64-darwin` | `x86_64-darwin` | 3 | Pending | Pending | Pending | Pending |
| macOS | `index-meta:aarch64-darwin` | `aarch64-darwin` | 3 | Pending | Pending | Pending | Pending |
| Linux | `single-attr:ripgrep` | `x86_64-linux` | 5 | Pending | Pending | Pending | Pending |
| Linux | `index-meta:x86_64-linux` | `x86_64-linux` | 3 | Pending | Pending | Pending | Pending |
| Linux | `index-meta:aarch64-linux` | `aarch64-linux` | 3 | Pending | Pending | Pending | Pending |
| Linux | `index-meta:x86_64-darwin` | `x86_64-darwin` | 3 | Pending | Pending | Pending | Pending |
| Linux | `index-meta:aarch64-darwin` | `aarch64-darwin` | 3 | Pending | Pending | Pending | Pending |

Wall units: integer milliseconds (ms). RSS units: integer KiB. Both as emitted by
the validated report; do not transcribe floats or invent precision.

---

## 6. Evidence acceptance checklist

Before any number from a Real run may be cited for DR-004 / PR-14 / PR-16 /
PR-32, the produced report must satisfy all of the following. Treat this as a
gate, not a rubber stamp.

- [ ] Harness internal validation passes and the run exits `0` (Complete), not
      `69` (Incomplete) and not `64`/`70`.
- [ ] `report.mode` is `real` (not `fake`).
- [ ] `report.completeness` is `complete` (not `incomplete`/`fakeOnly`).
- [ ] `report.harnessOnly` is `false`.
- [ ] **Exact** environment recorded: detected Nix version `2.34.8` equals the
      pin; the rev, `narHash`, attribute, and **host system** all match
      [`benchmark.json`](benchmark.json).
- [ ] All **five** canonical scenarios are present with the declared measured
      counts (`single-attr:ripgrep` = 5; each `index-meta:<system>` = 3) and the
      `1` warmup each.
- [ ] No recorded failures; no skipped samples; samples contiguous and in order
      (warmup before measured).
- [ ] Wall/RSS statistics **recompute exactly** from the measured samples (the
      harness already enforces this for Complete reports; re-check on review).
- [ ] Attachments preserved with the decision: `report.json`, `summary.md`, and
      host context (OS/version, arch, Nix version/path, CPU/RAM, whether the
      store/evaluator cache was warm).
- [ ] Ideally **repeated runs** on macOS **and** Linux; record stability across
      runs (not a single sample).

---

## 7. Decision

**No architecture or performance-budget decision may be accepted from the current
evidence.** What exists today is harness-correctness and source-integrity
evidence only — there are zero Complete Real samples and zero budgets, by
environment limitation, not by choice.

**Next action:** run the Complete Real lanes (macOS and Linux at minimum) using
the command in §4, fill the table in §5 from reviewed `report.json` outputs,
satisfy the checklist in §6, and **then** update this file and
[DR-004](../../plans/12-open-decisions-and-risks.md) with reviewed numbers. Until
that happens, [DR-004](../../plans/12-open-decisions-and-risks.md) stays
`Proposed` and [PR-14](../../plans/11-pr-roadmap.md) /
[PR-16](../../plans/11-pr-roadmap.md) / [PR-32](../../plans/11-pr-roadmap.md)
stay gated.

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
