# Contributing to `pkg`

**OpenSpec is the source of truth.** The active design is
[Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md)
with its [design](openspec/changes/simplify-to-native-nix/design.md) and
[tasks](openspec/changes/simplify-to-native-nix/tasks.md). The cask source
redesign is
[Generate the Cask catalog with a Rust tool](openspec/changes/generate-cask-catalog-with-rust/proposal.md).
The proposed next phase is
[Import raw Ruby casks from public taps](openspec/changes/import-public-cask-taps/proposal.md).
The alpha.4 runtime discovery and installer shell setup change is
[Improve runtime discovery and shell setup](openspec/changes/improve-runtime-discovery-and-shell-setup/proposal.md).
Search amortization is
[Amortize search with revision snapshots](openspec/changes/amortize-search-with-revision-snapshots/proposal.md)
with its [design](openspec/changes/amortize-search-with-revision-snapshots/design.md) and
[tasks](openspec/changes/amortize-search-with-revision-snapshots/tasks.md).

## 1. Before you open a PR

- **Map your PR to a task.** Name the `NN-xx` task number in the PR
  description. If your change is not covered by a task, propose a design
  change in OpenSpec first.
- **One purpose per PR.** A reviewer must be able to hold the whole change
  in their head.
- **Do not resurrect removed machinery.** The broker, root helper, package
  state engine, build approval, TUF channel, central index, and their tests
  are deleted by design. New work builds on the native Nix profile.
- **Old plans are history.** Files under `plans/` are superseded or
  completed records. Do not treat them as requirements. Do not mark
  superseded proposals as delivered.

## 2. Required checks

- All PRs: the **ci** workflow is green. It runs the ordinary Rust checks —
  build, test, format, clippy, docs, and dependency policy — on the pinned
  toolchain (currently Rust 1.96.1 via `rust-toolchain.toml`), plus a small
  client smoke job on Linux and macOS.
- All PRs: the **docs-linkcheck** workflow is green. It validates Markdown
  cross-references and requires README/CONTRIBUTING to link the active
  OpenSpec change.
- Run locally:

```sh
cargo build --locked --all-targets
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
sh -n install.sh
shellcheck install.sh
python3 docs/tests/test_install_script.py
python3 .github/scripts/check_docs_links.py
```

## 3. Tests and evidence

- Keep the test suite small and direct. Add a focused test when it catches a
  real defect in code we own. Do not add a new assurance framework, fault
  matrix, or mutation campaign.
- Do not fabricate results. Record only checks you ran, with the system,
  revisions, and outcomes. Historical proof records from the old engine are archived at the [plan baseline](https://github.com/spa5k/pkg/tree/5256cfa5a052f64ae4219ac1aaf316a7a00d49a2/tests); current evidence lives in `docs/verification/`
  and dated reports stay as history.
- Do not mark OpenSpec tasks complete in a PR that does not finish them.
  The tasks file records completion, not the PR description.

## 4. Release

Client releases are assembled by `tools/release/package_client.sh` through
the `client-release` workflow: one archive per supported system
(x86_64 Linux, Apple silicon macOS), with completions, notices, and
checksums. The client contains no privileged helpers and no private Nix
bundle. See [tools/release/README.md](tools/release/README.md).

## 5. Communication

User-facing docs and release notes use short, direct sentences
(ASD-STE100 Simplified Technical English), one instruction per sentence.
