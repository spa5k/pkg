# Rust Cask catalog implementation tasks

These tasks implement the [design](design.md) under the binding
[contract](contract.md). A box is checked only when the item is
implemented AND verified with recorded evidence (see
[docs/verification/2026-09-28-rust-cask-catalog.md](../../../docs/verification/2026-09-28-rust-cask-catalog.md)).
Unchecked boxes are open work or verified-partial work; notes say
which. Keep group order; each group is one reviewable unit.

No task may run a vendor installer, install Nix on a Mac, execute
Homebrew, or ship the maintainer tool in the client archive.

## 1. RC-01: Workspace scaffolding and packaging isolation

- [x] 1.1 Add `tools/cask-catalog` as a workspace member with a minimal CLI (`fetch`, `generate` — there is NO `self-test` command; focused `cargo test` suites are the checks) and doc comments; the pinned toolchain, lint set, and `deny.toml` pass for the whole workspace. Evidence: 81 tests, fmt/clippy/doc/deny green.
- [x] 1.2 Package only the dependency graph reachable from the client. Both full-workspace packaging runs passed: 49 client license sets on macOS, 50 on Linux, exactly one client binary, and no generator, catalog, or generator-only licenses.
- [x] 1.3 Add the catalog license and attribution file (`nix/casks/lib/README.md`) recording brew-nix's MIT license, the ported revision `16131ae4126c54b1502aa7eaf6573d7fbf16b656`, and the Homebrew data license. Evidence: file present and reviewed.
- [x] 1.4 Verify catalog regeneration on GitHub CI. The [regenerate-and-compare job](https://github.com/spa5k/pkg/actions/runs/36454702967) passed on implementation commit `f97c860`.

## 2. RC-02: Generator core (depends on RC-01)

- [x] 2.1 `fetch`: downloads `cask.json` at an exact revision, verifies SHA-256 before replacing anything, writes `nix/casks/catalog/input.json` (url, revision, sha256, license). Flags: `--revision --repo --sha256 --cache-dir --pin`. Evidence: committed pin + tests.
- [x] 2.2 Accepted input schema with `supported_platforms` required: snapshot missing the field family fails atomically; one record missing it is excluded `malformed-record`. Evidence: emit/classify tests, both fail-closed paths covered.
- [x] 2.3 Per-target effective-record merge with wholesale variation replacement including the artifacts array. Evidence: focused unit tests over research-note examples.
- [x] 2.4 Eligibility ordering and bounded reason vocabulary; OS support only from `supported_platforms` and `depends_on`. Evidence: per-reason fixtures in classify tests; committed catalog shows exactly the documented reasons.
- [x] 2.5 Typed plan builder: apps, binaries with renames, completions, manpages, archive kind, minMacos; no token-specific branches. Evidence: plan tests + committed catalog plans.
- [x] 2.6 Plan safety validation: anchors, absolute/traversal/unresolved-expansion rejection, link-name hygiene; each rejection names the field and excludes only that token. Evidence: plan tests.
- [x] 2.7 Every record with any formula dependency excluded (`formula-dependency`); every record with any cask dependency excluded (`cask-dependency-integration`); no closure engine, no depends output. Evidence: focused tests.
- [x] 2.8 Whole-catalog integrity (duplicate tokens, unusable keys, empty/non-array snapshot, pin hash mismatch) failing atomically; failed publish leaves the previous catalog untouched. Evidence: emit tests including read-only-directory case.
- [x] 2.9 Deterministic serialization and provenance; byte-identical output, sorted keys, no timestamps. Evidence: local byte-identical regeneration recorded.
- [x] 2.10 Focused generator test suite over these seams; fixtures small and real-shaped (one real-catalog fixture regenerated from actual output). Evidence: 81 tests green, final review strictness included (malformed dependency/option/variation shapes reject; duplicate artifact targets reject; macOS constraints validated on all targets while baseline comparison and minMacos apply only to aarch64-darwin, never Linux).

## 3. RC-03: Generated data and the macOS Nix source (depends on RC-02)

- [x] 3.1 Run `fetch` + `generate` against the pinned brew-api revision and commit `nix/casks/catalog/{input.json,catalog.json}`; revision, size, and counts recorded in the dated verification note (not duplicated here).
- [x] 3.2 (Nix owner) `nix/casks/flake.nix` reads the ONE committed `catalog.json` with `builtins.fromJSON`, keeps `nixpkgs` as the only input, derives `catalogIndex` and `catalogStatus`, exposes `packages.<system>.<token>` lazily. Evidence: both outputs evaluate over all 7709 records with IFD disabled.
- [x] 3.3 Generic archive builder (brew-nix port: archive dispatch, bundle placement, binary linking) driven only by plan data through the stdlib-Python helper; vendor strings never reach the shell. Evidence: Cursor 3.17.19 app+CLI built, installed, CLI ran, codesign deep-strict valid.
- [x] 3.4 Pre-write member and link validation plus post-install verification distinguishing vendor links from builder links. Evidence: B11 fixture checks pass.
- [x] 3.5 Read the bundle minimum macOS version without changing its bytes. Raycast 1.104.25 requires 13.0 and builds on the 15.7.7 baseline. The macOS 26.0 plist fixture is refused with a clear version error.
- [x] 3.6 `buildPkgPayload` structural rule (payload-only xar; no scripts, plugins, nested pkgs, system payloads; app-bundle layouts only — plain files rejected; no silent drops). Evidence: 13 Nix fixture checks plus the Python archive regressions; the final generator review also rejects the twelve choice-requiring pkg records as `installer-script` at generation, and `meta-quest-remote-desktop` as `unsupported-container`.
- [x] 3.7 Link completions and manpages from plan data. The real Nix app fixture checks each output and the renamed CLI path. The complete builder suite passed 13 checks.
- [x] 3.8 Confirm the GitHub regeneration job passes against the committed pin and data. Local byte comparison and the [GitHub job](https://github.com/spa5k/pkg/actions/runs/36454702967) passed.

## 4. RC-04: Linux builders and dependencies (depends on RC-03)

- [x] 4.1 Build static and dynamic Linux binaries. op 2.39.0 ran. A dynamic zlib fixture had its host loader and rpath replaced with store paths and ran. A missing-soname fixture failed and named its required library.
- [x] 4.2 `buildAppImage` via pinned Nixpkgs `appimageTools.wrapType2`; no hand-rolled extraction tricks. Evidence: KOReader AppImage `--help` printed `v2026.07.1` (headless run only).
- [x] 4.3 Refuse formula and cask dependencies through the actual client gate. Beutl was refused with formula-dependency. Acronis True Image was refused with cask-dependency-integration. No dependency expansion exists.
- [x] 4.4 Evaluate the full index without builds or IFD. Install only the requested Linux op and KOReader plans from the full catalog. Other plans remain unforced.

## 5. RC-05: Client integration (depends on RC-03; parallel with RC-04)

- [x] 5.1 Cask classification replaced with envelope lookups; `CASK_SYSTEM` and `casks_supported_on` deleted; `targets` read before per-system data. Evidence: full client suite green after the change.
- [x] 5.2 `pkg-cask-catalog/2` envelope decode with the strict schema gate and documented null rules in `manifest.rs`; fixture regenerated from real generator output; unknown schema names both schemas. Evidence: tests green on the real fixture.
- [x] 5.3 Catalog search through the cheap envelope eval with local Rust-regex filtering (declared `regex` dependency); existing cache/stale rules reused; no derivations forced; one bad record cannot fail the query set. Evidence: macOS `pkg search` journey passed; 7709-record index eval without IFD.
- [x] 5.4 `info`/`install` resolve `packages.<system>.<token>`; `gate_cask` on generated status; excluded (Zoom, `installer-script`) and unknown tokens refused. Evidence: Cursor + 1password-cli installs passed.
- [x] 5.5 Native lifecycle end to end: moving references, upgrade re-resolution, rollback, launcher sync. Evidence: list/update/upgrade/rollback/remove/apps-sync journey on the macOS VM, including the controlled version-label probe with vendor bytes preserved.

## 6. RC-06: Removal and documentation (depends on RC-03, RC-05)

- [x] 6.1 Delete the brew-nix flake input and curated token/override machinery; only `nixpkgs` remains; attribution retained in `nix/casks/lib/README.md`. Evidence: flake.nix has exactly one input.
- [x] 6.2 Delete the `pkg-cask-support/1` decode path and old fixture; no production or test code references the old schema or `CASK_SYSTEM`. Evidence: full test suite green; no references remain.
- [x] 6.3 Rewrite cask, command, install, product, and context documentation. The detailed HTML plan, update procedure, and index semantics match the final implementation. Local documentation links pass.
- [x] 6.4 Docs-linkcheck expectations updated and green. Evidence: docs-links check passed with this change's links.

## 7. RC-07: Verification and evidence (depends on RC-04, RC-05, RC-06)

- [x] 7.1 Full-catalog evaluation on both systems; total/eligible/by-reason counts recorded as eligibility coverage (not a compatibility promise) in the dated verification note.
- [x] 7.2 Run the full Linux client journey in E2B with strict shell error handling: search, info, install, execution, update, upgrade, rollback, removal. A controlled catalog version-label change selected a new output while preserving the original reference. Rollback restored the old output. The final profile was empty. KOReader was checked headless.
- [x] 7.3 macOS (parent Tart VM): app+CLI token (Cursor 3.17.19) and binary-only token (1password-cli 2.39.0) installed; app process launched; `cursor --version` ran; `codesign --verify --deep --strict` passed. GUI interaction not verified (recorded as such).
- [x] 7.4 Verify actual installer, formula-dependency, cask-dependency, and unknown-token refusals. Verify generation-time minimum constraints and the build-time macOS 26.0 plist refusal. No vendor installer runs.
- [x] 7.5 Package the client on both systems. The archives contain one client binary, completions, notices, and only client dependency licenses. No generator binary, generated catalog, or generator-only license sets are present.
- [x] 7.6 Record final coverage, strict client journeys, signed macOS bundles, gzip and native pbzx package fixtures, Linux dynamic linking, archive-path checks, and packaging in the dated verification note. State the limits: metadata eligibility is not universal compatibility, and Linux GUI use was not tested.

## 8. RC-08: Pre-merge cask stress checks

Evidence: [stress report](../../../docs/verification/2026-09-29-cask-stress.md).

- [x] 8.1 Pin a real Homebrew popularity response. Report the top 1,000 tokens without replacing missing records. Check the full catalog and evaluate every eligible derivation without app downloads or import-from-derivation.
- [x] 8.2 Exercise real catalog plans with tiny synthetic payloads. Separate successful plan execution, package exclusions, skipped formats, and vendor compatibility claims.
- [x] 8.3 Reproduce and fix generator, launcher-name, and archive defects. Retain focused regressions. Run archive checks on Linux and macOS.
- [x] 8.4 Adapt relevant, pinned Homebrew test scenarios with license attribution. Execute the adapted cases against this implementation. Record upstream behavior that is outside this project's scope.
- [x] 8.5 Verify an independent source's metadata on both targets. Record the public-tap extension contract, source identity, collision policy, and unsupported raw Ruby input.
- [x] 8.6 Record the results, costs, limits, and remaining risks. Run lint and the required project checks. Keep this PR open for review.
