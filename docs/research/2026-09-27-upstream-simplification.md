# Simplification with Determinate Nix and Homebrew

Research date: 27 September 2026. This is an exploration, not an approved architecture change.

Repository baseline: `origin/main` at `8bedfbaa11f8ad6def224b85c44ed3fb9bea3268`.
The remote was fetched for this research. A separate managed worktree preserves
the uncommitted work in the original checkout.

Current follow-up constraint: the user wants Homebrew Cask coverage through
Nix packages, with no Homebrew installation or invocation on the client.
Consume Cask metadata through an existing Nix converter. Support a tested
subset of applications. The earlier Brew execution proposal is rejected.
See the [cask integration research](2026-09-27-homebrew-casks.md).

## Finding

The strongest candidate is a small, ordinary-user client for Nix profiles. Determinate can own the machine installation. Nix can own package environments and their retained generations. Existing Cask-to-Nix converters can extend the package catalog. The Homebrew backend and distribution options below were compared before the user specified that clients must not need Homebrew.

This recommendation changes some current product promises. It needs a small proof before a deletion plan. In particular, Nix profiles do not reproduce the current per-file conflict policy, exact build approval, or product-owned recovery records without additional work.

## What Determinate already provides

The installer provides platform setup and uninstall. Its supported environments include Linux, Apple silicon macOS, and WSL. The vendor recommends its native package for macOS. Use the vendor executable as an external dependency; avoid copying its implementation. [Installer documentation](https://github.com/DeterminateSystems/nix-installer)

Determinate owns its generated `/etc/nix/nix.conf`. The supported extension point is `/etc/nix/nix.custom.conf`. Its Nix CLI includes upstream Nix capabilities. [Determinate Nix documentation](https://docs.determinate.systems/determinate-nix/)

Determinate Nixd provides disk-pressure garbage collection, macOS certificate export, boot initialization, version checks, and `sudo determinate-nixd upgrade`. An upgrade can select a specific version. The current command documentation does not establish automatic runtime upgrades as the default. Do not confuse automatic GC with automatic Nix upgrades. [Nixd documentation](https://docs.determinate.systems/determinate-nix/determinate-nixd/)

The installer has a `repair` command, but its current implementation provides shell-hook repair and a specific macOS Sequoia user-ID repair. It is not a general repair engine for arbitrary store damage or interrupted installs. [Repair source](https://github.com/DeterminateSystems/nix-installer/blob/main/src/cli/subcommand/repair.rs)

**Recommendation:** Treat a supported, healthy Determinate installation as a dependency. Keep a version and capability check. Let the vendor own updates and uninstall. A product uninstall should normally remove the product and its own profile, while leaving a shared Nix installation available. This is a proposed ownership change from the current ADR, not a vendor feature.

## What Nix profiles can replace

| Current responsibility | Upstream capability | Condition for removal |
| --- | --- | --- |
| Environment symlink tree | A Nix profile exposes package files through symlinks. | Accept Nix profile layout and conflict rules. |
| Package generations | Profile changes produce retained versions. | Make the profile authoritative. |
| Package list | `nix profile list --json --profile PATH` | Test the supported JSON format. |
| History and rollback | `nix profile history`, `diff-closures`, and `rollback` | Accept upstream history detail or retain a small presentation adapter. |
| Per-output GC protection | Profile generation links are GC roots. | Keep required generations and verify roots on both platforms. |
| Retention | `nix profile wipe-history --older-than Nd` | Accept retention by age, or retain a small selection policy. |

Sources: [profiles and GC roots](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile), [JSON list interface](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-list), [history](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-history), [rollback](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-rollback), [retention](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-wipe-history).

Use a dedicated profile path for `pkg`. Do not take over a user's default profile. `nix profile` and `nix-env` use different manifest formats and cannot freely share one profile. [Profile compatibility](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile#profile-compatibility)

The JSON interface exists, but upstream still marks it experimental. There is a concrete documentation mismatch: the manifest reference shows schema version 1, while Nix 2.35.2 emits version 3. Its implementation accepts versions 1–3. `profile list --json` uses the same serialization. This supports a small version-tested adapter, not an assumption that the schema is permanently stable. [Manifest reference](https://nix.dev/manual/nix/2.35/command-ref/files/manifest.json), [schema serialization](https://github.com/NixOS/nix/blob/2.35.2/src/nix/profile.cc#L209), [JSON list implementation](https://github.com/NixOS/nix/blob/2.35.2/src/nix/profile.cc#L804)

The same source creates a temporary symlink tree and imports it into the store. Profile assembly does not itself schedule a derivation build. This differs from the separate `buildEnv` approach rejected in ADR 0001. Package realization can still build packages. Equal-priority conflicts fail; package priorities can resolve them. This differs from `pkg` per-file keep-first/keep-last rules. [Profile assembly](https://github.com/NixOS/nix/blob/2.35.2/src/nix/profile.cc#L236), [priority option](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-add)

### Semantics that need a decision

- **Pins and upgrades:** Installation supports an exact flake revision and explicit outputs. Upgrade follows the original flake reference. A package installed from an already locked reference is skipped by ordinary profile upgrade. A centrally approved catalog revision therefore still needs an explicit policy. Do not assume `profile upgrade --all` preserves the present catalog contract. [Add examples](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-add), [upgrade behavior](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-upgrade)
- **History:** Upstream history shows changes in top-level packages. Closure differences are separate. The history command does not document JSON output. Keep only the presentation code that the intended interface needs. [History interface](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-history)
- **GC scope:** Profile pruning removes retained rollback points. Store GC removes unreachable paths across the shared store. It is not a `pkg`-only disk cleanup command. Prefer product profile retention plus vendor-managed store GC. [Profile pruning](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-wipe-history), [store GC](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-store-gc)
- **Build approval:** `--max-jobs 0` is not a complete no-build boundary. Remote builders remain eligible, and `preferLocalBuild = true` derivations are exempt from the local-job restriction. Disabling import-from-derivation prevents evaluation-triggered builds. None of these options supplies the current exact, one-time product approval record. [Nix settings](https://nix.dev/manual/nix/2.35/command-ref/conf-file)
- **Privilege:** `allowed-users` controls daemon access. `trusted-users` grants stronger rights and is effectively root access. An ordinary allowed user need not become trusted to use the normal daemon route. Remove the extra privileged service only after removing or separately handling operations that require those stronger rights, such as the current repair policy. [Daemon access settings](https://nix.dev/manual/nix/2.35/command-ref/conf-file)

**Recommendation:** Accept upstream package semantics where possible. Keep name resolution, output selection, and concise error messages in `pkg`. Avoid a second manifest, generation journal, or root ledger that duplicates the profile. These are design recommendations; the research has not proved a migration or failure-recovery implementation.

## Repository metadata: evaluate or consume data

A source parser can recognize Nix syntax. It cannot, by itself, determine a package value after imports, functions, overrides, and platform selection. Nix exposes evaluated JSON through `nix eval --json`. `nix search --json` searches evaluated package attributes and descriptions. Use these interfaces before adding a source parser. [Nix evaluation](https://github.com/NixOS/nix/blob/master/src/nix/eval.md), [Nix search](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-search)

There are three useful levels:

| Need | Existing source or tool | Remaining work |
| --- | --- | --- |
| Search and basic package details | `nix search --json` against a fixed flake revision | User-friendly ranking and optional local caching. Cold evaluation needs measurement. |
| Published Nixpkgs channel metadata | Channel `packages.json.br`, consumed by `nixos-search`'s `flake-info` | Check target systems, revision identity, fields, freshness, and trust. |
| Full evaluation of a chosen revision | `nix-eval-jobs --meta` | Select attributes and systems, normalize output, and publish/cache it if needed. |

`flake-info` has two different paths. For Nixpkgs, it imports a channel's published `packages.json.br`. For other flakes, it queries Nix. It can emit JSON without running the full search website. The website's OpenSearch service is an operational component, not evidence of a stable public package-resolution API. [Exporter documentation](https://github.com/NixOS/nixos-search/blob/main/flake-info/README.md), [search service documentation](https://github.com/NixOS/nixos-search)

`nix-eval-jobs` provides streamed JSON, independent evaluation failures, worker processes, memory controls, output paths, and optional metadata. It can create roots for evaluated derivations. `--apply` adds selected JSON fields; `--check-cache-status` reports observed local/cache availability. This can replace custom worker supervision and evaluation orchestration. It does not remove source trust, schema normalization, or cache publication. Cache status is a point-in-time observation, not a build-approval guarantee. Tool versions track the Nix major/minor series, so pin and test compatibility. [Tool documentation](https://github.com/NixOS/nix-eval-jobs)

`nix-index` answers a different question: which cached package contains a file or command? It is useful for command-not-found support. It is not a replacement for complete package metadata or evaluation. [nix-index documentation](https://github.com/nix-community/nix-index)

**Recommendation:** Start with native JSON search. Keep a small discovery cache only if measured cold search requires it. If a central catalog remains necessary, first compare published channel metadata. Use `nix-eval-jobs` when exact revision or field requirements justify evaluation. Do not import an entire search service just to obtain package names.

## The three Homebrew choices

| Choice | What it can remove | What it changes or retains | Assessment |
| --- | --- | --- | --- |
| Distribute `pkg` through a tap | Client download, placement, upgrade, and removal code | Nix setup is still a separate vendor operation. | Strong candidate after the client becomes unprivileged. |
| Use Homebrew as the package backend | The Nix integration and Nix catalog, if Nix is removed completely | Adopts Homebrew's prefix, package, upgrade, and cleanup model. | Viable product pivot. |
| Use Homebrew metadata | A Ruby formula parser and local core/cask checkout for discovery | It describes Brew packages, not equivalent Nix attributes or outputs. | Useful for a Brew-backed catalog; limited for a Nix product. |

### Distributing the client

A tap can publish a formula outside `homebrew/core`. Core has its own acceptance criteria and requires a stable upstream release. The current `pkg` alpha should not assume core admission. The project would still maintain a formula and any tap bottle publishing. [Tap documentation](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap), [core admission policy](https://docs.brew.sh/Acceptable-Formulae)

The full official core and cask repository trees were checked. Neither contained `nix.rb`, `nix-installer.rb`, or a Determinate-named formula/cask. Core tree: `340eb74d5379526452b6b8da67cb9882af8e394f`. Cask tree: `71d2392a6d4f7ba8ced1239d8886a9c407bb712b`. This is a bounded name search, not proof that no third-party tap exists. A `pkg` formula must not presume an official Brew dependency that provisions Determinate. [Core tree API](https://api.github.com/repos/Homebrew/homebrew-core/git/trees/340eb74d5379526452b6b8da67cb9882af8e394f?recursive=1), [cask tree API](https://api.github.com/repos/Homebrew/homebrew-cask/git/trees/71d2392a6d4f7ba8ced1239d8886a9c407bb712b?recursive=1)

### Replacing the backend

Homebrew supports ordinary formula installation without sudo after prefix setup. Its default prefixes are `/opt/homebrew`, `/usr/local`, and `/home/linuxbrew/.linuxbrew`. Casks and services can need additional privilege. A shared installation depends on trust in accounts that can write its prefix. This is a different ownership model from per-user profiles over the Nix store. [Installation and ownership](https://docs.brew.sh/Installation)

Homebrew upgrades can also upgrade dependencies and dependents. Old formula versions are normally cleaned up. Pinning does not guarantee indefinite preservation when dependents need a newer version. There is no equivalent whole-environment generation rollback documented in these workflows. A Brew backend is therefore suitable only if the product accepts these semantics. [Upgrade and cleanup behavior](https://docs.brew.sh/FAQ)

**Recommendation:** If Nix rollback and immutable package closures are core features, retain Nix. If the product only needs a simpler interface for normal install/update/remove operations, compare a pure Brew client. Maintaining both backends adds work unless a clear requirement needs both.

### Reusing metadata

The official JSON API provides bulk and individual formula/cask data. It includes versions, dependencies, platform variations, bottle URLs/checksums, and source references. Use it for discovery instead of parsing Ruby. [Formula API](https://formulae.brew.sh/docs/api/)

Homebrew's normal official-tap install path consumes JWS-signed metadata. This is distinct from merely fetching a convenient public JSON endpoint. Let Brew retain its verification and installation logic. Reimplementing bottle extraction, relocation, linking, receipts, and postinstall behavior would create a second package engine. [Supply-chain documentation](https://docs.brew.sh/Homebrew-Security-and-Supply-Chain), [Homebrew's account of implementation costs](https://docs.brew.sh/FAQ#why-dont-you-rewrite-homebrew-in-rust-to-make-it-faster)

## Where the current repository can become smaller

These findings come from the fetched commit. They describe candidates, not a
proved list of files that can all be deleted.

| Priority | Current code and reason for its existence | Proposed change | Remaining condition |
| --- | --- | --- | --- |
| 1 | [Private legacy Base Nix module](../../crates/pkg-installer/src/linux_uninstall.rs), plus old provisioning internals marked with `expect(dead_code)` | Remove the unreachable paths and their obsolete tests after checking callers. | Some managed modules still provide live vendor authentication and release assets. Do not delete by directory name. |
| 2 | [Activation forest](../../crates/pkg-store/src/activate.rs), [generation commits](../../crates/pkg-pipeline/src/commit.rs), [rollback](../../crates/pkg-pipeline/src/rollback.rs), and [root publication](../../crates/pkg-store/src/roots.rs) | Make a dedicated Nix profile authoritative. Remove the duplicate activation, generation, journal, and root protocols. | Accept profile conflict and history semantics. Keep only product metadata that Nix does not represent. |
| 3 | [Broker and helper services](../../crates/pkg-installer/src/service.rs), [maintenance capabilities](../../crates/pkg-nix/src/maintenance.rs), and platform service installation | Run the client as the user through the existing Nix daemon. | Drop or separately handle privileged store repair, exact approval enforcement, and product-specific source isolation. Ordinary daemon access must be available. |
| 4 | [Determinate handoff](../../crates/pkg-installer/src/determinate_handoff.rs), product install journals, and coupled system uninstall | Use a separately managed vendor installation. Distribute only the unprivileged client through a tap or release archive. | This changes the promise that installing/removing pkg also installs/removes Nix. Keeping automatic setup retains a small vendor invocation layer. |
| 5 | [Catalog projection](../../crates/pkg-index/nix/index-meta.nix), [release index tool](../../tools/release/src/bin/pkg-release-index.rs), and [signed channel](../../crates/pkg-channel/src/tuf.rs) | Compare native search and published channel metadata before retaining a custom publisher. | A source hash identifies content; it does not replace signed publisher authority, expiry, or rollback protection. Keep those controls if the product still requires them. |

The broker is not needed merely because Nix has a privileged daemon. It exists
to enforce additional pkg rules. [ADR 0003](../adr/0003-broker-root-helper-privilege-split.md)
names local-store repair and privileged root writes as reasons for the helper.
The [README](../../README.md) also states that raw Nix access and daemon access
are not product security barriers. This supports reconsidering the extra
services. It does not prove that deleting them preserves every current rule.

The repository already delegates Nix parsing and evaluation. The
[real adapter](../../crates/pkg-nix/src/real/mod.rs) runs `nix eval --json --apply`
with the small checked-in projection. The
[flake adapter](../../crates/pkg-nix/src/real/flake.rs) calls Nix for source locks
and derivation evaluation. The [resolver](../../crates/pkg-resolver/src/lib.rs)
uses the index for discovery only. Installing from a result still evaluates
the selected source. Replacing a Rust parser is therefore not the main task.

For scale, physical Rust source lines under each crate's `src` directory are:

| Area | Lines |
| --- | ---: |
| `pkg-installer` | 47,493 |
| `pkg-nix` | 38,467 |
| `pkg-pipeline` | 8,681 |
| `pkg-store` | 3,600 |
| `pkg-index` | 1,839 |

These counts include comments, blank lines, and tests in `src`. They show where
to investigate. They are not production-code counts or deletion estimates.

There are two constraints on a smaller design:

- The current [adapter version check](../../crates/pkg-nix/src/real/mod.rs)
  requires the exact Determinate 3.22.1 / Nix 2.35.2 version strings. Vendor-led
  upgrades need a tested compatibility policy. Simply allowing the vendor to
  update can make the present client refuse to run.
- Current [package commands](../commands.md) promise exact build approval,
  platform-specific source isolation, saved pin/output details, and defined GC
  retention. Each promise must be retained, changed, or removed explicitly.
  Determinate does not implement these pkg-specific rules on the project's behalf.

The old dead-code comments mention DN-19 as future work. The
[completed-work record](../../plans/completed-alpha-work.md) says that phase is
complete and requires live-use checks for the remaining modules. Treat this
as stale documentation plus an audit candidate. The runtime archive code still
has a caller in the release proof example.

## Recommended next experiment

Build one small profile adapter before changing the current application.
Use an isolated profile on disposable Linux and Apple silicon macOS hosts.
Exercise install, list, upgrade, remove, rollback, and profile-history pruning.
Include a conflicting executable, multiple outputs, a pinned package, and an
interrupted operation. Confirm that GC retains all selected rollback points.
Test the supported version's JSON instead of parsing its human-readable output.

Measure cold and warm search for one exact Nixpkgs revision. Compare the fields
and platform coverage with published metadata. Keep the existing small index
if an upstream replacement introduces more work or loses needed coverage.

The preferred target is: pkg owns names, commands, output, and a small amount of
user intent; Nix owns profiles and package realization; Determinate owns the
machine runtime. A pinned upstream converter supplies compatible Cask-derived
packages through the same Nix route. Brew execution is outside the user's
requested design. Coverage limits should be explicit instead of adding a
second host installer to support every Cask.

No prototype or migration was implemented in this research. No build or runtime
test was needed for the documentation change. The next experiment is proposed
work, not a claim of tested replacement behavior.

## License and operational limits

- The Determinate installer and public Nix CLI source use LGPL-2.1. This does not establish the license for every bundled component. The official Determinate flake downloads Nixd binaries separately. This research did not establish an explicit Nixd redistribution grant. Prefer vendor delivery; check the exact shipped artifact terms before expanding redistribution. [Installer license](https://github.com/DeterminateSystems/nix-installer/blob/main/LICENSE), [CLI license statement](https://github.com/DeterminateSystems/nix-src#license), [Nixd packaging](https://github.com/DeterminateSystems/determinate/blob/main/flake.nix)
- Homebrew uses BSD-2-Clause. Packages retain their own licenses. A Homebrew client dependency does not grant rights to redistribute every formula's software. [Homebrew license](https://github.com/Homebrew/brew/blob/main/LICENSE.txt)
- `nix-eval-jobs` publishes a GPL-3.0 license. Prefer the upstream executable as a build/research tool instead of copying its code into the Apache-2.0 project. Distribution of that executable still needs its notices and source obligations. This research did not resolve licensing for copied `flake-info` source. [nix-eval-jobs license](https://github.com/NixOS/nix-eval-jobs/blob/main/LICENSE.md)

No installers, builds, profiles, or host settings were changed for this research. Current docs and source were checked. Context7 resolved and queried Determinate, Nix, and Homebrew first. It did not return an exact library for `nix-eval-jobs` or `nixos-search`, so their upstream documentation was read directly. Runtime behavior still needs a disposable-host proof on the product's exact supported versions.
