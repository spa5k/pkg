# Spike S2 — real TUF via `tough` (PR-5 → DR-002)

| | |
|---|---|
| **Spike** | S2 (PR-5) — *Does the real TUF specification, as implemented by the Rust [`tough`](https://docs.rs/tough/) crate (awslabs / AWS Bottlerocket), express `pkg`'s channel/trust requirements — small target set, per-role threshold signatures, key revocation, rollback, freeze, mix-and-match, endless-data, and drained-stream tamper protection — **without** inventing any "TUF-lite" bespoke cryptography (D-09)?* |
| **Decision it feeds** | DR-002 ([`plans/12`](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md)). **Accepted 2026-08-09 after the F+A review recorded in §11.** |
| **Owner (spike)** | This directory only: `spikes/s2-tough/**`. It is a **standalone** Cargo workspace (`[workspace]` in [`Cargo.toml`](Cargo.toml)), deliberately **not** part of the production workspace at the repo root. `publish = false`, **no** `license` field, **no** SPDX headers (DR-015). |
| **Crypto boundary** | **No bespoke ("TUF-lite") signature or verification anywhere.** Targets/snapshot/timestamp/delegated targets are signed by `tough::editor::RepositoryEditor` + `tough::key_source::LocalKeySource`; all verification is `tough::RepositoryLoader` over `FilesystemTransport`. The ONE hand-assembled role is the bootstrap `root.json`, built from `tough::schema` types and signed through `tough::sign::Sign` (the narrow test-publisher boundary in [`src/keys.rs`](src/keys.rs)). `aws-lc-rs` is used **only to *generate* ephemeral Ed25519 PKCS#8 test material** — it performs no parsing and no signing here. See §2. |
| **Evidence labels** | **(a)** actually executed · **(b)** official docs/source inspected. F+A acceptance is recorded separately in §11. |

---

## 1. Question

Can `pkg`'s v1 channel/trust requirements be met by the **real TUF** specification,
as implemented by `tough`, with **no hand-rolled crypto**? The specific guarantees the
spike must prove `tough` enforces on `pkg`'s tiny target set (`plans/02` §6.4/§7;
threat model `plans/08` §6.5, T-CHAN-*):

1. **Threshold** signatures, **per-role** and role-local (T-CHAN-5, T-REL-2).
2. **Revocation** of a rotated key after a valid root rotation (T-CHAN-5, T-REL-2).
3. **Rollback** protection across runs (T-CHAN-1).
4. **Freeze** / expiry refusal (T-CHAN-2).
5. **Mix-and-match** defense (T-CHAN-3).
6. **Endless-data** bounding via metadata size caps (T-CHAN-4).
7. **Tampered target** refusal, and only after the stream is fully drained (T-CHAN;
   TRU-INV-01 — `pkg` never consumes partially-verified bytes).

---

## 2. Methods & cryptographic boundary (no TUF-lite)

The spike builds **real signed TUF repositories** with `tough`'s publisher APIs and
loads them through `tough`'s **real client verification path** (`RepositoryLoader`) over
`FilesystemTransport`. Every cryptographic operation is `tough`'s own:

- **Targets / snapshot / timestamp / delegated targets** — signed entirely by
  `tough::editor::RepositoryEditor` reading keys through
  `tough::key_source::LocalKeySource`. The editor internally calls
  `tough::sign::parse_keypair` + `Sign`.
- **Bootstrap `root.json`** — the ONE hand-assembled role (`RepositoryEditor` reads an
  *already-signed* root from disk). Built from `tough::schema` types; canonical-JSON
  bytes produced with `olpc_cjson` (the same crate `tough` uses); each signature produced
  through `tough::sign::Sign::sign`. This exactly mirrors
  `tough::editor::signed::SignedRole::new`. See [`src/keys.rs`](src/keys.rs) → `sign_role`.
- **`aws-lc-rs`** — used **only to *generate* ephemeral Ed25519 PKCS#8** test material
  (`Ed25519KeyPair::generate_pkcs8`). It performs no parsing and no signing; parsing the
  PKCS#8, signing bytes, and deriving the TUF public `Key` all go through `tough`'s public
  `tough::sign::{parse_keypair, Sign}` abstraction.
- **`read_target_fully`** ([`src/verify.rs`](src/verify.rs)) — the **mandatory** full-drain
  helper that consumes a target stream to completion via `IntoVec::into_vec` before
  returning any bytes, so `tough`'s incremental SHA-256 `DigestAdapter` always finishes
  before bytes are trusted (§5.4, test #11).

**Ephemeral test-key handling.** Every signing key is a freshly generated Ed25519
keypair held in memory for the duration of one test. Its PKCS#8 bytes are written to a
file **only** inside that test's `TempDir` (so `LocalKeySource` can read them); those
files are deleted with the `TempDir`. No private key material is ever written outside a
test's `TempDir`, and there are **no reusable/real secrets** in this repository.

---

## 3. Exact environment (executed)

| Item | Value |
|---|---|
| Toolchain | **Rust `1.96.1`** (repo pin [`rust-toolchain.toml`](../../rust-toolchain.toml); MSRV `1.96`). An active `RUSTUP_TOOLCHAIN` overrides the pin — clear it or pin `1.96.1`. |
| `tough` pin | **EXACTLY `=0.24.0`**, `default-features = false` (drops the optional HTTP transport `reqwest`/`rustls-platform-verifier`). See [`Cargo.toml`](Cargo.toml) §5. |
| Transport | **`FilesystemTransport` only** (spike-only; a local signed repo). PR-11 decides the production transport separately. |
| `cargo-audit` | `0.22.2` (matches [`ci-fast.yml`](../../.github/workflows/ci-fast.yml)). |
| `cargo-deny` | `0.20.2` (matches [`ci-fast.yml`](../../.github/workflows/ci-fast.yml)). |
| Host OS / arch | macOS (Apple Silicon). |
| Network | Used **only** by `cargo audit`/`deny` to fetch advisory-db / crates.io index; no runtime network in any test. |

---

## 4. Exact test result — 20 passed, 0 failed

```
$ cd spikes/s2-tough && RUSTUP_TOOLCHAIN=1.96.1 cargo test
     Running unittests src/lib.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/adversarial.rs
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/load_and_targets.rs
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Doc-tests pkg_spike_s2_tough
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

**Total: 20 passed, 0 failed, 0 ignored.** Breakdown: 5 unit in
[`src/descriptor.rs`](src/descriptor.rs) (serialization-shape guards), 6 in
[`tests/load_and_targets.rs`](tests/load_and_targets.rs) (happy-path load & targets),
9 in [`tests/adversarial.rs`](tests/adversarial.rs) (the security guarantees). The threat
matrix in [`README.md`](README.md) §11 maps each test to the threat it proves.

---

## 5. Adversarial evidence — threat by threat, with the concrete `tough` error (a)

Each adversarial test pins a **specific** `tough::error::Error` variant (and inner
`tough::schema::Error`) so a `tough` regression or bump cannot silently turn a hard
refusal into an accept. The refusals are produced by `tough`'s **client** verification
(`RepositoryLoader`), never by publisher-side validation. Threat IDs per `plans/08` §6.5.

### 5.1 Per-role THRESHOLD (accept + role-local refusal) — T-CHAN-5, T-REL-2

A repository declaring **differing** per-role thresholds (root=1, targets=2,
snapshot=1, timestamp=2), correctly signed so every role meets its own threshold, loads
through `tough`'s full client verification, and its target reads back byte-for-byte
(`differing_per_role_thresholds_load_when_met`). Then one of `timestamp.json`'s two
signatures is corrupted on disk; `tough`'s client independently counts only the one
remaining valid signature and refuses. The refusal is **role-local** — `tough` looks the
threshold up **per role** (`root.roles[Timestamp].threshold == 2`):

```
tough::Error::VerifyMetadata { role: Timestamp }
  └─ tough::schema::Error::SignatureThreshold { role: Timestamp, threshold: 2, valid: 1 }
```

The `role: Timestamp` + `threshold: 2` fields are the concrete role-local assertion:
timestamp's own threshold (2), distinct from root/snapshot (1). Test:
`insufficient_valid_signatures_rejected_by_tough_client_role_local`.

> Note `timestamp.json` is fetched with `fetch_max_size` (it has NO parent role recording
> its hash, unlike targets/snapshot), so corrupting one signature lets the bytes reach
> `Root::verify_role`, which verifies each signature and counts only the valid ones. The
> publisher produced two valid signatures (passing the editor's publisher-side count);
> after delivery one no longer verifies, so `tough` independently rejects it.

### 5.2 Freeze / expiry against the REAL clock — T-CHAN-2

`ExpirationEnforcement::Safe` refuses signed metadata whose `expires` field is in the
past, evaluated by `tough` against the **real wall clock**
(`jiff::Timestamp::now()`). The targets role is expired one hour in the past
(`targets_expires(hours_from_now(-1))`); root/timestamp/snapshot remain valid, so the
load reaches the targets expiration check (TUF 5.6.5) and is refused there:

```
tough::Error::ExpiredMetadata { role: Targets }
```

The contrast with `ExpirationEnforcement::Unsafe` on the **same** expired repo isolates
the cause: `Unsafe` loads the identical expired metadata without error. `Unsafe` is
**prohibited** on normal update/install paths. Test:
`expiration_safe_refuses_expired_targets_against_real_clock`.

> **We do NOT claim `tough` checks `descriptor.expiresAt` or any build-time value** —
> only the **signed TUF metadata** expiration, against the wall clock. Descriptor
> product-semantic validation is deferred to PR-11 ([`README.md`](README.md) §10).

### 5.3 Endless-data bounding via REAL limits — T-CHAN-4

The conservative `CONSERVATIVE_LIMITS` (unchanged, not a reduced copy) refuse an
**actually oversized** signed `timestamp.json`. The fixture authorizes **160 ephemeral
timestamp keys** (threshold 1); `RepositoryEditor` signs `timestamp.json` with **every
authorized key**, so the file lands **over** `max_timestamp_size`. The pinned `root.json`
(listing all 160 keys) stays **under** `max_root_size`, so the load reaches the timestamp
fetch and the limit that fires is specifically `max_timestamp_size`. Both sizes are
asserted on disk **before** load:

| File | Bytes | Cap | Verdict |
|------|-------|-----|---------|
| `timestamp.json` | **38,778** | `max_timestamp_size = 32,768` | **OVER** (by 6,010 B) |
| `1.root.json` | **55,530** | `max_root_size = 65,536` | under (by 10,006 B) |

`tough` refuses the oversized fetch with the **real** configured constant (not a reduced
copy), carried inside a transport-layer error:

```
tough::Error::Transport { source: <TransportError, kind = Other> }
  └─ tough::Error::MaxSizeExceeded { max_size: 32768, specifier: "max_timestamp_size argument" }
```

`timestamp.json` is the top of the chain (no parent role records its length), so
`max_timestamp_size` is enforced directly. Test:
`conservative_limits_refuse_oversized_timestamp_metadata`.

> **This exercises `max_timestamp_size` ONLY.** It does **not** prove endless-data
> protection for `max_root_size` / `max_targets_size` / `max_snapshot_size` (those caps
> are configured and applied, but only one is adversarially exercised — §10).

### 5.4 Tampered target refused AFTER full drain; no bytes returned — TRU-INV-01

After load (so the in-memory targets metadata pins the authentic sha256), exactly ONE
byte of the advertised target file is mutated on disk (length unchanged). The helper
`read_target_fully` drains the **entire** stream into a `Vec` via `IntoVec::into_vec`;
`tough`'s `DigestAdapter` emits the mismatch only at end-of-stream, so the error surfaces
**after** draining and the collected (tampered) bytes are dropped — never returned, never
written to disk by this path:

```
tough::Error::Transport { source: <TransportError, kind = Other> }
  └─ tough::Error::HashMismatch { expected: <signed sha256>, calculated: <tampered sha256> }
```

The test additionally snapshots **every** datastore regular file before/after the
tampered read and asserts (a) the exact **set** of files is unchanged (no tampered target
cached) and (b) every file's bytes are identical, with the **single documented
exception** of `latest_known_time.json` (`tough`'s monotonic-clock bookkeeping — see §5.6).
The on-disk target still holds the tampered bytes (nothing "healed" it). Test:
`one_byte_target_tamper_refused_after_drain_no_bytes_returned`. **`read_target_fully` is
the mandatory PR-11 contract: never consume target bytes from a stream that errored.**

### 5.5 Cross-run ROLLBACK depends on a PERSISTENT datastore — T-CHAN-1

Rollback protection (TUF 5.4.3.1) is enforced by `tough`'s `load_timestamp` against the
**persisted datastore** copy of `timestamp.json`, not against in-memory or publisher-side
state. The test publishes a validly-signed v2 repository at fixed URLs, **loads** it with a
pre-created persistent datastore (which writes `timestamp.json` v2), then republishes a
validly-signed **v1** set at the **same** URLs with the **same** role keys and **same**
trusted root. A second `load` with the **same** datastore path is refused:

```
tough::Error::OlderMetadata { role: Timestamp, current_version: 2, new_version: 1 }
```

(`current_version` = the persisted datastore copy; `new_version` = the freshly fetched
one; the timestamp guard fires before the snapshot is fetched, so the refusal is
attributed to Timestamp.) A **genuinely fresh** datastore (no persisted timestamp)
accepts the identical old valid v1 repo — **proving cross-run rollback protection
depends entirely on persisted state, not on the metadata bytes alone.** Test:
`cross_run_rollback_refuses_older_timestamp_via_persisted_datastore`.

> **Anti-rollback is NOT free.** It requires a **persistent datastore** that survives
> across `pkg update` runs. Without it, `tough`'s rollback guard is never entered (no
> previously-seen `timestamp.json` to compare against) and an older-but-validly-signed
> metadata set is accepted. This is a **datastore responsibility**, not a property of the
> metadata. PR-11 must own a durable, single-writer datastore and surface "channel
> rollback refused" to the user (§8).

### 5.6 Safe expiry + monotonic last-known-time caveat

- The spike always loads with **`ExpirationEnforcement::Safe`** on the normal path.
- `Safe` mode maintains a **monotonic last-known-time** bookkeeping file
  (`latest_known_time.json`) in the datastore. In `Safe` mode `read_target` re-samples the
  wall clock for its expiry check and may legitimately advance this file's value on every
  read; this is benign monotonic-clock bookkeeping — **neither signed metadata nor target
  content** — and is the documented side effect of the `Safe` expiry check. Test §5.4
  asserts this is the **only** datastore file that changes during a tampered read.

### 5.7 MIX-AND-MATCH: validly-signed older snapshot spliced — refused for hash mismatch — T-CHAN-3

Mix-and-match defense (TUF step 3.1): the snapshot a client fetches MUST be the exact
bytes whose sha256 + length the trusted `timestamp.json` pinned — even if those bytes are
themselves a validly signed snapshot for a **different** (older) repository. The test
builds **two independently valid** signed repositories sharing the same trusted role keys:
Repo A (old, v1, one meta entry) and Repo B (new, v2, **two** meta entries so its snapshot
is strictly larger). It **splices** A's validly-signed v1 `snapshot.json` into B's
`2.snapshot.json` path (the exact path B's v2 timestamp requests). Because A's snapshot is
no larger than B's declared length, the `max_size_adapter` never trips; at end-of-stream
the `DigestAdapter` finds A's sha256 ≠ B's pinned sha256:

```
tough::Error::Transport { source: <TransportError, kind = Other> }
  └─ tough::Error::HashMismatch { calculated: <A's snapshot sha256>, expected: <B's snapshot sha256> }
```

This pins the cryptographic binding between timestamp and snapshot; a future `tough`
regression that fetched the snapshot by path alone (ignoring the pinned hash/length)
would turn this refusal into an accept. Test:
`mix_and_match_spliced_older_snapshot_refused_for_hash_mismatch`.

### 5.8 Root rotation N→N+1: DUAL authorization — T-CHAN-5, T-REL-2

Root rotation (TUF 5.3.4) requires **dual** authorization: v2 `root.json` must be signed
by (1) a threshold of keys in the **trusted** root (v1), AND (2) a threshold in the **new**
root (v2) itself. `tough`'s `load_root` runs **both** checks, in order, each via
`Root::verify_role`, wrapping any unmet threshold in `VerifyMetadata { role: Root }`. The
test serves a v2 that **removes** the old root key and authorizes a distinct new key, with
three signature sets on the same v2 payload:

- **(A) v2 signed only by the new key** → fails check (1), the OLD-root threshold: the new
  key is not in v1's role.
- **(B) v2 signed only by the old key** → passes (1) but fails (2), the NEW-root SELF
  threshold: the old key is not in v2's role.
- **(C) v2 signed by BOTH** → dual-authorized, loads, target reads.

Both failures (A) and (B) produce **identical** `SignatureThreshold` numbers (threshold 1,
valid 0); the test disambiguates structurally via key-membership facts. The exact variant
for the unmet checks:

```
tough::Error::VerifyMetadata { role: Root }
  └─ tough::schema::Error::SignatureThreshold { role: Root, threshold: 1, valid: 0 }
```

Test: `root_rotation_dual_authorization_threshold_checks`.

### 5.9 ACTUAL revocation: revoked old key can no longer sign a new root — T-CHAN-5, T-REL-2

After a **valid** dual-signed v1→v2 rotation, the old root key is fully **revoked**
(absent from v2/v3 root role **and** key map). A correctly new-key-signed v3 **loads**
through the v1→v2→v3 chain. But the **same** v3 payload signed **only by the revoked old
key** is rejected at the v2→v3 hop: `v2.verify_role(v3)` counts 0 valid signatures
(v2's role {new} does not authorize the old key):

```
tough::Error::VerifyMetadata { role: Root }
  └─ tough::schema::Error::SignatureThreshold { role: Root, threshold: 1, valid: 0 }
```

This is a **real revocation** test, not a missing-signature test: the rejected `3.root.json`
**carries** exactly one signature whose keyid **names** the old (revoked) key, and the old
key is provably absent from v2's and v3's key maps and root-role keyids. Test:
`root_rotation_revocation_rejects_signed_by_revoked_key`.

---

## 6. Happy-path & shape evidence (a)

### 6.1 Load & targets (`tests/load_and_targets.rs`, 6 tests)

1. `pinned_root_loads_happy_path` — pinned root + `FilesystemTransport` + `Safe` +
   `CONSERVATIVE_LIMITS` + persistent datastore loads through `tough`'s full client
   verification (root → timestamp → snapshot → targets → delegated targets).
2. `persistent_timestamp_and_snapshot_after_load` — `tough` **persists** `timestamp.json`
   + `snapshot.json` into the persistent datastore during load (the cross-run rollback
   memory; §5.5).
3. `read_top_level_targets_after_drain` — top-level targets (`descriptor.json`, a
   managed-Nix runtime) read back byte-for-byte after full drain. Nixpkgs is intentionally
   fetched separately as a pinned flake and is not a product TUF target.
4. `read_delegated_index_target` — **delegated** `index` role (1-of-1, `paths=["index/**"]`)
   walked + verified; both preview index targets read back byte-for-byte through
   the same hash check as a top-level target. (Delegated targets ARE supported and proven —
   see [`README.md`](README.md) §7.)
5. `missing_target_is_none` — an unadvertised target returns `Ok(None)`, the contract
   PR-11 uses to distinguish "missing" from "tampered".
6. `descriptor_per_system_maps_have_preview_systems_and_match_fixture_bytes` — both
   descriptor per-system maps carry exactly the preview systems, and every
   descriptor target name/hash matches the actual signed fixture bytes.

### 6.2 Serialization-shape guards (`src/descriptor.rs`, 5 unit tests)

`top_level_keys_are_exactly_canonical_and_ordered`,
`build_policy_native_local_builds_covers_preview_systems`,
`camel_case_required_paths_exist`, `no_snake_case_drift_in_serialized_bytes`,
`round_trip_sample_is_equal`. These are **strict serialization-shape guards only**
(camelCase key set/order, preview-system coverage, round-trip) plus an end-to-end read-back
that cross-checks declared hashes against fixture bytes — they add **no** production
policy validation (deferred to PR-11; [`README.md`](README.md) §10).

---

## 7. Supply-chain evidence

### 7.1 Exact `tough` pin + patched floors (AWS-2025-007, CVE-2026-6967 / AWS 2026-019)

`tough` is pinned **exactly** `=0.24.0` in [`Cargo.toml`](Cargo.toml)
(`default-features = false`); every recorded result in this document was produced
against that version. Two relevant advisories were checked against this pin:

- **[AWS-2025-007](https://aws.amazon.com/security/security-bulletins/AWS-2025-007/)**
  — an older `tough` set of issues **fixed by `0.20.0`**. The pinned `0.24.0` is above
  that floor.
- **[CVE-2026-6967](https://nvd.nist.gov/vuln/detail/CVE-2026-6967)** /
  **[AWS 2026-019](https://aws.amazon.com/security/security-bulletins/2026-019-aws/)**
  (also published as the `tough` security advisory
  [GHSA-4v58-8p28-2rq3](https://github.com/awslabs/tough/security/advisories/GHSA-4v58-8p28-2rq3))
  — affects `tough` **before `0.22.0`** and is **fixed by `0.22.0` or later**. The
  pinned `0.24.0` is above that floor as well.

So the exact pin **`0.24.0` is above both** patched floors and is therefore not affected
by either advisory. A locally refreshed RustSec `advisory-db` (**2026-08-05**) did **not**
report CVE-2026-6967, so the **direct primary advisories** (the awslabs/`tough` GHSA, the
AWS security bulletin, and the NVD entry linked above) were checked to confirm the patched
floors and that `0.24.0` is unaffected. The exact pin (`=0.24.0`, not a range) guarantees
a later resolver bump cannot drop onto an affected version.

### 7.2 `cargo audit` — success over 139 dependencies

```
$ cd spikes/s2-tough && RUSTUP_TOOLCHAIN=1.96.1 cargo audit --file Cargo.lock --deny warnings
      Loaded 1189 security advisories (from $CARGO_HOME/advisory-db)
    Updating crates.io index
    Scanning Cargo.lock for vulnerabilities (139 crate dependencies)
$ echo $?     # -> 0
```

**Success: 0 advisories matched across 139 crate dependencies** (exit 0). See §7.1 for the
CVE-2026-6967 note on why `cargo audit` alone is not the sole signal.

### 7.3 `cargo deny check` — all checks pass; only duplicate-version warnings

```
$ cd spikes/s2-tough && RUSTUP_TOOLCHAIN=1.96.1 cargo deny --manifest-path Cargo.toml --config ../../deny.toml --all-features --locked check
warning[duplicate]: found 2 duplicate entries for crate 'syn'
   syn 2.0.119 registry+https://github.com/rust-lang/crates.io-index
   syn 3.0.3    registry+https://github.com/rust-lang/crates.io-index
warning[duplicate]: found 2 duplicate entries for crate 'untrusted'
   untrusted 0.7.1 registry+https://github.com/rust-lang/crates.io-index
   untrusted 0.9.0 registry+https://github.com/rust-lang/crates.io-index
advisories ok, bans ok, licenses ok, sources ok
```

**All four check groups pass** (`advisories`, `bans`, `licenses`, `sources`). The **only**
diagnostics are transitive duplicate-version **warnings** (not errors;
`multiple-versions = "warn"` in [`deny.toml`](../../deny.toml)):

- **`syn 2.0.119` vs `3.0.3`** — `syn 2` and `syn 3` are two separate transitive
  proc-macro dependency families (each pulled in transitively by unrelated
  proc-macro crates); no specific per-crate attribution is made here.
- **`untrusted 0.7.1` vs `0.9.0`** — `aws-lc-rs` (and legacy `tough`) pull `0.7.1`;
  `rustls-webpki` pulls `0.9.0`.

Neither is actionable from this spike (both are transitive and benign), and neither is an
error.

---

## 8. Persistence, expiry, and what is NOT free (anti-rollback / freeze)

This spike proves `tough` *enforces* rollback/freeze **given the right runtime invariants**,
but those invariants are **not free** — they are duties PR-11 must own:

1. **A persistent datastore is REQUIRED for cross-run rollback memory** (§5.5). Without a
   durable, single-writer datastore path that survives across `pkg update` runs, `tough`'s
   rollback guard is never entered and an older-but-validly-signed metadata set is
   accepted. **Anti-rollback is a datastore responsibility, not a property of the metadata
   bytes.**
2. **`ExpirationEnforcement::Safe` is mandatory on normal paths** and refuses signed
   metadata past its `expires` field against the real wall clock (§5.2). `Unsafe` is
   prohibited there. PR-11 must never load with `Unsafe` on update/install.
3. **Descriptor product-semantic validation is PR-11's, not `tough`'s.** `tough` supplies
   the cryptographic/TUF guarantees for `descriptor.json` (authentication, integrity,
   rollback, freeze, mix-and-match, threshold). The product-semantic policy fields
   (`schemaVersion`, `policyVersion`, `sequence`, `expiresAt`, systems allowlists,
   `substituters`/`trustedPublicKeys` allowlists, and the cross-checks between descriptor
   hashes and TUF-authenticated target hashes) are **deferred to PR-11** ([`README.md`](README.md)
   §10). **`tough` does NOT check `descriptor.expiresAt` or any build-time value.**

---

## 9. `aws-lc-sys` native build implications

`tough 0.24.0` → `aws-lc-rs 1.17.3` → **`aws-lc-sys 0.43.0`**, which **compiles AWS-LC**
(a C/C++/assembly fork of BoringSSL) **from source** via CMake. This native build requires
a working **C/C++ compiler**, **CMake**, and **pkg-config** on the host:

| Platform | Native prerequisites |
|----------|----------------------|
| **macOS** | **Xcode Command Line Tools** (provides `clang`/`cc`/`libc++`) **plus CMake** (and `pkg-config`, e.g. via Homebrew) |
| **Linux** | **compiler/build essentials** (`build-essential` / `gcc-c++` providing `gcc`/`g++`/`make`) **plus CMake and `pkg-config`** |

**Implications for DR-002:**

- The production workspace at the repo root does **not** have this native build dependency
  in v1 (it has no crypto crate yet); it is introduced **only** by this spike's
  `tough`/`aws-lc-rs` dependency. If DR-002 is Accepted and PR-11 adopts `tough`, the
  production build gains this native build step (longer cold builds; CI images and
  contributor machines must carry a C/C++ toolchain + CMake + pkg-config).
- This is a recorded DR-002 consequence, not a v1 blocker; `aws-lc-rs` also ships prebuilt
  bindings that mitigate (but do not eliminate) the native compile.

---

## 10. Limitations / non-claims

This spike deliberately does **not** claim:

- That **anti-rollback / freeze protection is free** — it **requires a persistent
  datastore** (§5.5, §8) and **never** should be described as automatic.
- Any **product-semantic validation of descriptor fields** (§8) — all deferred to PR-11.
- That it proves endless-data protection for `max_root_size` / `max_targets_size` /
  `max_snapshot_size` — the size-limit test (§5.3) exercises `max_timestamp_size` **only**.
  The other caps are configured and applied, but only one is adversarially exercised.
- That `tough` checks `descriptor.expiresAt` or any build-time value — only the **signed
  TUF metadata** expiration, against the wall clock (§5.2).
- That it evaluates a **real HTTPS** transport — only `FilesystemTransport` (spike-only;
  PR-11 decides production transport).
- That the spike alone accepted DR-002 — acceptance occurred later, on 2026-08-09, after the
  recorded F+A review in §11 and [`plans/12`](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md).
- That it is production code — it is a standalone, `publish = false` spike.

---

## 11. Decision record status (DR-002) — Accepted 2026-08-09

The documented **success criterion** for S2 (`plans/12` §2) is: *"Signed fixture metadata
verified end-to-end; revocation dry-run passes; threshold demo."* Against that, **the
technical recommendation and evidence are complete** (a):

- ✅ Signed fixture metadata verified end-to-end through `tough`'s real client path — §5.7
  (mix-and-match), §6.1 (happy-path load + delegated targets).
- ✅ Revocation demonstrated with real `tough` refusals — §5.9 (a revoked old root key can
  no longer sign a new root) and §5.8 (dual-authorized rotation).
- ✅ Per-role threshold semantics demonstrated — §5.1 (differing per-role thresholds load
  when met; insufficient valid signatures refused, role-local).
- ✅ Plus rollback (§5.5), freeze/expiry (§5.2), endless-data (§5.3), drained-stream
  tamper (§5.4) — all with the exact `tough` error variants pinned.

**DR-002 was Accepted on 2026-08-09.** The F+A review re-ran the 20-test suite against the
exact `tough 0.24.0` lock and rechecked the loader/datastore/expiry/transport APIs against
current upstream documentation and source. It accepted the recommendation without weakening
the spike's limitations: persistent single-writer state remains mandatory, normal paths remain
`Safe`, target streams must be drained, and pkg owns semantic validation. The AC-D1 gate is
cleared; PR-11 and PR-33 still must pass their own production/security gates.

The crisp technical recommendation stands: **adopt the real TUF specification via the
`tough` crate (exactly `0.24.0`), `FilesystemTransport` for local loads (production
transport decided by PR-11), `ExpirationEnforcement::Safe` on normal paths, the
conservative `Limits`, a persistent single-writer datastore (REQUIRED for rollback), the
mandatory `read_target_fully` full-drain helper, and per-role thresholds (1-of-1 v1 →
2-of-3 at GA).**

---

## 12. Reproducible commands

All commands run from **this directory** (`spikes/s2-tough/`). The spike has its own
`Cargo.lock` and `target/` and is excluded from the root build/clippy/test/doc lanes.
If your shell has `RUSTUP_TOOLCHAIN` set, prefix with `RUSTUP_TOOLCHAIN=1.96.1` (or
`unset` it) so commands run on the pinned channel.

```sh
# Formatting (rustfmt; --check passed through to rustfmt):
RUSTUP_TOOLCHAIN=1.96.1 cargo fmt --all -- --check

# Type-check every target against the committed lockfile:
RUSTUP_TOOLCHAIN=1.96.1 cargo check --all-targets --all-features --locked

# All 20 tests (5 unit + 6 load/targets + 9 adversarial; 0 doc-tests) vs the lockfile:
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

Docs cross-reference checker — run from the **repo root** (it validates links across the
whole plan set, not this spike in isolation):

```sh
cd ../.. && python3 .github/scripts/check_docs_links.py
```

Tool versions: `cargo-audit 0.22.2`, `cargo-deny 0.20.2` (matching
[`ci-fast.yml`](../../.github/workflows/ci-fast.yml)).

---

## 13. Cross-references

- Decision record: [`plans/12` DR-002](../../plans/archive/2026-08-22-custom-managed-nix-v1/12-open-decisions-and-risks.md)
- This spike's README (purpose, prereqs, commands, threat matrix, claims):
  [`README.md`](README.md)
- Channel/TUF design: [`plans/02`](../../plans/archive/2026-08-22-custom-managed-nix-v1/02-trust-and-update-model.md) §7
  (canonical descriptor schema), §6.5 (TUF roles)
- Threat model: [`plans/08`](../../plans/archive/2026-08-22-custom-managed-nix-v1/08-security-model.md) §6.5 (T-CHAN-*), §7 (crypto)
- PR roadmap: [`plans/11`](../../plans/archive/2026-08-22-custom-managed-nix-v1/11-pr-roadmap.md) PR-5 (this spike), PR-11
  (production client, gated)
