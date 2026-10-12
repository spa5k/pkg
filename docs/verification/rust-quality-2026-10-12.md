# Rust quality checks, 2026-10-12

The change starts at `e9ddf568f7890d1e49408d88ef5e88c9dabfe4ad`.
It builds on [PR #113](https://github.com/spa5k/pkg/pull/113).
The change is [PR #114](https://github.com/spa5k/pkg/pull/114).
The tested source commit is `5028885e242d2c765581212d83ba7538dd8d139d`.
The task IDs are RQ-01 through RQ-06 in
[Improve Rust error handling and ownership](../../openspec/changes/archive/2026-10-12-improve-rust-quality/tasks.md).

## Changes

Native process errors retain their I/O type.
JSON, TOML, and cache errors retain their causes through `Error::source`.
Commands retain native, catalog, and configuration errors until reporting.
Cask install, info, and upgrade gates preserve cancellation.
A cancelled runtime version probe also preserves its signal status.

Metadata rows use a tagged enum.
The decoder rejects unknown tags and missing package or child payloads.
Saved catalogs and query views share immutable indexes with `Rc`.
Tap cache mutation uses a mutable borrow and the map entry API.
The native profile decoder borrows its JSON input instead of cloning it.
Redundant path wrappers and impossible lazy-load errors are removed.

Snapshot staging uses a unique temporary file with cleanup on drop.
Old temporary files and symlinks cannot block or redirect publication.
The generator sets catalog permissions and syncs data before replacement.
Pin writes also sync staged data before replacement.

## Local results

The host used macOS 27.0.1 on Apple silicon.
The toolchain was Rust 1.96.1.
The native runtime was Determinate Nix 3.22.1 with Nix 2.35.2.

| Check | Result |
| --- | --- |
| `cargo build --locked --all-targets` | Passed |
| `cargo test --locked` | Passed. Generator library 118, client library 117, CLI 22, other targets 3 |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Rust docs with `-D warnings`, including private items | Passed |
| `cargo deny --locked --offline check bans licenses sources` | Passed. Existing duplicate dependency warnings remain |
| Shell syntax and ShellCheck | Passed |
| Installer tests | Passed. 44 tests, 7 skipped |
| GitHub script tests | Passed. 15 tests |
| Local documentation links | Passed |
| Formula catalog tests | Passed. 9 tests |
| Safe fetch tests | Passed. 58 tests |
| Plan tests with pinned Nix Python, 7zz, and libarchive | Passed |
| Release build and native CLI checks | Passed |

The installer skips require Linux mount namespace isolation.
This macOS host has no `unshare` command and has an existing Nix installation.
The release CLI check uses temporary HOME and XDG directories.
It checks native mutations, rollback, saved tap isolation, cancellation,
Formula aliases and refusals, original cask source upgrades, and macOS app helpers.

The cask CLI regression checks both bare and qualified IDs for install and info.
Each cancelled evaluation returns 130 and keeps the profile link unchanged.
Runtime tests check the I/O cause after an executable is removed.
They also check failed and cancelled configured version probes.
The cache test checks the I/O cause and temporary cleanup after failed publication.
