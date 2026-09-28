# Rust Cask catalog implementation tasks

These tasks implement the [design](design.md). All tasks are open.
Implementation is delegated; verification on real hosts is split:
Linux checks run in the E2B sandbox (headless; no GUI launch claims),
macOS checks run in the parent's Tart VM. Keep groups in this order;
each group is one reviewable work unit and the notes state
dependencies.

No task may run a vendor installer, install Nix on a Mac, execute
Homebrew, or ship the maintainer tool in the client archive.

## 1. RC-01: Workspace scaffolding and packaging isolation

- [ ] 1.1 Add `tools/cask-catalog` as a workspace member with a minimal CLI (`fetch`, `generate`, `self-test`) and doc comments; verify the pinned toolchain, lint set, and `deny.toml` pass for the whole workspace.
- [ ] 1.2 Fix `tools/release/package_client.sh` license collection (owned by this group for the change): traverse only the resolve graph reachable from `pkg-cli` for the packaged target — seed from the `pkg-cli` package id in the platform-filtered `cargo metadata` and walk `deps` edges (or use `cargo tree -p pkg-cli --target <triple>`) — excluding every workspace member and dependency not in that graph, so generator dependencies and licenses cannot enter the client archive; add a check that the archive contains no `cask-catalog` binary, license text, or catalog data.
- [ ] 1.3 Add the catalog license and attribution file (`nix/casks/lib/README.md` or equivalent) recording brew-nix's MIT license, the ported revision, and the Homebrew data license; verify a reviewer can find every upstream origin from it.
- [ ] 1.4 Add the CI regeneration check job placeholder wiring (job may fail until RC-03 commits data); verify CI stays green on non-catalog paths until then.

## 2. RC-02: Generator core (depends on RC-01)

- [ ] 2.1 Implement `fetch`: download `cask.json` at an exact revision, verify SHA-256, and write `nix/casks/catalog/input.json` (url, revision, sha256, license attribution); verify a wrong hash aborts without writing the pin.
- [ ] 2.2 Implement the accepted input schema with `supported_platforms` (and the other required fields) mandatory: a snapshot missing a required field family fails generation atomically with no output replacement; a record missing a required field is excluded as `malformed-record`; verify both fail-closed paths with focused tests.
- [ ] 2.3 Implement the per-target effective-record merge with wholesale variation replacement including the artifacts array; verify against the research-note examples (sha-only variation, artifacts-replacing variation) with focused unit tests.
- [ ] 2.4 Implement eligibility ordering and the bounded reason vocabulary from the design, with OS support decided only from `supported_platforms` and `depends_on` constraints — never from variation keys or artifact kinds; verify each reason with one minimal record fixture, including disabled/deprecated and minimum-os against the declared baseline.
- [ ] 2.5 Implement the typed plan builder: apps, binaries with renames, completions, manpages, archive kind, resolved cask depends, minMacos; verify multi-app and renamed-CLI records produce complete plans with no token-specific branches.
- [ ] 2.6 Implement plan safety validation: anchor normalization, absolute/traversal/unresolved-expansion rejection, link-name hygiene; verify each rejection names the field and excludes only that token.
- [ ] 2.7 Implement cask dependency closure resolution with cycle detection; exclude records with any Homebrew formula dependency (`formula-dependency` reason, no guessed nixpkgs attributes, no committed formula map); exclude records whose declared dependency implies vendor integration generic builders cannot provide (`cask-dependency-integration`); verify each case with focused tests.
- [ ] 2.8 Implement whole-catalog integrity checks (duplicate tokens, conflicting versions, pin hash mismatch, missing required field families) failing atomically with no output replacement; verify no partial catalog file is left behind on failure.
- [ ] 2.9 Implement deterministic serialization and provenance embedding; verify byte-identical output across runs and machines, including sorted keys and no timestamps.
- [ ] 2.10 Add the generator's focused test suite over the seams above; keep fixtures small and real-shaped; no mock campaign.

## 3. RC-03: Generated data and the macOS Nix source (depends on RC-02)

- [ ] 3.1 Run `fetch` + `generate` against the pinned brew-api revision and commit `nix/casks/catalog/{input.json,catalog.json}`; record input revision, size, and coverage counts in the change description.
- [ ] 3.2 Rewrite `nix/casks/flake.nix` to read the committed catalog with `builtins.fromJSON`, keep `nixpkgs` as the only input, and expose `packages.<system>.<token>` lazily plus the single `catalogIndex` envelope and `catalogStatus`; verify evaluation with import-from-derivation disabled.
- [ ] 3.3 Implement `buildAppArchive` (port from brew-nix `casks.nix`: archive dispatch, bundle placement, binary linking, wrapper) driven only by plan data with `escapeShellArg`-quoted interpolation; verify an app+CLI token builds and the CLI link name comes from the plan.
- [ ] 3.4 Implement pre-write path and link validation (members stay inside the staging root; vendor links resolve in-tree, allowing legitimate in-tree `..` chains) plus the post-install verification distinguishing vendor payload links from builder-created links into `$out` or declared Nix store inputs; verify an escaping member fails before any write and a wrapper link to a declared input is accepted.
- [ ] 3.5 Implement the generic build-time Info.plist minimum check (major/minor comparison against the declared baseline); verify a too-new bundle fails with a clear message naming the required version.
- [ ] 3.6 Implement `buildPkgPayload` with the structural relocatable-layout rule (payload-only xar; no scripts, plugins, or choice-requiring distribution; no system Library files, services, drivers, or privileged helpers; no silent dropping of essential payload); verify a scripts-carrying pkg and a system-payload pkg both fail without invoking any installer, and a pure relocatable payload extracts and installs.
- [ ] 3.7 Implement completion/manpage linking from plan data; verify a plan without them builds unchanged and a plan with missing files fails naming the file.
- [ ] 3.8 Turn on the CI regeneration check; verify it passes on the committed data and fails on a tampered catalog or pin.

## 4. RC-04: Linux builders and dependencies (depends on RC-03)

- [ ] 4.1 Implement `buildBinary` with the static passthrough and Nixpkgs `autoPatchelfHook` against the bounded generic runtime-library closure maintained in `builders.nix`; verify a static binary runs unchanged, a dynamic binary in the closure runs against store paths with no host `/usr` or `/lib` dependence, and an out-of-closure library fails naming the soname.
- [ ] 4.2 Implement `buildAppImage` using the pinned Nixpkgs `appimageTools.wrapType2` (API verified in the locked nixpkgs revision); no hand-rolled extraction environment tricks and no blanket auto-patching over the image; verify the built launcher runs a real AppImage; claim runnability only for what was executed.
- [ ] 4.3 Make cask dependencies concrete at runtime: the client expands installs to the full resolved cask closure in one profile operation, and wrapped binaries get explicit wrapper paths over declared dependency outputs; verify one `nix profile add` installs a token with its closure and the dependency binaries are on the profile path.
- [ ] 4.4 Verify lazy isolation on Linux: building one token forces only its plan; the index evaluates with no builds; one unbuildable record never fails other requests.

## 5. RC-05: Client integration (depends on RC-03; parallel with RC-04)

- [ ] 5.1 Replace `cask.rs` classification with envelope lookups; delete `CASK_SYSTEM` and `casks_supported_on`; read `targets` from the envelope before any per-system data so an untargeted system reports platform-skipped without evaluating a nonexistent attribute.
- [ ] 5.2 Add the `pkg-cask-catalog/2` envelope decode with the strict schema gate and the documented null rules in `manifest.rs`; regenerate the fixture from real generator output; verify an unknown schema fails naming both schemas.
- [ ] 5.3 Route cask search through the cheap envelope eval with local filtering via the declared `regex` dependency (Rust regex grammar, documented; Nixpkgs lane unchanged), reusing the existing cache, source identity, and stale logic; verify a broad search forces no derivations and one bad record cannot fail the query set.
- [ ] 5.4 Point `info` and `install` resolution at `packages.<system>.<token>`, keep `gate_cask` on generated status, and expand installs to the resolved cask closure; verify excluded and unknown tokens are refused with reasons and eligible tokens install through the ordinary profile path.
- [ ] 5.5 Verify the native lifecycle end to end after the change: original moving references, upgrade re-resolution, rollback, and launcher sync behave exactly as before; no Homebrew cleanup hooks run.

## 6. RC-06: Removal and documentation (depends on RC-03, RC-05)

- [ ] 6.1 Delete the brew-nix flake input, the curated token and override machinery, and the old `caskSupport` output; verify the flake lock no longer carries brew-nix or brew-api and attribution remains per RC-01.
- [ ] 6.2 Delete the `pkg-cask-support/1` decode path and old fixture; verify no production or test code references the old schema or `CASK_SYSTEM`.
- [ ] 6.3 Rewrite `docs/casks.md`, the catalog update procedure, the search grammar documentation, and any PRODUCT/CONTEXT text naming the curated set or macOS-only casks; verify docs describe the generated catalog, both targets, and the honest eligibility distinction.
- [ ] 6.4 Update the docs-linkcheck expectations and CONTRIBUTING pointer if the active change link must move; verify the docs-linkcheck workflow is green.

## 7. RC-07: Verification and evidence (depends on RC-04, RC-05, RC-06)

- [ ] 7.1 Run the full-catalog evaluation on both systems; record total, eligible, and excluded-by-reason counts as eligibility coverage, explicitly not a compatibility promise, in `docs/verification/`.
- [ ] 7.2 Linux (E2B, headless): real journeys with a real generated catalog — search, info, install of one binary and one AppImage, CLI execution, upgrade across one catalog revision, remove; verify runnable behavior only and make no GUI launch claims; record systems, revisions, outcomes.
- [ ] 7.3 macOS (parent Tart VM): install one app+CLI token and one binary-only token; verify app launch, CLI run, and `codesign --verify --deep --strict` on the intact bundle; record outcomes.
- [ ] 7.4 Verify one exclusion honestly: an installer-carrying token and a too-new-macOS token (hidden-plist case included via the build check) are refused with their recorded reasons in `info` and `install`.
- [ ] 7.5 Confirm release integrity: package the client on both systems with the RC-01 collector fix and verify the archive contains no generator binary, no maintainer-tool license texts, and no catalog data.
- [ ] 7.6 Record known limits and the pending observational probes (the `1password-cli` binary and `koreader` AppImage runs in the second sandbox) in the verification note when results arrive; do not mark tasks complete beyond the evidence.
