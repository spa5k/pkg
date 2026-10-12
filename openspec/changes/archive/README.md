# OpenSpec archive

These changes were archived on 12 October 2026.
The table separates completed work from superseded work.
The task files retain their original completion marks.

The native Nix design baseline is
[Simplify pkg to native Nix](2026-10-12-simplify-to-native-nix/proposal.md)
with its [design](2026-10-12-simplify-to-native-nix/design.md).
Later completed changes record updates to that baseline.
Superseded changes are history. They were not delivered.

| Change | Status |
| --- | --- |
| [Add deterministic verification suite](2026-10-12-add-deterministic-verification-suite/proposal.md) | Superseded; 22 tasks remain unchecked |
| [Amortize search with revision snapshots](2026-10-12-amortize-search-with-revision-snapshots/proposal.md) | Completed |
| [Execute production trust ceremony](2026-10-12-execute-production-trust-ceremony/proposal.md) | Superseded; 16 tasks remain unchecked |
| [Generate Cask catalog with Rust](2026-10-12-generate-cask-catalog-with-rust/proposal.md) | Completed |
| [Harden native package lifecycle](2026-10-12-harden-native-package-lifecycle/proposal.md) | Completed |
| [Implement audit repairs](2026-10-12-implement-audit-repairs/proposal.md) | Completed |
| [Import public Cask taps](2026-10-12-import-public-cask-taps/proposal.md) | Completed |
| [Improve CLI discovery](2026-10-12-improve-cli-discovery/proposal.md) | Completed |
| [Improve Homebrew discovery](2026-10-12-improve-homebrew-discovery/proposal.md) | Completed |
| [Improve package command experience](2026-10-12-improve-package-command-experience/proposal.md) | Completed |
| [Improve package conflicts](2026-10-12-improve-package-conflicts/proposal.md) | Completed |
| [Improve runtime discovery and shell setup](2026-10-12-improve-runtime-discovery-and-shell-setup/proposal.md) | Completed |
| [Improve Rust quality](2026-10-12-improve-rust-quality/proposal.md) | Completed |
| [Repair realistic use faults](2026-10-12-repair-realistic-use-faults/proposal.md) | Completed |
| [Simplify client](2026-10-12-simplify-client/proposal.md) | Completed |
| [Simplify native client internals](2026-10-12-simplify-native-client-internals/proposal.md) | Completed |
| [Simplify to native Nix](2026-10-12-simplify-to-native-nix/proposal.md) | Completed; design baseline |

These historical changes were archived with `--skip-specs`.
Their delta specs were not applied to `openspec/specs/`.
Some older deltas describe removed package engines and trust systems.

The two changes in the current Rust quality PR remain outside the archive:

- [Enforce Rust quality](../enforce-rust-quality/proposal.md)
- [Complete the Rust quality plan](../complete-rust-quality-plan/proposal.md)

Archive these changes after the PR is merged.
Keep all new work in a separate active change.
