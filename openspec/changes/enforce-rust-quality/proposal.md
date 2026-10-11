# Enforce Rust quality

The current workspace denies Clippy's default group and selected extra lints.
It does not check the full pedantic group.
The local `just lint` command also accepts warnings that CI rejects.
Public fallible functions often omit their error contract.
Some functions mix input validation, data conversion, and publication.

Use the official [Rust Style Guide](https://doc.rust-lang.org/style-guide/).
Use the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
for names, ownership, types, errors, and documentation.
Enforce the full Clippy pedantic group and selected useful restriction and nursery lints.
Keep each required exception local and give it a specific reason.
Use the same warning policy in local checks and CI.

Run build and lint checks across all 83 baseline Rust files in both crates.
Review the runtime, state, app, and importer owners in depth.
Record the manual review limits.
Record confirmed defects and quality weaknesses before repairs.
Repair the lint backlog and the highest priority confirmed defects.
Add focused tests for changed behavior.
Keep Nix as the owner of installed state.

The [design](design.md) defines the work sequence and completion checks.
The [audit](../../../docs/verification/rust-quality-audit-2026-10-12.md)
records findings, decisions, and results.
