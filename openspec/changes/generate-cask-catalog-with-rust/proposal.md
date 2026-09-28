# Generate the Cask catalog with a Rust tool

Planning change, 28 September 2026. Implementation is substantially
complete and verified on real hosts; per-task status and evidence
live in the [tasks](tasks.md) and the [verification
note](../../../docs/verification/2026-09-28-rust-cask-catalog.md).
Pending items are marked there, not hidden.

Baseline: workspace sync at `f5c227032964990a8cb6833f64472f9925777c85`
(`0.2.0-alpha.2`) on `feat/rust-cask-catalog`, including the research note
[Dynamic cask selection and Linux support](../../../docs/research/2026-09-28-dynamic-casks-and-linux.md).
Upstream converter reference: brew-nix `16131ae4126c54b1502aa7eaf6573d7fbf16b656` (MIT).

## Why

Today the cask source supports three hand-curated tokens on one system
through the pinned brew-nix converter, with a Nix-level classifier and a
literal `CASK_SYSTEM` constant in the client. Every new package needs a
reviewed token list edit, and Linux casks are impossible. The user has
chosen a different architecture: a Rust catalog generator that runs at
catalog-update time, so Nix only fetches, builds, and installs from
generated data, and the client never runs a converter.

Goal: broad automatic Homebrew Cask conversion with no token allowlists,
no hardcoded app names, no hosted API or database, no Brew or Ruby
runtime, and low maintenance. Reuse proven upstream brew-nix logic where
it fits instead of rewriting blindly. Breaking changes are allowed; no
compatibility layers.

## What Changes

- **BREAKING:** Add a maintainer-only Rust tool (`tools/cask-catalog`,
  a workspace member) that reads an offline pinned Homebrew Cask JSON
  snapshot and emits one deterministic, compact, normalized catalog JSON
  file with per-target eligibility, exclusion reasons, typed artifact
  plans, and provenance.
- **BREAKING:** `nix/casks` becomes a standard flake that reads the
  committed generated data with generic, data-driven builders. The
  brew-nix flake input is removed once the builders we need are owned
  and its MIT attribution is retained. No per-package Nix source files,
  no import-from-derivation, no compiler at Nix evaluation, lazy
  per-token derivations.
- **BREAKING:** There is no dependency graph and no profile expansion:
  every record with a formula dependency is excluded
  (`formula-dependency`) and every record with a cask dependency is
  excluded (`cask-dependency-integration`). The native Nix lifecycle stays
  single-package.
- **BREAKING:** Scope grows from `aarch64-darwin` only to
  `aarch64-darwin` plus `x86_64-linux`: macOS apps with app+CLI paths,
  Linux static/simple ELF binaries and AppImages through Nixpkgs tools,
  pure-extractable `.pkg` payloads under a structural proof rule. Fonts,
  formula bottles, services, drivers, and privileged installers stay out.
- **BREAKING:** Replace the `pkg-cask-support/1` manifest and the
  client's `CASK_SYSTEM` global with a `pkg-cask-catalog/2` status index
  generated from the same artifact plans that drive the Nix builds.
  Broad catalog search reads the cheap generated index instead of
  forcing derivations. `info` and `install` point at the ordinary flake
  attribute. Unknown tokens never become eligible.
- Snapshot updates are maintainer-run, repeatable, and reviewed: pinned
  input revision plus content hash, generator version in provenance,
  generated data and pin committed together. No server, no scheduler,
  no redundant double pins.
- The client keeps the ordinary native profile lifecycle. Original
  moving references, upgrades, rollback, and launcher sync are
  unchanged. Homebrew cleanup hooks (`zap`, `uninstall`) are never run.

## Capabilities

### New Capabilities

- `cask-catalog-generation`: the offline Rust generator — determinism,
  input pinning, variation merge, eligibility, typed artifact plans,
  path and injection safety, whole-catalog integrity.
- `generated-cask-packages`: the standard Nix source over generated
  data — generic builders for macOS and Linux artifacts, dependency
  handling, structural `.pkg` rule, lazy evaluation without IFD.
- `catalog-status-discovery`: one generated catalog status source for
  the client — format gate, cheap broad search, per-system capability
  without a client constant, install gating, moving-reference
  behavior.

### Modified Capabilities

None. The repository has no canonical `openspec/specs` yet; the prior
change `simplify-to-native-nix` is unarchived, so its capability names
are not reused as modified subjects here. Its `cask-packages` behavior
is replaced wholesale by this change's new capabilities.

## Impact

- New code: `tools/cask-catalog` (generator), `nix/casks/lib`
  (builders), `nix/casks/catalog/` (generated data + pin).
- Replaced: the brew-nix wrapper layer and curated token lists in
  `nix/casks/flake.nix`; `caskSupport`, `CASK_SYSTEM`, and
  `pkg-cask-support/1` decoding in `crates/pkg-cli`.
- Unchanged: native profile lifecycle, Nix runtime policy, launcher
  sync, release packaging (the client archive still contains only the
  client; the generator is a maintainer tool and never ships).
- The [design](design.md) resolves the open points. The
  [plan](../../../docs/plans/rust-cask-catalog.md) is the readable
  end-to-end view; the [HTML copy](../../../artifacts/rust-cask-catalog-plan.html)
  mirrors it. OpenSpec is authoritative.
