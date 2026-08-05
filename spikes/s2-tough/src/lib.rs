// Spike S2 — real TUF via `tough` (PR-5 / DR-002).
//
// This is a STANDALONE Cargo workspace (see `Cargo.toml`'s `[workspace]` table)
// that validates, against the real `tough` crate (awslabs, exactly `0.24.0`),
// that pkg's channel/trust requirements are expressible and correctly enforced.
//
// SLICE 1 (this checkpoint) establishes a compilable, tested standalone base
// fixture before adversarial security cases:
//
//   * pin a trusted root and load a signed repository over `FilesystemTransport`;
//   * enforce expiration with `ExpirationEnforcement::Safe`;
//   * apply explicit, conservative `Limits`;
//   * keep a *persistent* datastore (required for rollback protection);
//   * read targets by FULLY CONSUMING the stream, so target hash validation
//     completes before any bytes are used (TRU-INV-01);
//   * model the small pkg target set (`descriptor.json` + managed-Nix runtime +
//     Nixpkgs source + per-system indexes);
//   * serialize the canonical `descriptor.json` (plans/02 §7) with a strict
//     snake-case-drift guard; and
//   * prove a pinned-root happy-path load, persistent timestamp/snapshot files
//     after load, delegated-target verification, and `read_target_fully`.
//
// It performs NO bespoke ("TUF-lite") cryptography: all signing is
// `tough::sign::Sign` + `tough::editor::RepositoryEditor` +
// `tough::key_source::LocalKeySource`, and all verification is `tough`'s real
// TUF client (`RepositoryLoader`). The only hand-assembled metadata is the
// bootstrap `root.json`, built from `tough::schema` types and signed through
// `Sign` (the narrow test-publisher boundary documented in `keys.rs`). aws-lc-rs
// is used only to *generate* ephemeral Ed25519 PKCS#8 test material.
//
// Slice 2 (not in this checkpoint) adds the adversarial cases: threshold
// acceptance/refusal, one-bit tamper refusal, root rotation with revocation,
// rollback refusal, mix-and-match refusal, expiry refusal, and conservative
// size-limit refusal.

pub mod descriptor;
pub mod fixture;
pub mod keys;
pub mod limits;
pub mod repo;
pub mod verify;

pub use descriptor::ChannelDescriptor;
pub use fixture::{Fixture, build_fixture};
pub use keys::{SignKey, generate_keys, sign_role};
pub use limits::CONSERVATIVE_LIMITS;
pub use repo::{DelegationSpec, RepoBuilder, RepoPaths, RoleSpec, RootSpec};
pub use verify::{Verifier, read_target_fully};
