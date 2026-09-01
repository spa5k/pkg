# Channel V1 signed fixture

This is the committed contract fixture for `pkg-channel` PR-11. It contains a
generated TUF repository with threshold-capable root metadata, a signed
`descriptor.json`, managed-Nix runtime and canonical asset-manifest targets for
all four V1 systems, fixed product installer payloads, and delegated per-system
RFC 8785 canonical,
Brotli-compressed schema-v1 catalog indexes. All keys and artifact bytes are
synthetic test data; the indexes are structurally real but deliberately empty.

Regenerate it from the isolated S2 publisher harness:

```sh
cd spikes/s2-tough
RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --example export_fixture
```

The fixture deliberately has long-lived test-only expirations so tests do not
become date-dependent. Its rotation-policy citation points to the
[legacy plan](../../plans/archive/2026-08-22-custom-managed-nix-v1/02-trust-and-update-model.md).
That citation records fixture history. It is not current design authority.
