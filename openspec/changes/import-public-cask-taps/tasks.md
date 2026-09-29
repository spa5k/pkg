# Implementation tasks

Status: implementation and local verification complete (2026-09-29).
Delivered in [PR #82](https://github.com/spa5k/pkg/pull/82). Not merged
or released; do not merge or release without a later instruction. The
[implementation contract](implementation-contract.md) is authoritative.
Evidence below is summarized; full records are in
[docs/verification/public-taps.md](../../../docs/verification/public-taps.md)
and the immutable baseline commit
<https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/docs/verification/public-taps-2026-09-29/results.md>.

## 1. PT-01 — backend importer library (backend worker)

- [x] 1.1 Agree and publish the exact `cask_catalog::tap` Rust signatures and the schema-3 envelope with the client worker.
- [x] 1.2 Provide the pinned Nix environment: native Ruby and the upstream Homebrew reader dependencies, pinned together with recorded licenses.
- [x] 1.3 Implement the credential-free public fetch: canonical resolution, redirect and destination policy with a real URL parser, no private-network destinations, no hooks, filters, or submodules, whole-tree hashing with source-relative file provenance.
- [x] 1.4 Run the pinned reader inside an ordinary sandboxed Nix derivation: not fixed-output, sandbox enforced, no sandbox fallback or relaxed mode, no user home, secrets, sockets, or model credentials, bounded execution and output limits.
- [x] 1.5 Capture metadata with per-record outcomes: eligible entries for the generic subset, explicit exclusions with stable codes, callbacks and structured steps preserved as metadata for records that load.
- [x] 1.6 Classify offline in Rust from captured bytes only; emit `pkg-cask-catalog/3` with `inputs` provenance, qualified identities, and the one-pass escaped attribute; generate the flake into the caller-owned staging directory; never publish the client registry.
- [x] 1.7 Retain the official JSON source, regenerated in schema 3 with source `homebrew/cask`.

## 2. PT-02 — client tap commands and consent (client worker)

- [x] 2.1 Implement `pkg tap add/list/update/remove`.
- [x] 2.2 Gate import behind first consent: canonical repository and approval scope shown, interactive consent remembered per origin, `--trust` required noninteractively, no Ruby when consent is refused.
- [x] 2.3 Keep search of saved catalogs free of prompts and Ruby; bare tokens resolve only when unique counting excluded matches; qualified IDs resolve only within their source; unknown qualified sources instruct the explicit add flow, never reinterpretation as Nix code.
- [x] 2.4 Validate importer results and publish atomically into the client registry; failed refreshes preserve the previous source bytes; `remove` blocks future source use without uninstalling packages.
- [x] 2.5 Preserve the native profile lifecycle: stable generated flake references for updates, install/upgrade/rollback/removal through the native profile, old generation outputs preserved.
- [x] 2.6 Add focused client tests for consent states, registry persistence, ambiguity rules including excluded-entry collisions, atomic refresh failure recovery, and the unreadable-saved-catalog rule.
- [x] 2.7 Publish each saved source into a stable real directory (never a symlinked flake root) and swap it by atomic directory exchange with no two-rename fallback.

## 3. PT-03 — schema and identity cutover

- [x] 3.1 Switch generator, Nix projection, and client decoder to schema 3 together; delete the schema-2 decoder, its fixtures, and single-input runtime assumptions.
- [x] 3.2 Verify qualified install/info/search, same-token coexistence, excluded-entry ambiguity, identity-stable upgrades, and escaped-attribute installables.
- [x] 3.3 Confirm the retained official source publishes schema 3 and ordinary search reads the cheap index without forcing derivations or IFD.
- [x] 3.4 Add an encoded-attributes test: dots and underscores map to distinct reversible escaped physical attributes (`tool@1.2` -> `tool@1_d_2`), catalog and search keys stay original, profile labels decode.

## 4. PT-04 — sandbox and safety proofs on real native hosts

- [x] 4.1 Prove denied network from the Ruby derivation on both hosts with behavior probes.
- [x] 4.2 Probe denial of host-home reads and a supplied credential sentinel; sentinel bytes never appear in captured output.
- [x] 4.3 Prove output-exhaustion containment: bounded diagnostics, bounded result size, no successful empty catalog on timeout or crash.
- [x] 4.4 Prove helper-only commit invalidation and whole-tree cache identity; prove refused consent and noninteractive-without-`--trust` run no Ruby.
- [x] 4.5 Verify no sandbox fallback or relaxed mode is accepted, no host paths are added, and no credentials reach public fetches; record effective daemon isolation per platform.
- [x] 4.6 Record the macOS daemon-isolation evidence with the required configuration and implement the fail-closed gate in the product.

## 5. PT-05 — adversarial fixtures and evidence

- [x] 5.1 Execute the `tools/public-tap-check` fixture matrix and metadata-only expectations in a clean verification sandbox; record actual per-case outcomes. Stage scope: reader stage ran all 20 cases per OS; converter stage closed by the seven capture-case regressions, backend tests, and the seven-source whole import; the installer-script and branch fixture whole-imports and the native two-tree collision import stay explicit limits.
- [x] 5.2 Re-run the existing `tools/cask-build-check` plan and catalog replay checks against schema-3 output; reuse, do not duplicate.
- [x] 5.3 Exercise the seven real pinned public taps in `public-sources.json` (30 casks, metadata-only); record every actual entry result; do not force eligibility.
- [x] 5.4 Run the bounded native sample through install, use, update, and rollback on both platforms; separate GUI limits from command and lifecycle checks.
- [x] 5.5 Fill the verification record with actual evidence, commands, hashes, and limits; replace every UNRUN marker.
- [x] 5.6 Expand Linux verification with real downloads from three public taps: four apps, five vendor files, real commands, multiple packages, an upgrade, and rollback. Fix compressed manual pages, binary AppImage routing, and shell completion names; verify Bash, Zsh, and Fish completion loading.

## 6. PT-06 — review, lint, and delivery

- [x] 6.1 Run Rust, Ruby, Python, Nix, and workflow lint for changed files; run strict OpenSpec validation, docs checks, and normal CI.
- [x] 6.2 Reconcile Cargo.lock and integration; parent review and independent verification of code and integration. Done: the parent review is complete and all requested fixes are in.
- [x] 6.3 Open the single reviewable implementation PR; do not merge or release without a later instruction. Delivered in [PR #82](https://github.com/spa5k/pkg/pull/82).
