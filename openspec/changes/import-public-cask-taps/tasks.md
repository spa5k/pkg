# Implementation tasks

All tasks are open. This change contains planning documents only.
Baseline: `f6f99a748653861302876dfe127dfc6bf498d323`.

## 1. RT-01 — prove the upstream export contract

- [ ] 1.1 Pin Homebrew, Ruby, dependencies, runner recipes, and adapter version. Record licenses. Build clean Linux and macOS tool environments without tap or model credentials.
- [ ] 1.2 Export small official and synthetic casks through the actual upstream loader. Verify interpolation, arch/OS branches, language policy, helpers, renames, URL options, dependencies, and platform evidence.
- [ ] 1.3 Prove native target contexts. Record why Homebrew simulation alone cannot establish arbitrary Ruby host compatibility.
- [ ] 1.4 Detect payload-dependent metadata without app downloads. Demonstrate the staging guard on a real-shaped directory enumeration and a simple non-staging cask.
- [ ] 1.5 Preserve legacy and structured install actions. Show that unknown or required unsupported actions cannot become eligible through lossy serialization.
- [ ] 1.6 Prove network, filesystem, process, time, memory, output, and disk limits. Verify that raw export runners contain no model or publication credentials. Record blockers before broader implementation.

## 2. RT-02 — source registry and captured exports (PR A)

- [ ] 2.1 Add the source registry, exact lock, source identity validation, public HTTPS Git fetch, and canonical tree hashes.
- [ ] 2.2 Discover casks in sorted order. Reject duplicate tokens, escaping paths, and unpinned submodules. Retain local helper files from the pinned source tree.
- [ ] 2.3 Add `pkg-cask-source/1` capture with per-file provenance, target context, stable error codes, complete inventory, and content hashes.
- [ ] 2.4 Keep JSON and Ruby inputs behind one source interface. Reuse the existing classifier and artifact planner.
- [ ] 2.5 Cache by whole source/runtime/target/policy identity. Invalidate helper-only changes. Distinguish captured-byte determinism from arbitrary Ruby execution.
- [ ] 2.6 Verify per-record exclusions, source-wide failures, bounded diagnostics, and atomic runtime-catalog replacement. Keep the default published catalog unchanged in PR A.

## 3. RT-03 — schema and identity cutover (PR B)

- [ ] 3.1 Emit `pkg-cask-catalog/3` with `inputs`, qualified entry IDs, separate token/source fields, and origin paths/hashes.
- [ ] 3.2 Expose qualified IDs as single quoted Nix attributes. Use distinct stable store names and detect hash-suffix collisions.
- [ ] 3.3 Update the client schema decoder, source display, ID parser, and exact-token ambiguity handling together. Never use source order as precedence.
- [ ] 3.4 Verify qualified install/info, search caching, same-token source ambiguity, source removal, native update/rollback, and preservation of the moving generated-source reference.
- [ ] 3.5 Remove schema-2 decoding, obsolete fixtures, the old `input.json` runtime assumptions, hardcoded mirror/hash defaults, and the mirror-only CI assertion. Retain the JSON adapter through the shared interface.
- [ ] 3.6 Extend the existing regeneration and focused regression checks for source identities. Keep catalog search free of builds and IFD.

## 4. RT-04 — pilot and default-source cutover (PR C)

- [ ] 4.1 Select and pin the official Cask tap plus at least three public taps with clear source/license records. Include a helper case and an intentionally excluded active-step case.
- [ ] 4.2 Compare inventories and normalized records against the pinned Homebrew exporter. Record every parse failure, exclusion, and missing record by source/target.
- [ ] 4.3 Compare the previous top-1,000 ranking against the new source. Evaluate all eligible derivations without IFD and replay compatible plans with synthetic payloads.
- [ ] 4.4 Run the bounded native package sample on macOS and Linux. Cap new vendor downloads at four. Separate GUI limits from command and lifecycle checks.
- [ ] 4.5 Publish the raw official-source catalog only after the acceptance gates pass. Commit registry, lock, captured exports, attribution, and catalog together. Retire the default mirror reference.
- [ ] 4.6 Document independent catalog publishing, adding a tap by configuration, unsupported Ruby behavior, failure recovery, and source-qualified install examples.
- [ ] 4.7 Run Rust, Python/Ruby/Nix lint as applicable, strict OpenSpec, docs links, and normal CI. Record actual coverage and costs. Use E2B GLM 5.3 for implementation and parent verification for acceptance.
