# Cask and Nix reliability review — 2026-09-30

The failures have different causes. The main reliability gap is the boundary
between the CLI, native Nix, vendor payloads, and the host OS. The current CI
does not run a real install or build the macOS launcher helper. Existing tests
passed while this review found defects through native operations.

The reported Stats failure was an SBCL/macOS 27 compatibility fault. It was not
evidence that the user's memory limit was too low. The helper repair is in
[PR #97](https://github.com/spa5k/pkg/pull/97). See the
[macOS 27 verification note](2026-09-30-macos27-app-helper.md).

## Source and execution

- Reviewed source: `58d802213be132ed8e78cd72719b8c374da4de3e` on
  `feat/macos27-app-helper`. The tree was clean at the start.
- Delegated execution: Pi with `zai/glm-5.3` in E2B. The bridge's launch
  arguments explicitly selected that model. No model was substituted.
- E2B image: `safivo/codex-pi-rust-nix-2026-09-28:default`.
- Runtime: x86-64 Linux, Rust 1.96.1, Determinate Nix 3.22.5 / Nix 2.35.2.
- Both sandboxes received the tracked source. The final sandbox's 910 source
  file hashes matched before and after the tests.
- Production code was not changed. Both sandboxes were stopped after evidence
  was downloaded. No merge or release was done.

[Evidence archive](cask-nix-reliability-2026-09-30.tar.gz) contains the source
manifest, delegated report, case records, native logs, small fixtures, baseline
test logs, and the parent agent's independent results. The findings below are
the final classification. Raw delegated notes can contain earlier conclusions.

## Confirmed findings

### P2 — Default cancellation is reported as a start failure

Owner: [native stderr reader](../../crates/pkg-cli/src/nix/process.rs), line 236.

Trigger: start a real slow Nix build through normal `pkg install`. Send SIGINT
to the pkg PID while it waits for stderr. Do not use `--verbose`.

Observed: pkg exits 1 with `could not start the Nix command: Interrupted system
call (os error 4)`. The build had already started. The expected result is exit
130 with an interruption report after the child is reaped. The test profile
was unchanged. A retry installed the fixture, and its binary ran.

`pipe.read(...)?` returns early on `ErrorKind::Interrupted`. This skips
`child.wait()`. The shared runner then converts the read failure into a spawn
error. The verbose path uses a different wait operation and passed the same
signal test. This explains why behavior can change when verbose output is on.

Repair: retry interrupted reads. Ensure every error path waits for the child.
Preserve the received signal until cancellation is classified. Add one native
slow-build test for both output modes. Evidence:
`parent/filtered-cancel-results.json` and `parent/filtered-cancel-fixture/`.

### P2 — A mutable path can be classified as a fixed revision

Owner: [reference rules](../../crates/pkg-cli/src/nix/reference.rs), lines 37–60.

Trigger: configure the Nixpkgs source as a local `path:` flake whose directory
name is 40 hex characters. Install version 1. Change the flake to version 2.

Observed: `pkg upgrade --all` exits 0 and says `fixed source reference`. The
installed binary remains version 1. Native `nix profile upgrade --all` on that
same isolated profile succeeds and installs version 2.

The fixed-reference rule accepts a final 40-hex path segment for every source
scheme. A directory name does not make a local flake immutable. The same rule
also reads revision identity, so its assumptions affect more than the skip
message.

Repair: classify references by transport and revision semantics. Accept a
commit pin only where Nix treats it as one. Keep mutable path sources movable.
Evidence: the six `hex-path-*` records in `parent/results.json` and
`parent/reliability.py`.

### P2 — An unrelated broken tap blocks a qualified install

Owner: [install setup](../../crates/pkg-cli/src/commands/lifecycle.rs), line 33;
[saved tap loading](../../crates/pkg-cli/src/tap/saved.rs).

Trigger: leave a valid registry entry for a tap without a usable published
generation. Request `pkg install nixpkgs:probe` from a working local flake.

Observed: install exits 1 before Nix adds the package. The message names the
unrelated broken tap. The control home, without that registry entry, installs
the same package and runs its binary.

Install loads every saved tap before it handles qualified IDs. Strict loading
is necessary for bare names: ignoring one failed source can select the wrong
package. A qualified Nixpkgs request does not need that source set. This is a
coupling problem against the
[qualified-source isolation requirement](../../openspec/changes/import-public-cask-taps/specs/namespaced-cask-catalog/spec.md).

Repair: load only the source data required by a qualified request. Keep strict
loading for ambiguous bare-name resolution. Do not add a global ignore-error
fallback. Evidence: `delegated/logs/case2-install-broken-tap.txt`, the control
journey, and the native case records.

Doctor returns success in the broken-tap home. It checks runtime, daemon,
profile, and launcher health, but it does not inspect saved tap generations.
This is a diagnostic gap. Add a read-only tap health observation with the
existing repair commands.

### P2 — The generated catalog loses the maximum macOS requirement

Owner: [OS constraint classification](../../tools/cask-catalog/src/classify.rs),
line 158; [plan fields](../../tools/cask-catalog/src/plan.rs);
[index projection](../../nix/casks/lib/catalog.nix).

Trigger: generate a catalog from a synthetic app record with the supported
`arm64_sequoia` tag and `maximum_macos <= 15.9`.

Observed: the compiled generator exits 0 and marks the app eligible. Neither
the plan nor the index retains the maximum OS requirement. The baseline is
15.7.7, so the generator accepts the record for that baseline. The client then
checks `aarch64-darwin`, without checking the host's OS version. It cannot
refuse this record on macOS 27.

This is proven as a data-loss defect with the compiled generator and source
inspection. It is not a claim that a real vendor app failed to launch in this
run. The current catalog baseline is also a deliberate compatibility policy;
raising it alone does not repair the lost maximum requirement.

Repair: retain the declared OS range in the plan and client index. Check the
host version before install. Define how host versions select catalog variants.
Keep the read-only bundle minimum check at build time. Evidence:
`metadata/input.json`, `metadata/pin.json`, and `metadata/result.json`.

### P3 — Info can give two identical, unusable choices

Owner: [exact lookup choices](../../crates/pkg-cli/src/catalog/search.rs),
lines 1034–1041.

Trigger: a configured flake exposes `probe` through both
`packages.x86_64-linux` and `legacyPackages.x86_64-linux`.

Observed: install succeeds. `pkg info nixpkgs:probe` exits 1 with
`choose one of: probe, probe`. Repeating either printed choice cannot resolve
the ambiguity. Explicit full attribute paths work and report the two versions.

Refusing an ambiguous lookup is valid. Removing the namespace from both
choices makes the guidance unusable. It also conflicts with the intended
search-ID round trip.

Repair: retain full attribute paths when short names collide. Use the same
rule for search IDs and ambiguity choices. Check that info identifies the
output native install selects. Evidence: `parent/namespace-results.json`.

## How the package paths work

| Stage | Current owner and behavior | Remaining boundary |
| --- | --- | --- |
| Metadata | Rust merges target variants, validates paths and checksums, and excludes unsupported installer actions and dependencies. | Eligibility is a metadata decision. It does not prove payload compatibility. |
| Download | A Nix fixed-output fetch verifies SHA-256. Public HTTPS destinations are checked and pinned. Transfer size and time are bounded. | Vendor URLs can disappear. Archive expansion has no general byte quota. |
| Cask build | Generic Nix builders call `plan.py` for content detection, path/link checks, extraction, artifact placement, and output audit. Linux uses a bounded library set. | Real archive layout, shared libraries, bundle requirements, and app behavior are separate checks. |
| Install | pkg validates IDs, reads the native profile, and sends one add operation to Nix. It reads the profile again afterward. | Source isolation and default cancellation have the defects above. |
| App exposure | macOS launchers are a second operation through a separate helper profile. Public tap apps have assessment checks. | Package installation and launcher refresh are separate results. Helper dependencies must work on the host OS. |
| Recovery | Remove, upgrade, history, rollback, and prune use native Nix generations. | Exact entry IDs are required. Prune does not run store-wide GC. |

The success message now follows launcher refresh. A launcher failure after an
install must still report partial success, because the native package profile
has already changed. Combining those two operations into one success label
would hide the installed state.

## Executed proof and exclusions

The first sandbox passed 99 pkg library tests, 18 CLI integration tests, and
the no-shell canary. It also passed 109 cask generator library tests, two
single-test integration/canary targets, and 57 fetch safety tests. Its initial
native fixture used store paths that pure Nix evaluation refuses. Those failed
fixture installs are excluded from product findings. The sandbox later stopped
accepting shell commands. Its cause was not established.

The second sandbox ran real Nix operations with static C fixture builders.
The delegated journey covered install, package use, list, remove, history,
rollback, prune, and a moving-source upgrade. Duplicate install was unchanged.
Bad input and a failed multi-package operation left the manifest and generation
unchanged. A home path with spaces and a quote passed install, use, and remove.
Exact removal preserved the other entries.

The parent agent checked the mutable hex-path source and the overlapping
attribute namespaces independently. It also checked default cancellation,
retry, and package use. Its archive tests accepted a good ZIP and refused
traversal, duplicate members, and an escaping symlink chain before writes
outside staging. The delegated archive checks cover further bad ZIP cases.

Removed false positives:

- Install IDs and native remove IDs can differ. The native lifecycle spec
  requires the exact installed entry ID. `pkg list` supplies that ID.
- Current Nix matches remove arguments literally. Regex matching requires
  `--regex`. Similar names were preserved in the native test.
- A hand-written fake profile directory and a dangling link were also treated
  as empty by native Nix. Those checks do not prove a pkg-specific corruption
  defect.
- One slow builder did not create its output. That was a fixture defect.
- Unbounded stderr capture and archive expansion remain source-level risks.
  No memory-exhaustion or disk-exhaustion claim is made from this run.

No current vendor GUI launch, real cask download/build, public Ruby tap import,
macOS VM boot, Gatekeeper, APFS, or reboot was tested in this audit. The E2B
daemon did not prove the required no-fallback tap sandbox policy. Its settings
were not weakened. Linux results do not prove macOS behavior. Older macOS
15.7.7 evidence is in the [cask stress report](2026-09-29-cask-stress.md).
This was a targeted subsystem review, not a declaration that every source or
test line was reviewed.

## First repair and test batch

1. Repair default cancellation and add one real slow-build test for both
   output modes. Keep the pre-fix failure and post-fix pass together.
2. Repair fixed-reference classification and qualified source loading. Extend
   the existing native journey with those two cases.
3. Retain OS requirements. Add a compiled-generator case and a native macOS
   host-version check. Correct the overlapping info choices.
4. Add a small native Nix install/use/remove/rollback job to CI. It needs one
   deterministic fixture, not a bulk vendor test framework.
5. Add a disposable macOS helper build/startup/sync check on the supported OS
   versions. Include macOS 27 before the next compatibility release.
6. Route cask builder checks for changes anywhere under `nix/casks/`, including
   `flake.nix` and `flake.lock`. Run the existing archive/builder checks.

The [ordinary CI smoke job](../../.github/workflows/ci-fast.yml) runs help,
version, completions, and doctor. It does not perform an install. The
[cask workflow](../../.github/workflows/cask-catalog.yml) regenerates metadata
and runs generator/fetch tests. It does not run the native builders, and its
path filter omits the flake and lock files. These gaps explain why a green CI
result currently gives limited evidence for a package install on a new OS.
