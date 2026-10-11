# Client simplification verification

## Source and system

- Base revision: `8fc06b3a0f57ecd189903c42ba5ca758c3913bbe`.
- Branch: `feat/simplify-client`.
- Host: Apple silicon, macOS 27.0.1, build 26A434.
- Rust: 1.96.1.
- Runtime: Determinate Nix 3.22.1, Nix 2.35.2.
- Release client SHA-256: `02e11dac7b1782907b7bee7a4ce238db244544fa3ecf846fcc8d836f42d823f9`.

## Source count

Count physical lines in tracked `.rs`, `.py`, `.sh`, `.nix`, and `.rb` files.
Include tests and fixtures with those extensions.
Exclude documentation, JSON, lock files, binaries, and ignored build output.
Compare the base tree with the final branch tree.
File moves contribute no reduction.

| Scope | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| All source | 88,222 | 43,135 | 45,087, or 51.11% |
| Superseded experiments | 44,736 | 0 | 44,736 |
| Current source, excluding experiments | 43,486 | 43,135 | 351, or 0.81% |
| Client, including tests | 20,931 | 20,544 | 387, or 1.85% |
| Cask generator, including tests | 11,095 | 11,129 | Increase of 34 |

Most deleted lines belong to experiments for the removed managed Nix design.
The current product reduction is smaller.
Shared version code moves from the client to the generator.
New command assertions exercise actual search output.

## Module changes

| Previous file | Previous lines | Current owners |
| --- | ---: | --- |
| `apps.rs` | 1,538 | Coordinator, storage, payload, scan, assessment, helper, destination. Largest file is 354 lines. |
| `commands/query.rs` | 1,084 | Search, source selection, result storage, info, list. |
| `commands/tap.rs` | 991 | Commands, staging, tests. Largest file is 454 lines. |
| `nix/manifest.rs` | 1,351 | Native decoding, generated index decoding, index types, index tests. |
| `nix/mod.rs` | 912 | Native command interface and runtime discovery. |
| Generator `classify.rs` | 1,272 | Classification and its tests. |
| Generator `plan.rs` | 1,118 | Build plans and their tests. |

The [module map](../architecture.md) shows the call paths and code owners.
Catalog identity, revision, numeric version, and app locking rules use existing shared implementations.
Catalog record lookup uses the validated full identity key.
Search source selection has one typed owner.
Tap updates borrow the locked registry snapshot.
Index decoding reads borrowed JSON values without cloning the whole envelope.

The client accepts only the current `pkg-cask-apps/2` private app layout.
Old private cask outputs must be rebuilt to provide that layout.
Historical findings and evidence remain under `spikes/`.
Links to deleted source use the exact base revision.

## Bounded test audit

The audit covers the declarations below and their callers.
It does not claim that the whole test suite has no redundancy.

| Declaration or test | Evidence and action |
| --- | --- |
| `catalog::invalidate_cache` | No production caller. Removed the obsolete cache cleanup API and its isolated test. Current snapshot retention remains tested. |
| `catalog::escape_regex` | Only its own test called it. Removed the custom escape API and test. Actual literal and regex command checks remain. |
| `token::classify_invalid` | Only its own test called it. Removed the list partition API and test. Token validation tests remain. |
| `tap::bare_ambiguity`, `tap::is_official` | No callers. Removed unused public helpers. Current resolution and source checks remain. |
| `ImportError::is_sandbox_refusal` | Tests were its only callers. Tests now match the typed enum directly. Refusal and text-lookalike checks remain. |
| `catalog::rows_from` | Only tests called this old search row implementation. Removed it and its isolated description test. The compiled CLI now checks shared namespace IDs and a description-only match against a full snapshot. |
| Numeric range tests | Moved with the shared version owner. Overflow, signs, empty components, zero filling, and both bounds remain tested. |
| Private app discovery test | Uses the current namespace. It checks unsafe paths and rejection of schema 1. The helper must not run during discovery. |

## Local results

All commands below passed.

| Check | Result |
| --- | --- |
| `cargo build --locked --all-targets` | Passed |
| `cargo test --locked` | Passed. 118 generator library tests, 115 client library tests, 22 CLI integration tests, plus auxiliary targets. |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed on macOS |
| `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items` | Passed |
| `cargo fetch --locked` | Passed |
| `cargo deny --locked --offline check bans licenses sources` | Passed with cargo-deny 0.20.2 |
| `cargo build --locked --release -p pkg-cli` | Passed |
| `sh -n install.sh` and `shellcheck install.sh` | Passed |
| `python3 docs/tests/test_install_script.py` | 44 tests. Seven Linux isolation tests skipped on macOS. |
| `python3 -m unittest discover -s .github/scripts/tests -p 'test_*.py' -v` | 15 tests passed |
| `python3 .github/scripts/check_docs_links.py` | Passed |
| `python3 tools/formula-catalog/tests.py` | Nine tests passed |
| `python3 tools/cask-catalog/tests/test_safe_fetch.py` | 58 tests passed |
| macOS archive and external signature checks | All plan regression checks passed with the CI-pinned Python, 7zz, and libarchive shell. |
| `python3 tools/native-cli-check.py --pkg target/release/pkg` | Passed with real Nix and an isolated home directory. |
| `git diff --check` | Passed |

The native check exercises install, duplicate install, upgrade, rollback, removal,
mutable sources, namespace choices, damaged tap isolation, cancellation in both
output modes, formula refusals, macOS bounds, helper startup, launcher sync, and
launcher removal.
The temporary home keeps the launcher fixture outside the user's app folder.

The archive check uses Nixpkgs revision `567a49d1913ce81ac6e9582e3553dd90a955875f`.
The CI jobs provide Linux Rust checks and native lifecycle checks on Linux and macOS 15.
Their results are recorded on the PR.
