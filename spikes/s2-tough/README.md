# Spike S2 — real TUF via `tough` (PR-5 / DR-002)

> Links into the dated plan archive are legacy-plan context. They are not
> current design authority.

> **Status:** STANDALONE spike. This directory is its own Cargo workspace (see
> [`Cargo.toml`](Cargo.toml)) and is deliberately **not** part of the production
> workspace at the repository root. It is `publish = false`, carries **no**
> `license` field and **no** SPDX headers (DR-015), and proves nothing about
> production code. It produces a technical recommendation for **DR-002** only.
> Concrete results live in [`findings.md`](findings.md); the decision record is
> [`plans/12`](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md) DR-002.
>
> ⚠️ **One target-set row below is SUPERSEDED** — the accepted architecture
> ([`plans/02` §6.5](../../plans/archive/2026-08-22-custom-managed-nix-v1/02-trust-and-update-model.md),
> [`plans/03` §6.2](../../plans/archive/2026-08-22-custom-managed-nix-v1/03-nixpkgs-source-and-index.md)) acquires Nixpkgs via
> the locked-flake fetcher, **not** as a `src.tar.gz` TUF target. See §7 for the
> record, preserved verbatim as historical evidence.

---

## 1. Purpose

This spike answers the PR-5 / S2 question:

> Does the **real TUF** specification, as implemented by the Rust [`tough`][TOUGH]
> crate (awslabs / AWS Bottlerocket), express `pkg`'s channel/trust requirements —
> the small target set, per-role threshold signatures, key revocation, rollback,
> freeze, mix-and-match, and endless-data protection — **without inventing any
> "TUF-lite" bespoke cryptography** (D-09)?

It does so by building **real signed TUF repositories** with `tough`'s publisher
APIs and loading them through `tough`'s **real client verification path**
(`RepositoryLoader`) over `FilesystemTransport`. Every cryptographic operation is
`tough`'s own (`tough::sign::Sign`, `tough::editor::RepositoryEditor`,
`tough::key_source::LocalKeySource`); there is **no** hand-rolled signature or
verification anywhere. The only hand-assembled metadata is the bootstrap
`root.json`, built from `tough::schema` types and signed through `Sign` (the
narrow test-publisher boundary documented in §6 and [`src/keys.rs`](src/keys.rs)).

This spike validates DR-002 (channel signing via real TUF). See
[`findings.md`](findings.md) for the exact pass/fail results and threat-by-threat
evidence.

[TOUGH]: https://docs.rs/tough/0.24.0/tough/

---

## 2. Prerequisites

This spike pins the **exact repo toolchain** declared by
[`rust-toolchain.toml`](../../rust-toolchain.toml): **Rust `1.96.1`** (the MSRV is
`1.96`). An active `RUSTUP_TOOLCHAIN` env var overrides the repo pin — clear it or
pin it to `1.96.1` before the gates (see
[`CONTRIBUTING.md` §3.1](../../CONTRIBUTING.md)):

```sh
unset RUSTUP_TOOLCHAIN
# …or pin it explicitly:
RUSTUP_TOOLCHAIN=1.96.1 cargo --version
```

In addition, `tough` pulls in **`aws-lc-rs` → `aws-lc-sys`**, which **compiles
AWS-LC** (a C/C++/assembly fork of BoringSSL) from source via CMake. That native
build requires a working **C/C++ compiler**, **CMake**, and **pkg-config** on the
host (see [`findings.md`](findings.md) §9 for the dependency graph):

| Platform | Native prerequisites |
|----------|----------------------|
| **macOS** | **Xcode Command Line Tools** (provides `clang`/`cc`/`libc++`) **plus CMake** (and `pkg-config`, e.g. via Homebrew) |
| **Linux** | **compiler/build essentials** (`build-essential` / `gcc-c++` providing `gcc`/`g++`/`make`) **plus CMake and `pkg-config`** |

The production workspace at the repo root does **not** have this native build
dependency in v1 (it has no crypto crate yet); it is introduced **only** by this
spike's `tough`/`aws-lc-rs` dependency and is recorded as a DR-002 consequence.

---

## 3. Standalone commands

All commands run from **this directory** (`spikes/s2-tough/`). The spike has its
own `Cargo.lock` and `target/` and is excluded from the root
`build`/`clippy`/`test`/`doc` lanes. Every gate runs on the repo-pinned toolchain
(**Rust `1.96.1`**); an active `RUSTUP_TOOLCHAIN` env var overrides the pin, so
each command prefixes it explicitly (or `unset` it first) — see
[`CONTRIBUTING.md` §3.1](../../CONTRIBUTING.md).

```sh
# Formatting (rustfmt; --check passed through to rustfmt):
RUSTUP_TOOLCHAIN=1.96.1 cargo fmt --all -- --check

# Type-check every target/feature against the committed lockfile:
RUSTUP_TOOLCHAIN=1.96.1 cargo check --all-targets --all-features --locked

# All 20 tests (5 unit + 6 load/targets + 9 adversarial) vs the lockfile:
RUSTUP_TOOLCHAIN=1.96.1 cargo test --all-features --locked

# Clippy over every target/feature vs the lockfile; warnings are hard errors:
RUSTUP_TOOLCHAIN=1.96.1 cargo clippy --all-targets --all-features --locked -- -D warnings

# Rustdoc: treat warnings as errors, include private items, all features, locked:
RUSTUP_TOOLCHAIN=1.96.1 RUSTDOCFLAGS='-D warnings' cargo doc --no-deps \
  --document-private-items --all-features --locked

# Supply-chain audit against this spike's own Cargo.lock; warnings are denials:
RUSTUP_TOOLCHAIN=1.96.1 cargo audit --file Cargo.lock --deny warnings

# cargo-deny against the repo-root deny.toml, all features, locked:
RUSTUP_TOOLCHAIN=1.96.1 cargo deny --manifest-path Cargo.toml \
  --config ../../deny.toml --all-features --locked check
```

Docs cross-reference checker — run from the **repo root** (it validates links
across the whole plan set, not this spike in isolation):

```sh
cd ../.. && python3 .github/scripts/check_docs_links.py
```

Tool versions used for the recorded results in [`findings.md`](findings.md):
`cargo-audit 0.22.2`, `cargo-deny 0.20.2` (matching
[`ci-fast.yml`](../../.github/workflows/ci-fast.yml)).

---

## 4. File map

```
spikes/s2-tough/
├── README.md            ← this file (purpose, prereqs, commands, matrix, claims)
├── findings.md          ← EXACT results + threat-by-threat evidence (read me!)
├── Cargo.toml           ← standalone [workspace]; tough = "=0.24.0", default-features = false
├── Cargo.lock           ← committed (139 crate deps) for reproducible builds
├── src/
│   ├── lib.rs           ← module root; re-exports
│   ├── descriptor.rs    ← plans/02 §7 channel descriptor schema (camelCase guards)
│   ├── fixture.rs       ← builds the tiny signed repo over FilesystemTransport
│   ├── keys.rs          ← ephemeral Ed25519 test keys via tough::sign + aws-lc-rs gen
│   ├── limits.rs        ← CONSERVATIVE_LIMITS (tight metadata size caps)
│   ├── repo.rs          ← RepoBuilder: real RepositoryEditor + LocalKeySource
│   └── verify.rs        ← Verifier: real RepositoryLoader + read_target_fully (full drain)
└── tests/
    ├── load_and_targets.rs  ← 6 happy-path integration tests
    └── adversarial.rs       ← 9 adversarial security tests (slice 2)
```

Test-generated repository material and signing keys live under an ephemeral
`TempDir` per test; no files escape it and no reusable secrets exist in this
directory.

---

## 5. The exact `tough` pin and spike-only transport

- **`tough` is pinned to EXACTLY `=0.24.0`** (not a range) in [`Cargo.toml`](Cargo.toml).
  This is the version every recorded result in [`findings.md`](findings.md) was
  produced against.
- **`default-features = false`** drops the optional HTTP transport
  (`reqwest` / `rustls-platform-verifier`). The spike loads a **local** signed
  repository, so it uses **`FilesystemTransport`** only. This is **spike-only**:
  PR-11 will decide the production transport (likely HTTPS) separately and is not
  bound by this choice.

```toml
tough = { version = "=0.24.0", default-features = false }
```

---

## 6. Cryptographic boundary (no TUF-lite)

There is **no** bespoke signature or verification code in this spike. The
publisher boundary is intentionally **narrow**:

- **Targets / snapshot / timestamp / delegated targets** are signed entirely by
  `tough::editor::RepositoryEditor` reading keys through
  `tough::key_source::LocalKeySource` (tough's public key source). The editor
  internally calls `tough::sign::parse_keypair` + `Sign`.
- **The bootstrap `root.json`** is the ONE hand-assembled role, because
  `RepositoryEditor` reads an *already-signed* root from disk. It is built from
  `tough::schema` types and signed through `tough::sign::Sign::sign` (the
  canonical-JSON bytes come from `olpc_cjson`, the same crate tough uses). This
  exactly mirrors `tough::editor::signed::SignedRole::new`. See [`src/keys.rs`](src/keys.rs).
- **`aws-lc-rs`** is used **only to *generate* ephemeral Ed25519 PKCS#8 test
  material**; it performs no parsing and no signing here. Parsing the PKCS#8,
  signing bytes, and deriving the TUF public `Key` all go through `tough`'s public
  `tough::sign::{parse_keypair, Sign}` abstraction.

**Ephemeral test-key handling:** every signing key is a freshly generated
Ed25519 keypair held in memory for the duration of one test. Its PKCS#8 bytes are
written to a file **only** inside that test's `TempDir` (so `LocalKeySource` can
read them); those files are deleted with the `TempDir`. No private key material is
ever written outside a test's `TempDir`, and there are **no reusable/real secrets
in this repository**.

---

## 7. Target set (what this models)

The fixture is reconciled to the accepted design: Nixpkgs source is **not** a
product TUF target. Its `rev`/`narHash` are authenticated transitively by the
descriptor target, and the later bundled-Nix fetch path verifies
`locked.rev`/`locked.narHash` (`plans/03` §6.2). This spike does not exercise
that separate flake-fetch path.

The spike models `pkg`'s real channel target set (`plans/02` §6.4 / §7):

| Target | Signed by | Notes |
|--------|-----------|-------|
| `descriptor.json` | top-level `targets` (1-of-1) | the channel descriptor itself (§10) |
| `nix/<ver>/<sys>.tar.xz` | top-level `targets` (1-of-1) | managed-Nix runtime, one per supported system |
| `index/<seq>/<sys>.json.br` | **delegated** `index` role (1-of-1, paths `index/**`) | disposable per-system catalog index |

### Delegated targets ARE supported and demonstrated

The delegated `index` role (1-of-1, `paths = ["index/**"]`) is exercised
end-to-end: `tough` fetches `index.json`, verifies its signature against the
delegation recorded in the top-level targets, and reads its four per-system index
targets through the **same** hash check as a top-level target (test
`read_delegated_index_target`). This is real even though some upstream `tough`
prose about delegations reads as stale — **the path is supported and proven by
this spike**, and the delegated targets read back byte-for-byte identical to the
fixture bytes.

---

## 8. Persistence, expiry, and what is NOT free (anti-rollback)

**Rollback / freeze protection is NOT free.** It depends on a **persistent
datastore** that survives across `pkg update` runs:

> **A persistent datastore is REQUIRED for cross-run rollback memory.** Without
> it, `tough`'s rollback guard is never entered (no previously-seen
> `timestamp.json` to compare against), and an older-but-validly-signed metadata
> set is accepted. The spike proves this directly: a *fresh* datastore accepts
> the old valid repo, while the *same* datastore that previously saw a newer
> version refuses it as a rollback (test
> `cross_run_rollback_refuses_older_timestamp_via_persisted_datastore`).
> **Never claim anti-rollback is free** — it is a datastore-responsibility, not a
> property of the metadata bytes alone.

PR-11 must own a real, durable, single-writer datastore path and surface
"channel rollback refused" to the user. See [`findings.md`](findings.md) §5.

### ExpirationEnforcement::Safe and monotonic last-known-time

- The spike always loads with **`ExpirationEnforcement::Safe`** on the normal
  path. `Safe` refuses signed metadata whose `expires` field is in the past,
  evaluated by `tough` against the **real wall clock** (`jiff::Timestamp::now()`).
  Test `expiration_safe_refuses_expired_targets_against_real_clock` proves this
  against the real clock and isolates it from `ExpirationEnforcement::Unsafe`
  (which loads the identical expired metadata). `Unsafe` is **prohibited** on
  normal update/install paths.
- `Safe` mode maintains a **monotonic last-known-time** bookkeeping file
  (`latest_known_time.json`) in the datastore. In `Safe` mode `read_target`
  re-samples the wall clock for its expiry check and may legitimately advance
  this file's value on every read; this is benign monotonic-clock bookkeeping —
  neither signed metadata nor target content — and is the documented side effect
  of the `Safe` expiry check. Test
  `one_byte_target_tamper_refused_after_drain_no_bytes_returned` asserts that this
  is the **only** datastore file that changes during a tampered read.

### Descriptor `expiresAt` / build-time semantic validation is NOT implemented here

This spike authenticates and **delivers** the descriptor bytes (and proves they
serialize to the exact canonical `plans/02` §7 shape). It does **not** implement
product-semantic validation of descriptor fields. Specifically **deferred to
PR-11**: `schemaVersion`, `policyVersion`, `sequence`, `expiresAt`,
`supportedSystems`/systems allowlists, the `substituters`/`trustedPublicKeys`
URL/key allowlists, and the cross-checks between the hashes recorded in the
descriptor and the TUF-authenticated target hashes. See §10.

---

## 9. Target streams must be fully drained before bytes are trusted

`tough::Repository::read_target` returns a stream whose SHA-256 is validated
**incrementally** by a `DigestAdapter`; the hash check is only complete once the
stream reaches end-of-input. Therefore `pkg` must **fully consume** the stream
before trusting any bytes — never hand a partially-read stream to a consumer.

The spike provides the helper [`read_target_fully`](src/verify.rs) (`src/verify.rs`)
that drains the entire stream into a `Vec<u8>` via `IntoVec` and returns `Err` if
any chunk fails. Callers never receive partially-verified or tampered bytes. Test
`one_byte_target_tamper_refused_after_drain_no_bytes_returned` proves a one-byte
mutation is refused **after** the full drain (the `DigestAdapter` emits the
mismatch at end-of-stream) and that no tampered bytes are returned or persisted.

---

## 10. Semantic descriptor validation is deferred to PR-11

A `descriptor.json` is itself a TUF **target**. `tough` supplies the
**cryptographic / TUF** guarantees for it (authentication, integrity, rollback,
freeze, mix-and-match, threshold). The **product-semantic** policy fields are a
PR-11 responsibility. The spike deliberately implements **none** of:

- `schemaVersion` (format version; doc `05` owns migrations),
- `policyVersion` (monotonic per channel — TRU-INV-03),
- `sequence` (strict monotonicity referenced by generations — TRU-INV-03),
- `expiresAt` (TRU-INV-04 — refused for *new* installs past expiry; grace policy
  UD-02.1),
- `supportedSystems` / systems allowlists,
- `substituters` URLs + `trustedPublicKeys` allowlists (D-10),
- cross-checks between the hashes recorded in the descriptor and the
  TUF-authenticated target hashes (defense in depth, `plans/02` §11).

The spike's descriptor tests are **strict serialization-shape guards only**
(camelCase key set/order, four-system coverage, round-trip) plus an end-to-end
read-back that cross-checks declared hashes against fixture bytes — they add
**no** production policy validation.

---

## 11. Test matrix — 20 tests and the threat each proves

All 20 tests pass (5 unit in [`src/descriptor.rs`](src/descriptor.rs), 6 in
[`tests/load_and_targets.rs`](tests/load_and_targets.rs), 9 in
[`tests/adversarial.rs`](tests/adversarial.rs)). Zero failures.

### Happy-path / load & targets (`tests/load_and_targets.rs`)

| # | Test | Proves |
|---|------|--------|
| 1 | `pinned_root_loads_happy_path` | Pinned trusted root + `FilesystemTransport` + `Safe` + `CONSERVATIVE_LIMITS` + persistent datastore loads the fixture through tough's full client verification (root → timestamp → snapshot → targets → delegated targets). |
| 2 | `persistent_timestamp_and_snapshot_after_load` | `tough` persists `timestamp.json` + `snapshot.json` into the **persistent** datastore during load — the cross-run rollback memory (§8). |
| 3 | `read_top_level_targets_after_drain` | Top-level targets (`descriptor.json` and a Nix runtime target) read back byte-for-byte **after full drain** (TRU-INV-01). Nixpkgs is intentionally fetched separately as a pinned flake. |
| 4 | `read_delegated_index_target` | Delegated `index` role is walked + verified; both preview index targets read back byte-for-byte (§7). |
| 5 | `missing_target_is_none` | An unadvertised target returns `Ok(None)` — the contract PR-11 uses to distinguish "missing" from "tampered". |
| 6 | `descriptor_per_system_maps_have_preview_systems_and_match_fixture_bytes` | Both descriptor per-system maps carry exactly the preview systems, and every descriptor target name/hash matches the actual signed fixture bytes. |

### Adversarial — the security guarantees pkg relies on (`tests/adversarial.rs`)

| # | Test | Threat proved | Concrete tough error variant |
|---|------|---------------|------------------------------|
| 7 | `differing_per_role_thresholds_load_when_met` | **Per-role threshold** semantics (root=1, targets=2, snapshot=1, timestamp=2) load when met (T-CHAN-5, T-REL-2) | (accept) full client verification passes |
| 8 | `insufficient_valid_signatures_rejected_by_tough_client_role_local` | **Threshold refusal** by tough's *client* (not publisher-side count); role-local (T-CHAN-5) | `VerifyMetadata{role: Timestamp}` ⊃ `SchemaError::SignatureThreshold{role: Timestamp, threshold: 2, valid: 1}` |
| 9 | `expiration_safe_refuses_expired_targets_against_real_clock` | **Freeze / expiry** refusal against the real wall clock (T-CHAN-2) | `ExpiredMetadata{role: Targets}` (and `Unsafe` loads the identical expired bytes, isolating the cause) |
| 10 | `conservative_limits_refuse_oversized_timestamp_metadata` | **Endless-data** refusal via `CONSERVATIVE_LIMITS.max_timestamp_size` (T-CHAN-4) | `Transport` ⊃ `MaxSizeExceeded{max_size: 32_768, specifier: "max_timestamp_size argument"}` |
| 11 | `one_byte_target_tamper_refused_after_drain_no_bytes_returned` | **Full-drain target tamper** refusal; no bytes returned or persisted (T-CHAN tampering, TRU-INV-01) | `Transport` ⊃ `HashMismatch{expected: <signed>, calculated: <tampered>}` |
| 12 | `cross_run_rollback_refuses_older_timestamp_via_persisted_datastore` | **Rollback** refusal depends on persisted datastore (T-CHAN-1) | `OlderMetadata{role: Timestamp, current_version: 2, new_version: 1}` (fresh datastore accepts the same old repo) |
| 13 | `mix_and_match_spliced_older_snapshot_refused_for_hash_mismatch` | **Mix-and-match** — a validly-signed *older* snapshot spliced into a newer repo is refused for the timestamp-pinned hash (T-CHAN-3) | `Transport` ⊃ `HashMismatch{calculated: <A's snapshot>, expected: <B's snapshot>}` |
| 14 | `root_rotation_dual_authorization_threshold_checks` | **Root rotation N→N+1** dual authorization: old-root check (1) then new-root self check (2) (T-CHAN-5, T-REL-2) | `VerifyMetadata{role: Root}` ⊃ `SignatureThreshold{role: Root, threshold: 1, valid: 0}` for each unmet check |
| 15 | `revocation` (`root_rotation_revocation_rejects_signed_by_revoked_key`) | **Revoked-key rejection** — after a valid rotation, a root signed only by the revoked key is refused (T-CHAN-5, T-REL-2) | `VerifyMetadata{role: Root}` ⊃ `SignatureThreshold{role: Root, threshold: 1, valid: 0}` |

### Serialization-shape guards (`src/descriptor.rs`, unit tests)

| # | Test | Proves |
|---|------|--------|
| 16 | `top_level_keys_are_exactly_canonical_and_ordered` | The descriptor top-level key **set** and **order** match `plans/02` §7 byte-for-byte. |
| 17 | `build_policy_native_local_builds_covers_preview_systems` | `buildPolicy.nativeLocalBuilds` covers exactly the preview systems, both `allow-with-gates`. |
| 18 | `camel_case_required_paths_exist` | Every required camelCase JSON path exists in the serialized descriptor. |
| 19 | `no_snake_case_drift_in_serialized_bytes` | No snake_case Rust field name leaks into the JSON (regression for a dropped `#[serde(rename)]`). |
| 20 | `round_trip_sample_is_equal` | The canonical sample round-trips through serialize/deserialize to an equal descriptor. |

**Exact results:** 20 passed, 0 failed, 0 ignored. See [`findings.md`](findings.md)
for the byte sizes, version floors, and dependency details.

---

## 12. Non-claims / limitations

This spike deliberately does **not** claim:

- That anti-rollback / freeze protection is free — it **requires a persistent
  datastore** (§8) and **never** should be described as automatic.
- Any product-semantic validation of descriptor fields (§10) — all deferred to
  PR-11.
- That it proves endless-data protection for `max_root_size` / `max_targets_size`
  / `max_snapshot_size` — the size-limit test (#10) exercises
  `max_timestamp_size` only. (The other caps are configured and applied, but only
  one is adversarially exercised.)
- That `tough` checks `descriptor.expiresAt` or any build-time value — only the
  **signed TUF metadata** expiration, against the wall clock (#9).
- That it evaluates a *real HTTPS* transport — only `FilesystemTransport`
  (spike-only; §5).
- That it is production code — it is a standalone, `publish = false` spike.
- That this spike alone accepted DR-002. The decision was subsequently **Accepted on
  2026-08-09** after the F+A review recorded in
  [`findings.md` §11](findings.md) and [`plans/12`](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md).

---

## 13. Supply-chain checks

Both pass at the recorded commit:

- **`cargo audit`** — **success over 139 crate dependencies**. See
  [`findings.md`](findings.md) §7 for the CVE/footnote on `tough` and the note
  that **`cargo audit` alone may not yet mirror CVE-2026-6967**, so the **vendor**
  (awslabs/tough) and **NVD** advisories were checked directly too.
- **`cargo deny check`** — **success**. The only diagnostics are transitive
  duplicate-version **warnings** (not errors, `multiple-versions = "warn"` in
  [`deny.toml`](../../deny.toml)): `syn 2.0.119` vs `3.0.3`, and `untrusted 0.7.1`
  vs `0.9.0`. See [`findings.md`](findings.md) §7.

---

## 14. Cross-references

- Decision record: [`plans/12` DR-002](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md)
- Exact results + evidence: [`findings.md`](findings.md)
- Channel/TUF design: [`plans/02`](../../plans/archive/2026-08-22-custom-managed-nix-v1/02-trust-and-update-model.md) §7 (canonical descriptor schema), §6.5 (TUF roles)
- Threat model: [`plans/08`](../../plans/archive/2026-08-22-custom-managed-nix-v1/08-security-model.md) §6.5 (T-CHAN-*), §7 (crypto)
- PR roadmap: [`plans/11`](../../plans/archive/2026-08-22-custom-managed-nix-v1/11-pr-roadmap.md) PR-5 (this spike), PR-11 (production client, gated)
