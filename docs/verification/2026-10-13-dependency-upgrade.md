# Dependency update verification

Date: 2026-10-13.
Scope: the existing Package Resolution working tree, Rust 1.99.0, macOS Apple
silicon, and Linux x86_64.

All 21 direct external Rust dependencies match the latest stable versions
reported by the [crates.io registry API](https://crates.io/data-access).
Fourteen dependencies were already current. Seven changed:

| Dependency | Before | After |
| --- | --- | --- |
| clap | 4.6.6 | 4.6.7 |
| clap_complete | 4.6.9 | 4.6.11 |
| libc | 0.2.189 | 0.2.190 |
| reqwest | 0.12.28 | 0.13.5 |
| serde_json | 1.0.151 | 1.0.154 |
| sha2 | 0.10.9 | 0.11.1 |
| toml | 0.9.8 | 1.1.8+spec-1.1.0 |

`cargo update` refreshed transitive dependencies within their upstream version
requirements. Exact direct version pins remain. The minimum supported Rust
version remains 1.96. The pinned `cargo-deny` 0.20.2 is already current.

[Reqwest 0.13](https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md)
renamed the Rustls feature. It now uses platform certificate verification and
the AWS-LC provider. Default features remain disabled. Proxy refusal, address
validation, pinned DNS answers, redirect refusal, and request bounds remain.
SHA-2 now uses one lowercase hexadecimal encoder. A fixed SHA-256 test vector
checks padding, case-insensitive comparison, and mismatch reporting.

The CI actions use exact commits for the latest stable releases:
[checkout v7.0.1](https://github.com/actions/checkout/releases/tag/v7.0.1),
[upload-artifact v7.0.2](https://github.com/actions/upload-artifact/releases/tag/v7.0.2),
and [determinate-nix-action v3.23.1](https://github.com/DeterminateSystems/determinate-nix-action/releases/tag/v3.23.1).
The ShellCheck directive in the optional toolchain flags step now has valid syntax.

The shared Nixpkgs pin follows the existing rolling channel. The latest tested
[channel revision](https://channels.nixos.org/nixpkgs-unstable/git-revision) was
`8edc0c72e3a38faf5434e40d1f19431125fe4b30`. Both flake locks and the raw tap
runtime tarball hashes match it. The Formula catalog was regenerated from the
existing pinned Core snapshot. Linux reproduced the same bytes.

The catalog has 1,337 changed entries. On macOS, 141 entries became eligible and
30 became excluded. On Linux, 218 became eligible and 24 became excluded.
These changes follow upstream versions and platform support. Existing identity,
version-major, dependency, and installation gates remain.

| Check | macOS | Linux |
| --- | --- | --- |
| Locked workspace build, all targets and features | Pass | Pass |
| Locked workspace tests, all features | Pass | Pass |
| Format and strict Clippy | Pass | Pass |
| Private Rustdoc, warnings denied | Pass | Pass |
| Dependency policy, including advisories | Pass | Pass |
| Four real Nix discovery tests | Pass | Pass |
| Native CLI checks | Pass, four groups | Pass, two groups |
| Reqwest HTTPS download and pinned hash verification | Pass | Pass |
| Tap reader under the pinned Ruby runtime | Pass, nine checks | Pass, nine checks |
| Archive boundary checks | Pass | Pass |
| Formula `fd` build and execution | Pass, 10.5.0 | Pass, 10.5.0 |
| Formula bottle build and execution | Pass | Unsupported by design |
| Cask builder checks | Linux suite | Pass, 14 checks |

The macOS installer suite ran 44 tests: 37 passed and seven skipped because the
host lacks mount namespace isolation. Shell syntax and ShellCheck passed.
Workflow lint, Markdown links, and diff whitespace checks passed.

The Rust and Nix checks used a manifest of 206 files. Hashes matched before and
after both complete Rust runs. Only the CI comment syntax changed afterward;
workflow lint checked that change separately. No Rust or Nix file changed.
Raw Linux logs were downloaded to `/tmp/pkg-deps-linux-evidence-20261013/`.
The Linux sandbox was stopped after the download.

This follows the [Rust compiler update](2026-10-13-rust-1.99-upgrade.md) and the
[Package Resolution and cancellation work](2026-10-12-package-resolution-and-tap-cancellation.md).
The user withdrew the 20% reduction target. No further reduction work was done.

## CI follow-up, 2026-10-17

The cask generation check found 36 stale exclusion detail messages. Regeneration
from the same verified snapshot changed only those messages. Eligibility, status,
reason codes, plans, and input pins did not change.

The native smoke checks exposed a runtime difference. Determinate Nix 3.23.1
quotes catalog attribute segments in profile JSON. Version 3.22.1 leaves those
segments unquoted. The adapter now normalizes valid encoded catalog segments in
profile entries and native search results. Opaque profile entry keys and other
attribute syntax remain unchanged. This keeps package identity, duplicate
installation checks, and upgrade gates consistent across both runtime versions.

The regression test failed on the captured quoted shape before the fix. It now
passes for both quote forms. Separate checks retain unknown escapes, raw dots,
embedded quotes, and ordinary quoted Nix paths. All workspace tests, strict
Clippy, formatting, and private Rustdoc checks passed locally. The CI smoke
script now records the runtime version and the actual rows on identity failures.
