# Native Nix design

## Context

See [proposal.md](proposal.md) for the reason and scope.

This plan uses origin/main at `5256cfa5a052f64ae4219ac1aaf316a7a00d49a2`,
fetched on 28 September 2026. Compared with the first report at `8bedfba`,
main has alpha.57 and alpha.58 release work, better preparation failures,
better public-source error categories, and Linux install checks. It still
uses the same custom package engine.

The workspace has eleven product crates and the release-tools crate.
The CLI still exposes the old command grammar in
[cli.rs](https://github.com/spa5k/pkg/blob/5256cfa5a052f64ae4219ac1aaf316a7a00d49a2/crates/pkg-cli/src/cli.rs).
[pkg-store](https://github.com/spa5k/pkg/blob/5256cfa5a052f64ae4219ac1aaf316a7a00d49a2/crates/pkg-store/src/lib.rs) owns activation, generations,
roots, and journals.
[pkg-pipeline](https://github.com/spa5k/pkg/blob/5256cfa5a052f64ae4219ac1aaf316a7a00d49a2/crates/pkg-pipeline/src/lib.rs) owns custom preparation
and commit behavior.
[pkg-installer](https://github.com/spa5k/pkg/blob/5256cfa5a052f64ae4219ac1aaf316a7a00d49a2/crates/pkg-installer/Cargo.toml) builds three
privileged or installation executables.

Some prose on main still says alpha.57. The Cargo version, current commands,
and installed release entry have moved to alpha.58. Use the fetched code as
the baseline. Do not treat stale release prose as a new work requirement.

User constraints:

- Breaking changes are allowed.
- Old promises, state formats, command syntax, and tests are not constraints.
- Homebrew must not be installed or invoked by pkg.
- This change plans the implementation. It does not implement or publish it.

## Goals / Non-Goals

**Goals**

- Delegate package state and realization to Nix.
- Delegate machine runtime lifecycle to Determinate.
- Keep pkg useful for package names, commands, output, and desktop apps.
- Remove whole responsibilities and their maintenance work.

**Non-Goals**

- Backward compatibility, automatic migration, or old-install removal.
- A daemon, root helper, generic provider framework, or second package engine.
- Exact build-resource approval, a custom package trust channel, or custom GC.
- Universal Cask support, macOS installer scripts, drivers, or system services.
- Rewriting old tests, preserving coverage counts, or completing old proof campaigns.
- Whole-system configuration through Home Manager or nix-darwin.

## Decisions

### D1. Use one application crate and one concrete Nix adapter

Keep `crates/pkg-cli` as the application crate and keep its public binary name
`pkg`. Replace orchestration in place on the implementation branch.
Use internal modules: commands, output, nix, catalog, apps, and config.
Keep useful presentation code only when it fits the new command behavior.

The nix module owns process construction and native output decoding.
Callers ask for profile operations. They do not know Nix JSON fields.
The catalog module maps names to installables. It does not own installed state.
The apps module derives launchers from installed store outputs.

Do not add a permanent pkg-next crate, a legacy mode, or a compatibility adapter.
Intermediate commits on the implementation branch need not support alpha users.
Each reviewable work group should build before it is merged.

Alternative rejected: reduce crate count while keeping the same broker,
journal, and approval protocols. That would preserve the main cost.

### D2. Treat Determinate Nix as an external prerequisite

Find Nix from the user's configured executable path. Resolve its absolute path
once per invocation. Probe version and the required command behavior.
Use the normal daemon as an ordinary allowed user.
Do not add the user to trusted-users. Do not use sudo or manage services.
Keep source-provided Nix settings disabled. Never automatically accept new
substituters or signing keys from a flake.

Record a tested runtime range. Start with the current standard runtime
baseline: Determinate 3.22.1 with Nix 2.35.2. Remove the old dual private/standard
runtime routing and exact full-brand-string gate.
Unsupported output or a missing command fails with a useful diagnostic.
Runtime installation and updates remain vendor operations.

Keep the lesson from alpha.58: a download failure, an unavailable daemon,
and a verification failure must not all become "package not found."
Preserve native diagnostics. Only classify errors when there is direct
evidence. Do not copy the old broker refusal-code protocol.

Alternative rejected: keep a small pkg installer around the vendor installer.
Even a wrapper would retain handoff, receipts, privilege, and uncertain-state work.

### D3. Make the native profile authoritative

Use this named profile on both supported systems:

    ${XDG_STATE_HOME:-$HOME/.local/state}/nix/profiles/pkg

Require an absolute XDG state path when set. Pass the complete profile path
to every native profile command. Do not use the user's default profile,
old pkg state directory, or a global registry alias to choose it.
Let Nix create and maintain the profile and generation links.
Confirm root registration in the first native check. Do not add a second
root ledger if a path assumption is wrong; correct the profile use.

Use config at `${XDG_CONFIG_HOME:-$HOME/.config}/pkg/config.toml` and disposable
discovery data at `${XDG_CACHE_HOME:-$HOME/.cache}/pkg/` on both systems.
Require absolute configured base paths. These paths are new conventions.
No config or cache file is authoritative for installed packages.
For the initial client, config stores only source references and display settings.

Read installed entries from `nix profile list --json`.
Keep native entry names as removal and upgrade identifiers.
Read the original source, locked source, attributes, and outputs from Nix.
Ignore unknown optional JSON fields. Fail clearly when required identity
fields are absent. Do not directly rewrite Nix's manifest.

Use Nix's profile locks. If the small read-and-launcher-sync sequence needs
serialization, use one local per-profile advisory lock. It is not a journal.

Alternative rejected: retain pkg's manifest to make old list/history output
look the same. That would restore two sources of installed truth.

### D4. Use a smaller command contract

| Command | Final behavior |
| --- | --- |
| search QUERY | Search supported sources. Show source-qualified names and support status. |
| info ID | Show source, attribute, metadata revision, and known limits. Identify installed data separately. |
| install ID… | Resolve all names, then submit one native profile add. Allow normal Nix downloads and builds. |
| list | Read the dedicated profile. Show native entry IDs and source identity. |
| remove ENTRY… | Remove exact installed entries in one native operation. Do not treat input as an unreviewed regular expression. |
| update | Refresh discovery data. Do not alter installed packages or define a global approved version. |
| upgrade ENTRY… / --all | Follow each installed entry's original reference. Explicitly report fixed references. |
| history | Display native profile generations. No product diff database or JSON history contract. |
| rollback [N] | Use Nix's native previous or selected generation. Refresh derived launchers. |
| prune --older-than Nd | Prune only eligible non-current profile generations. Require an explicit positive age. |
| doctor | Read runtime, profile, and launcher status. Do not repair or send reports. |
| shellenv | Print idempotent Bash/Zsh path settings for this profile. Do not edit shell files. |
| completion SHELL | Generate completion for the current reduced grammar. |
| apps sync | Rebuild pkg-owned macOS launchers from the active profile. This also retries a failed post-operation sync. |

Remove pin/unpin, outdated, repair, gc, system uninstall, keep-first/keep-last,
global dry-run, build approval, keep-going, alternate legacy state, and JSONL.
Keep help, version, no-color, and verbose.
Offer a small versioned JSON result only for search, info, list, and doctor.
Mutating commands show native progress and a short final result.
Return success only when the requested work succeeds. If launcher sync fails
after a profile update, return a nonzero result that states the package change
completed and names `pkg apps sync` as the retry.
Use normal CLI usage errors for removed commands. Do not preserve old numbers.

Do not silently resolve file collisions with product policy.
Report Nix's conflict and the conflicting entries. Initial support requires
the user to remove the conflicting entry. Defer a priority option.

Fixed references replace pin state. A fixed package does not advance during
normal upgrade. To change its reference, remove that entry and install the new
reference. Retained generations provide rollback; do not add a repin transaction.

### D5. Discover packages without parsing repositories

Default Nixpkgs source: `github:NixOS/nixpkgs/nixpkgs-unstable`.
Use the explicit source, not the user's global flake registry.
Allow config to replace the default reference.
Use `nixpkgs:<attribute>` and `cask:<token>` as catalog IDs.
A bare name is valid only when it has one supported exact match.
Ambiguous names require a source-qualified ID.
Accept explicit public GitHub flake installables with an attribute after `#`.
Do not add arbitrary tap execution, a source-provider framework, or local path
installation in the initial client.

Use native evaluated JSON for search and info. Request only required metadata.
Cache search results with the source, resolved revision, system, and schema.
`update` refreshes or invalidates that cache. A network failure must not be
reported as an empty successful search. Clearly label reused stale data.
Use the locked reference for a single discovery result set.
Pass the original moving reference to installation so native upgrade works.
Show the actual resolved installation after completion.

Do not promise exhaustive Nixpkgs search coverage or the previous record count.
Measure cold search during implementation. Start with this native path.
Published Nixpkgs metadata is a later optimization if a measured problem remains.
Do not add a custom evaluator, `nix-index`, OpenSearch, or repository parser now.

Alternative rejected: keep the old signed central index as installed-version
authority. It adds source publication and approval semantics that are no longer wanted.

### D6. Reuse brew-nix through a small product flake

Add `nix/casks/flake.nix` with locked Nixpkgs, brew-nix, and metadata inputs.
Use the inspected brew-nix revision as the initial seed:
`16131ae4126c54b1502aa7eaf6573d7fbf16b656`.
Resolve and commit the complete lock during implementation.
Override its metadata input explicitly; do not rely on a possibly stale default.

Expose ordinary `packages.aarch64-darwin` outputs for supported tokens.
Publish a support list from the same revision. It includes unsupported reasons.
Default client source:
`github:spa5k/pkg/main?dir=nix/casks`.
The outer reference moves; the inputs in each published revision are locked.
Updates are ordinary reviewed lock changes on main. No signing service,
Ruby parser, mirror, scheduler, or second installer is required.

Initial support: immutable vendor downloads with verified hashes, simple app
archives, and standalone executables. Add small overrides for needed app-plus-CLI
cases. Exclude native installer actions, privileged helpers, drivers, services,
unsupported platform variants, and missing checksums.
Extraction of a .pkg payload is not proof that its installer behavior works.
There is no Brew fallback.

Start with three working representatives: one app archive, one standalone
executable, and one app with an extra CLI link. Pick tokens from the pinned
metadata during implementation. Raycast and Cursor are candidates, not promises.
Use an installer-heavy entry as an exclusion check. Expand only when a package
can be supported without restoring another package engine.
Keep import-from-derivation disabled for the Cask source.

Alternative rejected: write a full Cask interpreter or bottle installer.
That would transfer Brew's lifecycle work into pkg.

### D7. Derive macOS launchers from intact store bundles

Use mac-app-util's standalone tools through a Nix-managed helper profile.
The helper profile is separate from the user's package profile:

    ${XDG_STATE_HOME:-$HOME/.local/state}/nix/profiles/pkg-app-tools

Pin the helper package reference in the client release. The helper profile
uses native roots. It is implementation support, not a second user inventory.
Only create it when macOS app exposure is needed. Removing or rolling back a
user app must not remove the helper.

Keep the app bundle intact in its original store output.
Use `~/Applications/pkg/` as the only launcher destination.
Refuse a pre-existing destination that is not established as pkg-owned.
Do not hand a directory-replacing sync tool the full Applications directory.
The ownership marker protects launcher deletion; it is not package state.

Enumerate active package outputs from profile metadata. Build a temporary
view of their intact Applications bundles for the launcher tool.
Refresh launchers after install, remove, upgrade, and rollback.
Use `pkg apps sync` for an explicit retry. Doctor reports drift without fixing it.
App preferences, accounts, and databases remain outside rollback.
If an app requires a writable installation or a native helper, exclude it.

Check the exact helper version, behavior, and distribution notices at integration.
If that helper cannot meet the stated behavior, revise this integration choice
in OpenSpec. Do not silently add a home-grown app launcher engine.

Alternative rejected: use the old per-file activation forest for .app bundles.
It does not establish intact signed-app or desktop integration behavior.

### D8. Delete old machinery and old tests together

| Area | Action |
| --- | --- |
| pkg-cli | Keep the crate and useful output. Replace command coordination, path rules, progress, errors, and source selection. |
| pkg-nix | Move only the small needed process/JSON logic into pkg-cli. Delete the large adapter trait, framing, broker, managed runtime, build authority, maintenance, and root helpers. |
| pkg-core | Move only necessary local types. Delete product generations, journal, pin state, approval, and catalog trust types. |
| pkg-store | Delete activation, journal, current pointer, roots, leases, and product GC. |
| pkg-pipeline | Delete preparation, acquisition policy, custom commits, recovery, repair, and approval. |
| pkg-installer | Delete pkg-install, pkg-nix-broker, pkg-root-helper, platform services, accounts, handoffs, and asset repair. |
| pkg-channel | Delete TUF and product-channel verification/publication use. Native Nix verification remains. |
| pkg-index and pkg-resolver | Replace with catalog name routing and evaluated metadata. Delete the old resolver and central index protocol. |
| pkg-macos-security | Delete privileged helper authentication with its callers. Use the selected external launcher tool for apps. |
| pkg-testkit | Delete the old fake engine and fixtures. Keep only small direct fixtures if still useful. |
| tools/release | Replace custom channel and machine asset assembly with client archive packaging. Remove the Rust crate if no live work remains. |
| install.sh and tools/install | Replace the old all-in-one installation entry with client-only setup guidance or a small archive downloader. It must never provision Nix. |
| CI and proof tools | Remove old test-contract mutations, machine install/uninstall matrices, TUF fixture publication, repeat-proof, index-determinism, and old quality ratchets when their subject is deleted. |

Do not port tests that enforce removed behavior. Delete tests and their
production-only seams with that behavior. Do not make test totals or coverage
parity an acceptance criterion.
Keep ordinary compile, format, lint, dependency, and documentation checks.
Keep only focused checks for new parsing, argument handling, and real user journeys.
No new fault matrix, fuzz campaign, mutation framework, or permanent evidence service.

### D9. Replace release and planning authority

Deliver one client archive per supported target, with completions, notices,
and a checksum. Use normal repository release integrity.
Do not ship a vendor Nix runtime, privileged service, installer package,
custom TUF repository, or central package index.
Let Nix fetch application payloads from their vendor sources. Do not mirror
all proprietary apps as part of this change.

Keep the current rust-toolchain pin while rewriting. It is not the source of
the package-engine complexity. Reduce stale lint exceptions and custom quality
ratchets with their deleted code rather than adding a toolchain migration.

This change is the future design authority. The previous verification and
production trust changes are superseded, not delivered.
Do not require offline TUF key custody, hosted package channel infrastructure,
installer signing, or exact whole-machine proofs for the new client.
Handle standard client signing if the chosen distribution requires it.
Do not claim a production certification merely because the plan exists.

## Risks / Trade-offs

- **Experimental profile interfaces** → Keep Nix details in one module.
  Check the required behavior on the supported runtime range.
- **Cold native search** → Label progress and cache display data.
  Measure before adding a metadata service.
- **Mutable source between search and install** → Display both discovery
  revision and actual installed resolution. Do not promise a frozen preview.
- **Reduced package trust policy** → Keep explicit source selection, native
  store checks, download hashes, and disabled source-provided Nix settings.
  A hash is not publisher identity or approval.
- **Profile success followed by launcher failure** → Report partial completion.
  Retry only derived launchers with apps sync.
- **App path and signature limits** → Support only the working subset.
  Do not advertise all upstream Casks.
- **Interrupted Nix operation** → Forward the signal, reap, and re-read the
  profile. Report uncertainty when state cannot be read. Do not blindly retry.
- **Old installations on developer machines** → Start new state and leave old
  files alone. Use clean or disposable hosts for replacement checks.

## Migration Plan

There is no data migration. Implement on a branch from the recorded main revision.
Replace the CLI directly. Delete the old engine after its callers are replaced.
Do not keep dual runtime modes to support old users.

Fresh setup installs Determinate through the vendor, installs the new client,
and selects packages into the new native profile.
The client neither adopts nor erases the old package state.
Client updates replace the executable and keep the native profile.
Client removal leaves Nix and package state intact.

A source rollback is a Git revert. Reinstalling the old client does not make it
understand the new native profile. Use native Nix to inspect that profile.
Do not call this backward-compatible rollback.

## Completion Evidence

Keep the checks small:

1. Build the shipped client on both supported systems.
2. Run install, list, upgrade, rollback, remove, and prune against controlled
   source revisions. Check dedicated profile roots and default-profile isolation.
3. Exercise one source failure, one native conflict, and one interruption.
   Confirm clear output and readable native state.
4. Run the three accepted Cask representatives through the same profile.
   Check app launchers, CLI entry points, removal, and launcher retry on macOS.
5. Install the actual archive on clean hosts with external Nix.
   Confirm that no pkg service, helper, Brew runtime, or old state is needed.

These are implementation checks, not a requirement to rebuild the old test suite.
They have not been executed by this planning change.

## Evidence

- [Initial upstream research](../../../docs/research/2026-09-27-upstream-simplification.md).
- [Casks without Brew](../../../docs/research/2026-09-27-homebrew-casks.md).
- [Nix profiles](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile).
- [Profile layout and roots](https://github.com/NixOS/nix/blob/2.35.2/doc/manual/source/command-ref/files/profiles.md).
- [Native upgrade](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-upgrade).
- [History pruning](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-wipe-history).
- [Inspected brew-nix converter](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/casks.nix).
- [mac-app-util](https://github.com/hraban/mac-app-util).
- [Determinate runtime](https://docs.determinate.systems/determinate-nix/).

No unresolved product decision blocks the task list. Runtime command details,
lock revisions, and representative Cask tokens are bounded implementation
choices. A finding that contradicts this design requires a documented design
change, not an undocumented compatibility layer.
