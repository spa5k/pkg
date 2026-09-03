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
