// Spike S4 — single-attribute + index-meta reevaluation cost (PR-6 / DR-004).
//
// STANDALONE Cargo workspace (`[workspace]` in `Cargo.toml`), deliberately NOT
// part of the production workspace at the repo root. `publish = false`, NO
// `license` field, NO SPDX headers (DR-015). It is a benchmark/evidence harness,
// not production pkg code. Unsafe code is forbidden everywhere in this crate.
//
// What this library owns:
//   * an immutable, strictly-validated benchmark manifest (pinned Nix 2.34.8 +
//     pinned Nixpkgs rev/narHash + raw-archive evidence);
//   * pure-flake command construction with NO shell-eval / NO `--impure` /
//     NO `NIX_PATH` / NO mutable channel / NO build / NO cache-clearing;
//   * warmup-vs-measured sampling with min/median/p95/max wall-ms stats;
//   * maximum-RSS capture via `/usr/bin/time` with correct macOS/BSD vs GNU
//     parsing;
//   * bounded (capped) child-output capture that fails clearly on overflow;
//   * deterministic JSON + readable Markdown reporting; and
//   * a Fake mode that exercises the EXACT pipeline with deterministic fixture
//     children (no network, no Nix), and a Real mode that fails CLOSED with a
//     machine-readable incomplete result + nonzero exit when Nix is missing or
//     the wrong version.
//
// It never fabricates Real results and never claims store-cold numbers.

#![forbid(unsafe_code)]

pub mod base64sri;
pub mod caps;
pub mod cli;
pub mod command;
// INTERNAL finite-time executor (`execute::run`). Declared crate-private
// (`mod`, not `pub mod`) so the in-crate Fake and Real orchestration can each
// drive the `/usr/bin/time` spawn→drain→reap→RSS pipeline, but no external
// crate can reach it. `execute::run` is used by both Fake and Real
// orchestration; the public execution surfaces are [`runner::run_fake`] and
// [`real::run_real`].
mod execute;
pub mod fake;
pub mod flakeref;
pub mod manifest;
pub mod real;
pub mod report;
pub mod runner;
pub mod stats;
pub mod timeparse;
pub mod validate;

pub use manifest::{Manifest, benchmark_manifest};
