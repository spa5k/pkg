# DN-1 PR-6 implementation notes (repeat-run loopback proof)

## Scope

Implements the workflow half of PR6-GROUNDING.md. The mint script landed in
the foundation commit; this PR adds the in-VM loopback channel server, the
`proof-repeat.yml` workflow, their structural tests, and the operator README.
The Linux leg needs nothing new: `Dockerfile.stage` already bakes
`https://127.0.0.1:8443/metadata/`.

## What shipped

1. `tools/release/serve_pair_loopback.py` — the in-VM TLS channel server.
   `bootstrap` validates the pair exactly as the sealed inventories describe
   it, moves it to `channel/` with one rename, read-only-izes it, generates a
   disposable CA plus one server cert (SAN `127.0.0.1`, via `openssl` config
   files so LibreSSL and OpenSSL both work), serves `n/` and `n-plus-1/` on
   `127.0.0.1:<port>` over TLS 1.2+ only, then fetches every sealed file back
   through the live endpoint before it reports success. `stop` refuses
   foreign processes, removes the publication and state, and fails if the
   port still accepts connections. `test_serve_pair_loopback.py` covers
   serving, digest equality, plaintext-HTTP and untrusted-TLS refusal,
   read-only publication, restore-on-reject, port bounds, foreign-process
   refusal, and corrupt-state fail-closed (11 tests).
2. `.github/workflows/proof-repeat.yml` — mirrors the DN-16 workflow shape
   (validate-dispatch, harness, acquire-inputs, four slot phases, aggregate)
   with the PR-6 deltas: acquisition downloads the pair bundle from the
   `dn1-proof-pair-1` tag through `gh api` (asset id, `.draft = false`,
   octet-stream, exact length + digest pins), extracts the tar with bounded
   member validation, verifies both full inventories including per-side total
   bytes and canonical rows digests, and cosign-verifies the proof-inputs
   with the same `publish-release.yml@refs/tags/dn16-proof-workflow-1`
   identity. Each slot phase then publishes the loopback channel in-VM,
   installs the CA with `security add-trusted-cert -d -r trustRoot`, proves
   `verify-cert` passes, runs the unchanged lifecycle harness, and an
   `if: always()` teardown reverses the trust (`remove-trusted-cert -d`),
   stops the server, and asserts the port is closed and the CA is gone.
   A strict teardown failure fails the phase (B2, no scheme-gate fallback).
3. `tests/macos-clean-host/test_repeat_workflow.py` — 15 structural tests
   binding the workflow, the tool, `prove.sh`, and `REPEAT.md`. The harness
   job runs it next to the DN-16 binding.
4. `tests/macos-clean-host/REPEAT.md` — the operator README shipped inside
   the harness artifact (mint prerequisites, runner registration, the
   operator pause, teardown guarantees).

## Decisions and deviations from PR6-GROUNDING.md

- **"prove.sh unchanged" did not hold in three places.** The grounding is
  right that the strings gate and lifecycle logic need nothing, but:
  1. `prove.sh` hard-pins the release tags. The new pair is alpha.26/27, so
     the pins moved from alpha.24/25 (test_workflow.py updated in lockstep).
  2. `prove.sh` byte-binds the DN-16 VM acquisition record (21 logical
     fetches, compact channel bytes). The PR-6 record has a new schema
     (`PKG-DN1-VM-ACQUISITION-V1`, pair tag, tarball bytes, loopback URL)
     and `require_env` now names `PKG_PROOF_PAIR_TAG` and
     `PKG_PROOF_PAIR_TARBALL_LENGTH` instead of the compact-channel counters.
  3. `prove.sh` asserted the DN-16 compact tree (`len(files) == 34`,
     `actual == proof_inputs`). The in-VM product fetches targets from the
     loopback server, so the publication must be the FULL tree; the check is
     now exact full-tree equality against each inventory.
  All three changes are visible in `test_repeat_workflow.py`, which also
  forbids the old compact markers in `prove.sh`.
- **The aggregate compares digest-level keys only**, per the grounding:
  pair/tarball/inventory digests and lengths, channel totals, canonical rows
  digests, trusted root, product commit, releases, plus internal identity
  consistency (one runner and nonce per slot, boot UUID changes across the
  reboot, distinct nonces across slots). Run-scoped dates and ids are
  recorded in evidence but never compared against orchestrator values.
- **Pair pins are pending by design.** Twelve `PENDING-DN1-MINT` placeholders
  (zeros and `1`s) sit in the workflow env. They pass the regex gates but can
  never match a sealed pair, so every dispatch fails closed until the real
  mint fills them and the signed `dn1-proof-workflow-1` tag is cut.
- **The disposable-runner contract is reused verbatim** (labels
  `pkg-disposable-macos-proof-1/2`, names `pkg-dn16-proof-runner-1/2`,
  marker and continuation schemas). The runners were removed after the DN-16
  cleanup; re-registration is the recorded user action before any dispatch.
- **The pair stays on the DN-1 TEST root** (`c317d2ad…`, minted 2026-09-03 after the DN-16 signing state was wiped), pinned as
  `PKG_PROOF_TRUSTED_ROOT_SHA256`. The production channel root
  (`52523a9b…`) is untouched.

## Verification

- `python3 -m unittest tools.release.test_serve_pair_loopback` — 11 passed
- `python3 -m unittest` over `tools/release` (all five tool suites) — OK
- `python3 -m unittest tests.macos-clean-host.test_workflow` — 27 passed
  (DN-16 binding, updated for the moved pins)
- `python3 -m unittest tests.macos-clean-host.test_repeat_workflow` — 15 passed
- `tests/security`, `tests/linux-clean-host` suites — OK, untouched
- `proof-repeat.yml`: YAML parses; every embedded bash block passes
  `bash -n`; every embedded python block compiles; the aggregate block was
  executed against a synthetic four-phase evidence tree (pass) and a
  tampered receipt (fail closed)
- `bash -n` on `prove.sh` and `mint_dn1_proof_pair.sh` — clean
- No Rust source changed; `cargo` gates unaffected

## Mint record (2026-09-03, the completion run)

- **Releases**: v0.1.0-alpha.26 (N, sequence 1) and v0.1.0-alpha.27 (N+1,
  sequence 2), built from 56f6782 with `https://127.0.0.1:8443/{n,n-plus-1}/`
  URLs baked via `option_env!`, tagged at 56f6782, signed through
  publish-release.yml dispatched from the dn16-proof-workflow-1 tag
  (expected_sha 42c4244f, environment approval via the pending-deployments
  API). Both releases carry the full 19-asset set (10 base + 9 generated).
- **SHA256SUMS gotcha**: the signer requires every base asset checksummed —
  including 1.root.json and release-manifest.json. The mint script now
  documents this.
- **Sealed-manifest gotcha**: --prepare-dn16-manifest emits a bundle-free
  manifest; publish-dn16 (Dn16Sealed) requires one WITH the three
  cliArtifact bundle fields. The sealed manifest is constructed by adding
  the bundle digest/length/target fields to the prepared manifest; the
  publish equality check validates the construction.
- **Channel sequences are 1..=2**, not release numbers.
- **Upstream artifacts** (nix 2.34.8 tarballs, determinate 3.22.1 set) are
  reused byte-exact from the preserved DN-16 pair tree, digest-verified.
- **AppleDouble gotcha**: macOS tar injects `._*` members; the workflow
  extractor rejects them. The bundle tarball is built with
  COPYFILE_DISABLE=1 (pair-2 was discarded for this; pair-3 is clean).
- **Immutability**: releases are immutable in this repo — a bad bundle
  means a new tag, not an asset swap (hence pair-3).
- **Pair**: proof-pair.json sha 3ee51dd5…, productCommit 56f6782, channels
  n (total 321645504 bytes, 35 files) and n-plus-1 (321645391, 35 files),
  test root c317d2ad. Bundle tarball sha aac6aa23…, 419431832 bytes, at
  tag dn1-proof-pair-3.
- **Pins**: all twelve filled; PKG_REVIEWED_COMMIT now 56f6782 (the actual
  build commit — cbd3494 was the DN-16 tree, wrong for this pair).
- Verified locally: bundle layout extraction (workflow's own algorithm),
  every inventory digest against the extracted tree, 15 structural tests.

## Session 2 (2026-09-04/05) — TUF expiry diagnosis and pipeline rebuild

### Root causes found

1. **TUF timestamp expiry (the primary blocker)**: The `--publish-dn16` tool sets a
   24-hour timestamp expiry (designed for Linux proofs that run in minutes). The
   macOS lifecycle proof spans hours with reboots; by the next day every install
   fails with `ExpiredMetadata(Timestamp)` mapped through four layers of generic
   error codes to the unhelpful `pkg installation failed`.
2. **Root keys are ephemeral by design**: The `--prepare` tool generates root keys,
   signs root.json, then discards the private keys. Only online keys are persisted.
   Once the timestamps expire, the only recovery is a complete re-key.
3. **MacOS temp directory saturation**: After hours of failed runs, `/tmp` had
   hundreds of entries. The tough library's `tempfile::tempdir()` fails silently,
   surfacing as another opaque `Filesystem` error. Fixed by setting a clean TMPDIR.

### Fixes landed

- `PKG_TIMESTAMP_TTL_HOURS` env var on the publication tool (default 24, set 168
  for macOS proofs)
- `tools/release/build_proof_pair.sh`: single-entry-point pair builder (correct
  build order: build → prepare manifest → draft → sign → download sigstore →
  seal → publish channels → bind pair → tarball → pins)
- Debug instrumentation in pkg-install pinpointing the exact failure layer
- Nine infrastructure fixes: harness split (hosted macOS can't accept loopback
  connections), Python 3.9 compatibility, tar member modes, tab-separated
  evidence, derive-the-dispatch-tag check, ancestor directory rule, marker
  windows widened to 900s, CA teardown by SHA-1, reboot marker newline

### Remaining blocker

The `--publish-dn16` tool's `sign_channel` function returns `SignError::Filesystem`
("release output boundary is unavailable") on the second channel publication
despite all verifiable preconditions passing (root hash matches, output doesn't
exist, consistent_snapshot=true, root not expired). The error chain terminates
without revealing the underlying io path. Needs investigation with the tough
crate source (v0.24.0) — likely in the `RepositoryLoader` or `FilesystemTransport`
during post-signing verification.

### Proven working

Dispatch 34 (run 33826143490) passed the complete prepare phase on real Apple
Silicon: validate → harness → acquire → VM preflight → loopback TLS → CA trust →
prove.sh full lifecycle (fresh install → no-op → repair → package ops → offline
N+1 upgrade) — all 34 cases green.
