DONE
PR-6 (repeat-run proof) complete. 6 signed commits on
verify/dn1-pr6-repeat-proof.
- alpha.26/.27 minted from 56f6782 with the loopback URL baked, signed
  via the pinned publish-release workflow (19 assets each)
- channels published with the c317d2ad test root; pair sealed
  (3ee51dd5...) and uploaded clean to tag dn1-proof-pair-3
  (bundle aac6aa23..., 419431832 bytes)
- all twelve pins filled; PKG_REVIEWED_COMMIT corrected to 56f6782
- proof-repeat.yml: dispatch gated, acquire-from-tag with digest pins,
  in-VM TLS loopback channel, per-dispatch CA + System keychain trust
  with strict always-teardown, digest-key verdict aggregate
- tests: 15 repeat-workflow + 11 loopback-server + 27 DN-16 bindings +
  release suite - all green; bundle layout and every inventory digest
  verified against the real tarball
Remaining before first dispatch (user action): re-register the two
Apple Silicon self-hosted runners (pkg-disposable-macos-proof-1/2).
PR: https://github.com/spa5k/pkg/pull/10
