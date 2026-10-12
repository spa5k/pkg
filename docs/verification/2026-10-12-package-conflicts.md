# Package conflict verification — 2026-10-12

Change: [Explain package file conflicts](../../openspec/changes/archive/2026-10-12-improve-package-conflicts/proposal.md).
Base revision: `ddf6f66b3a9d3fbd61f1534f77e6a2d8b18d42b5`.
Client candidate: `0.2.0-alpha.14`.
Host: Apple silicon macOS.
Runtime: Determinate Nix 3.22.1 / Nix 2.35.2.

## Real conflict

A separate profile symlink started at the user's existing profile output.
The candidate ran `pkg --no-color install brew:homebrew/core/bat` with a
temporary XDG state and cache directory. Nix rejected the duplicate file.
The command exited with status 1 and empty stdout. Stderr was:

```text
Installing: brew:homebrew/core/bat
Failed: install did not complete.
Installed entries unchanged.
Cause: nixpkgs:bat (Nixpkgs) is already installed and provides bin/bat.
Requested: brew:homebrew/core/bat (Homebrew Formula).
```

No store path, replacement command, or priority command appeared.
Both profile manifests retained SHA-256
`b137161f201cd4e42027fb6011aed7f14f5da7b6baf8083507b79c365d504782`.

## Regression coverage

The CLI test replays the colored native diagnostic. It checks a single owner,
multiple active owners, a failed profile re-read, inactive entries, and a
store path with a similar string prefix. Unknown owners are not named.
Warnings and trust prompts remain visible. Verbose output retains native
paths and priority instructions. An identical installed Nixpkgs output exits
successfully with `Already installed: nixpkgs:bat` and no profile add.

The adapter test rejects mismatched relative files, paths outside the store,
and parent traversal. Existing native cause and cancellation tests also pass.

## Local checks

- `cargo build --locked --all-targets`: passed.
- `cargo test --locked -- --test-threads=1`: 259 tests passed, plus doc tests.
- `cargo fmt --all --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items`: passed.
- `cargo deny --locked --offline check bans licenses sources`: passed with the three existing duplicate-version warnings.
- `sh -n install.sh` and `shellcheck install.sh`: passed.
- `python3 docs/tests/test_install_script.py`: 37 passed; 7 host-isolation tests skipped on macOS.
- `python3 .github/scripts/check_docs_links.py`: passed.

The first parallel Rust test run hit the existing cask log-flood deadline
test at 5.49 seconds against its 5-second limit. The full serial run passed.
No deadline or runtime code changed.
