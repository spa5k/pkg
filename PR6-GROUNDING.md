# DN-1 PR-6 grounding: the repeat-run proof

Recon completed 2026-09-03 against `verify/dn1-pr6-repeat-proof` (base b1cbc84).

## Architecture decision (changes the plan's sketch)

The proof product runs INSIDE a tart VM. The runner's 127.0.0.1 is not the VM's
127.0.0.1. Therefore the in-job channel server must run INSIDE each macOS slot
VM, bound to the VM's own 127.0.0.1:8443 — then the baked URL
`https://127.0.0.1:8443` is correct for BOTH the runner-side strings checks
and the in-VM product fetches. Acquisition needs no HTTP channel at all: the
pair bundle comes from the dedicated git tag via `gh api`, digest-verified.

## The minting pipeline (every command verified against the tree)

One release (N), repeat for N+1 with `/n-plus-1/` URLs:

1. Build with the URL baked at compile time (`option_env!` in
   `pkg-install.rs:17`, `service.rs:76–80`):
   ```
   PKG_RELEASE_TUF_ROOT_JSON="$(cat <signing-state>/1.root.json)" \
   PKG_RELEASE_CHANNEL_METADATA_URL=https://127.0.0.1:8443/n/metadata/ \
   PKG_RELEASE_CHANNEL_TARGETS_URL=https://127.0.0.1:8443/n/targets/ \
   cargo build --locked --release \
     -p pkg-cli --bin pkg \
     -p pkg-installer --bin pkg-nix-broker --bin pkg-root-helper --bin pkg-install \
     -p pkg-release --bin pkg-release-index
   ```
2. Preview package: `packaging/macos/build-preview.sh target/release/pkg-install \
   pkg-<ver>-preview.pkg <ver>` (ad-hoc signs a copy; script-only package).
3. Stage the draft asset set: 1.root.json (the pair's TEST root), install.sh,
   pkg-aarch64-darwin, pkg-x86_64-linux, pkg-installer-x86_64-linux, both
   tarballs, release-manifest.json, SHA256SUMS.
4. `gh release create --draft v0.1.0-alpha.2X` with the staged set.
5. Sign via `publish-release.yml` dispatched from the
   `dn16-proof-workflow-1` tag checkout (cosign keyless, OIDC identity pinned
   to workflow+tag; validate-dispatch enforces `GITHUB_REF` is that tag).
6. Channel tree: `linux_proof_publication --publish-dn16 OUT RUNTIMES \
   AARCH64_INPUT X86_64_INPUT SEALED_MANIFEST SIGNING_STATE SEQ RELEASE_ID`
   (prepare-dn16-manifest first). SIGNING_STATE is preserved at
   `/private/tmp/pkg-dn16-proof-build.CoSikP/signing-state/` (root
   `1c5ceff8…`, 3 online keys — the DN-16 proof root).
7. Pair: `linux_proof_publication --bind-dn16-pair PAIR_DIR N_ID N+1_ID \
   PRODUCT_COMMIT` → proof-pair.json + n.inventory.json + n-plus-1.inventory.json.
8. Upload the pair bundle to the dedicated tag `dn1-proof-pair-1`.

## The workflow (proof-repeat.yml)

- `workflow_dispatch`: pair_tag, pair_sha, inventory SHAs/lengths (mirroring
  the DN-16 env block), destructive confirmation.
- acquire-inputs: `gh api` asset download + digest verify (NO live channel).
- Per macOS slot: copy channel into the VM; IN-VM: generate CA + server cert
  (openssl), serve `n/` and `n-plus-1/` on 127.0.0.1:8443 (TLS wrapper around
  the publication dirs, `if: always()` teardown), install the CA into the
  VM System keychain (`security add-trusted-cert -d -r trustRoot`),
  remove in cleanup (B2 — no scheme-gate fallback; product enforces HTTPS in
  `policy.rs:261/:517`).
- prove.sh unchanged: its strings gate already expects the baked URL, and
  `https://127.0.0.1:8443` passes acquire-inputs' URL validation.
- Linux leg: the staged host already proves this exact pattern
  (`Dockerfile.stage` bakes `https://127.0.0.1:8443/metadata/`).
- Verdict aggregate: digest-level evidence keys only (runners embed dates/ids).

## Blockers and constraints

- B1 (dead tunnel URL in prebuilt releases): solved by minting alpha.26/.27
  with the loopback URLs, through the real signing pipeline.
- B2 (macOS TLS trust): per-dispatch CA + System keychain in-VM, `STRICT`
  cleanup; macOS runners refuse sandbox-exec (PR-2 verdict) — irrelevant here,
  the VM is disposable.
- **User action required**: the Apple Silicon self-hosted runners were removed
  in the post-proof cleanup. The macOS legs cannot dispatch until they are
  re-registered. The Linux leg runs in regular CI.
- Discovered during recon: `release.yml` now pins the PRODUCTION channel
  (`releases.happytoolin.com`, root `52523a9b…`, live, root expires
  2027-08-18). The proof pair deliberately stays on the TEST root
  (`1c5ceff8…`) — proofs must not touch production.
