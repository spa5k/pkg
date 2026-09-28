# Raw Ruby and public taps: end-to-end design

Status: proposed. Nothing in this document is implemented by the plan commit.
Baseline: merged PR #81, `f6f99a748653861302876dfe127dfc6bf498d323`.

## 1. Decision

Use **a small pinned Homebrew exporter at catalog-update time**. Keep the
orchestrator and catalog compiler in Rust. Keep Nix as the build and install
engine. Keep the client free of Ruby and Homebrew.

This makes one explicit change to the earlier design: Ruby is allowed in the
maintainer export environment. It is still absent from the client archive,
client commands, Nix evaluation, and generated package builds.

| Approach | What we must own | Decision |
| --- | --- | --- |
| Rust parses and evaluates Cask Ruby | DSL rules, Ruby expressions, helpers, platform branches, and upstream changes | Reject for the first version. This becomes a partial Ruby interpreter. |
| Pinned upstream Ruby reader in an isolated update runner | Small export adapter, source pins, isolation, and output validation | Recommended. Reuse upstream semantics. |
| JSON snapshots only | Existing reader and pins | Retain as an input option. It cannot ingest a tap that publishes only Ruby. |
| Install Brew inside each user's workflow | Brew state and another package lifecycle | Outside the chosen product architecture. |

[Prism](https://github.com/ruby/prism) provides Ruby parsing and Rust
bindings. It does not provide Homebrew's runtime semantics. Using it would
still leave us to evaluate helper calls and platform logic. This is an
engineering inference from its documented parser interface.

The Homebrew export interfaces are internal. Pin the exact upstream source
and its Ruby dependencies. Upgrade them together, with focused comparison
checks. Do not copy a large selection of Homebrew files into our codebase or
maintain a permanent fork. Keep the upstream checkout in the tool cache and
keep its license notices. Our small exporter is the only integration code.

## 2. Scope and user flows

The supported targets stay `aarch64-darwin` and `x86_64-linux`. Casks must
actually support the selected target. Formula-only Linux taps do not become
Casks. Fonts, services, drivers, installer scripts, and dependency expansion
remain excluded. A source repository being public does not imply support.

**Consumer:** search the generated catalog, see the tap name and exclusion
reason, then install a qualified cask through the existing native lifecycle.
No raw repository is evaluated during `pkg install`.

**Catalog maintainer:** add one source entry, lock its revision, export its
metadata, generate a catalog, review the diff, and publish the Git change.
All casks under the configured Cask directory are discovered. No app names
are added to Rust or Nix code.

**Independent publisher:** run the same tool against a registry they own.
Commit the generated catalog into a flake that uses the generic builders.
Consumers point the existing `sources.casks` setting at that flake. This
allows public taps without adding them to our default catalog.

There is no instant `pkg tap add` that evaluates arbitrary Ruby on a user's
machine. A raw tap must first become generated data. A future client helper
may select an already-generated flake; that does not require a Ruby runtime.

## 3. One data flow

```text
sources.toml + pinned exporter environment
                  |
           Rust fetch / lock             network allowed
                  |
       immutable source trees
                  |
     isolated Homebrew Ruby export       network denied
       /                       \
 macOS ARM64                Linux x86-64
       \                       /
       hashed export snapshots
                  |
       Rust validation / generation      offline
                  |
        one generated catalog v3
                  |
       Nix flake + cheap search index
                  |
        ordinary pkg / Nix lifecycle
```

Only the trusted fetch phase uses the network. It obtains source trees and
the pinned exporter environment. Export does not fetch vendor applications,
request installers, run livecheck update scans, install gems, install tap
formulae, or invoke tap commands. Loading a livecheck or caveats declaration
can still execute Ruby; the isolated runner contains that load-time code.
Generation consumes captured bytes; it does not run Ruby.

## 4. Source registry and locks

Proposed files:

- `nix/casks/catalog/sources.toml`: desired source identities, URLs, refs,
  Cask directories, and declared licenses.
- `nix/casks/catalog/sources.lock.json`: exact revisions, tree/input hashes,
  exporter environment identity, target contexts, and export content hashes.
- `nix/casks/catalog/catalog.json`: one runtime artifact, including all
  authoritative input provenance. No runtime reader needs the registry.

Example registry shape. Values are illustrative, not selected source pins:

```toml
[[sources]]
id = "homebrew/cask"
kind = "ruby-tap"
url = "https://github.com/Homebrew/homebrew-cask.git"
ref = "HEAD"
cask_dir = "Casks"
license = "BSD-2-Clause"

[[sources]]
id = "vendor/tools"
kind = "ruby-tap"
url = "https://github.com/vendor/homebrew-tools.git"
ref = "HEAD"
cask_dir = "Casks"
license = "MIT"
```

Source IDs are stable, lowercase two-part names. They are configured
explicitly; they are not guessed from a redirect. Duplicate source IDs are
errors. The first version accepts public HTTPS Git sources and local cached
trees that match their lock. SSH credentials and private tap support are out
of scope. Git transport redirects must not silently change the locked source
identity. Source URL changes require a visible registry and lock diff.

Fetch resolves a moving ref to a commit, verifies the resulting checkout,
and records a canonical tree content hash. It disables local Git hooks and
external checkout filters. It does not initialize submodules. Export rejects
submodule dependencies and paths that escape the source root. Retain the
whole pinned tap tree in cache so local Ruby helpers can resolve. Discover
regular `.rb` files below `cask_dir` in sorted order; do not follow symlinks
outside it. Reject duplicate tokens within one source.

Record the tap's own license and origin. Do not label every public tap as
Homebrew BSD data. Unknown licensing remains explicit and blocks default
redistribution until the source metadata is resolved.

The JSON adapter receives the same source ID and exact input hash. It joins
the same compiler path after input decoding. It does not bypass validation.

## 5. Ruby exporter and its limits

Start from the upstream `generate-cask-api` loading pattern. The researched
revision uses `API.with_no_api_env`, `Cask.generating_hash!`,
`Cask::CaskLoader.load(path)`, and `to_hash_with_variations`. The simple
serializer is `to_h`; do not assume `Cask#to_hash` exists. See the
[pinned-source research](../../../docs/research/2026-09-29-raw-ruby-public-taps.md)
for exact paths and semantics.

For our native-per-target export, use a narrower sequence than the official
all-variation job. In a fresh target process, enter no-API mode and generation
mode, select the target and language, then load the explicit pinned file.
Call `cask.platform_supported?(tag, installable: true)` and serialize
`cask.to_h` while that context remains active. The predicate is the one
upstream uses for support evidence and does not refresh the Cask.

Do not run `to_hash_with_variations`, language-variation expansion, or
`refresh` in this native path. They re-evaluate DSL blocks without necessarily
reloading top-level statements. Capture a boolean
`upstreamPlatformSupported` with the exact target/tag. Do not invent a full
Homebrew `supported_platforms` array from one target run. The JSON adapter
keeps its existing complete-field/variation rules; both adapters produce
one effective target input for the same Rust classifier.

Materialize the pinned tap in Homebrew's expected tap layout inside the
disposable workspace. Keep it read-only. Configure any upstream tap-trust
decision only for that selected source in that workspace; never change host
trust settings. Use explicit file paths for loading. The adapter must verify
that Homebrew's reported tap/file identity agrees with the outer source lock.
The spike must prove this layout and trust setup against the pinned loader.

First prove this integration with a small exporter spike. Do not design a
new DSL reader around guessed method names. The spike must show that all
fields used by our classifier survive export, including platform evidence,
renames, URL options, dependency declarations, and active install operations.

Load each cask/target in a fresh Ruby process or clean child fork from a
trusted preloaded parent. No tap code runs before that parent is cloned.
This limits accidental constant and monkey-patch leakage. It is not the
security isolation between untrusted sources.

Use a native macOS ARM64 runner for the macOS export and a native Linux x86-64
runner for Linux. Homebrew target simulation affects Homebrew APIs; it does
not emulate arbitrary Ruby system calls, host files, or `RUBY_PLATFORM`.
Mac simulation selects a release family, not an exact patch version. Keep
the declared macOS baseline and the actual runner version in provenance.
Do not claim that a Linux-only export proves native macOS behavior.

The first version uses the pinned Homebrew default language policy. It does
not generate every locale variant. Locale, timezone, prefix, home directory,
and target settings are fixed in the runner recipe and recorded. Unknown
locale or host-dependent behavior is reported, not silently generalized.

### Metadata that needs a payload

Some public casks use `staged_path` to enumerate files inside a downloaded
application. With no payload, a directory glob can return an empty list.
That would hide a required manpage or binary.

The exporter must catch the upstream staging access in this case and emit
`staged-path-during-metadata`.
The spike will test a narrow guard on Homebrew's staging access during DSL
evaluation. Keep a sticky refusal flag even if a cask rescues the exception.
The guard may conservatively refuse harmless staging-path interpolation.
It must not interfere with ordinary upstream serialization.
Do not download an application to make this metadata-only phase succeed.
Do not add per-token exceptions. If a reliable guard cannot be demonstrated,
do not ship a claim of complete raw-tap conversion; narrow the supported
export contract and record this as an unresolved implementation blocker.

Arbitrary Ruby can inspect other host state without calling that accessor,
including through a manual path, `caskroom_path`, or a helper.
No finite guard proves complete semantic independence. Therefore support is
limited to metadata that resolves from the pinned source and declared runner
context. Exported snapshots are the reproducibility boundary. Raw revisions
alone are not a promise of deterministic Ruby execution.

### Active operations and unsupported data

Preserve legacy flight blocks and structured `preflight_steps` /
`postflight_steps` as explicit capability flags or metadata. Required install
actions that our builders do not reproduce yield an exclusion. A successful
`to_h` call is not sufficient for eligibility. Uninstall and zap behavior
remain outside our lifecycle and are never executed.

Custom download strategies, credentials, live network lookups, helpers
outside the pinned tree, or missing required metadata yield a precise
exclusion or source failure. Unknown active stanzas fail closed. A tap may
contain formulae, commands, and other files; none is invoked as a plugin.

## 6. Isolation and failure handling

Use the existing disposable E2B runner for Linux and a disposable native
macOS VM for Darwin. Build the pinned tool image before adding any tap.
Do not snapshot a VM after it has evaluated untrusted source code.

Prefer an ordinary sandboxed Nix derivation for the export command inside
each disposable runner. Nix can supply the pinned Ruby environment and
read-only dependencies. Enable sandboxing explicitly on both hosts; refuse
Linux sandbox fallback. Keep network fetches separate. Do not make the Ruby
export a fixed-output derivation or use a relaxed sandbox exception.
The [Nix sandbox reference](https://nix.dev/manual/nix/2.35/command-ref/conf-file.html#conf-sandbox)
documents the Linux network exception for fixed-output builds and the
different platform defaults. Verify the actual macOS network policy rather
than inferring it from Linux behavior.

This is an explicit maintainer build. The published flake reads only the
resulting JSON, so it still uses no import-from-derivation. VM and process
limits supplement Nix where it does not supply a resource quota.

The implementation spike must prove these runner properties:

- Public source export has no network access, model API key, GitHub token,
  signing key, developer home mount, SSH agent, or publishing credential.
- The pinned source tree and exporter toolchain are read-only. Writable
  space is temporary. Resource limits cover wall time, memory, process count,
  writable disk, and result size.
- Different sources have separate isolation. A subprocess alone is not a
  sandbox. Nix build isolation and the macOS VM network
  policy must be tested. Do not assume the E2B SDK's model-key handling alone
  enforces these properties.
- A trusted process outside that isolation validates result files. It checks
  expected source, target, token inventory, schema, bounded size, and hashes.
  The exporting VM cannot publish or replace another source's artifacts.

This is one runner contract, not a new sandbox platform. Use existing tools.
If a runner cannot enforce the contract, it does not run public tap Ruby.

Per-record syntax or unsupported metadata errors produce excluded entries
with stable error codes. A process timeout is recorded as an export failure,
not a successful empty cask. Missing expected records, an unavailable runner,
corrupt result files, changed runtime identity, or a source-wide crash abort
the update. They must not publish an apparently smaller successful catalog.

Generate in a temporary directory. Replace the runtime catalog only after
all requested sources and targets have valid results. Its embedded pins are
the runtime authority. Commit registry, lock, snapshots, and catalog together.
Check their agreement in CI. A failed update leaves the previous runtime
catalog byte-for-byte intact. Do not mix old source output with fresh output
without an explicit lock that identifies both.

## 7. Captured exports and reproducibility

Use a small versioned interchange document, `pkg-cask-source/1`. It contains
source identity, per-file path and hash, target context, exporter identity,
and either complete metadata or an exclusion for each discovered token and
target. It has no executable callback channel.

Record the upstream Brew commit, Ruby/runtime recipe digest, adapter version,
source commit/tree hash, and export content hash. Raw output and diagnostics
are bounded. Normalize report-only paths and stack traces outside catalog
data. Preserve license notices with captured artifacts.

Ruby can read time and other ambient state. A repeated export comparison
helps find instability but cannot prove arbitrary Ruby deterministic. The
strong contract is narrower: **the same captured exports and Rust compiler
version produce identical catalog bytes**. Published catalog regeneration
uses captured exports, not a fresh raw Ruby evaluation.

Reuse an export only when the entire source tree, exporter runtime, target
context, and export policy match its lock. Do not cache only by cask file:
another helper in the same repository can change its result. For the first
version, unchanged source/target pairs are the cache unit. No new distributed
cache service is needed. Keep captured JSON compressed in the catalog repo;
measure size before adopting a different artifact store.

## 8. Identity and catalog schema 3

Every entry has separate fields:

```text
id:       vendor/tools/editor
sourceId: vendor/tools
token:    editor
origin:   Casks/e/editor.rb + file hash
targets:  the existing per-system status and plan shape
```

`entries` is keyed by `id`. `inputs` is keyed by source ID and carries its
lock provenance. `pkg-cask-catalog/3` is the only newly supported envelope.
The generator, Nix projection, and Rust client decoder change together.
Remove the schema-2 decoder and its obsolete fixtures after cutover.

Qualified command shape:

```sh
pkg install cask:vendor/tools/editor
```

`cask:editor` is shorthand only when exactly one catalog entry has that token.
Count eligible and excluded matches; an excluded entry must not cause us to
silently select a different tap. Multiple matches return qualified choices.
Bare `editor` keeps existing Nixpkgs-versus-Cask ambiguity handling and adds
the qualified cask choices. Do not give the first source or official tap
implicit precedence. A qualified ID never falls back to another source.

Two different taps may both contain `editor`. Two records with the same full
ID are a whole-input integrity error. A source rename changes identity; there
is no automatic migration to a similarly named repository.

The Nix package key is one quoted attribute:

```text
packages.aarch64-darwin."vendor/tools/editor"
```

Do not turn `/` into a filesystem path or nested attribute lookup. Validate
each identity segment, and use one quoting routine for CLI installables.
Keep store package names separate: a safe token plus a stable source-ID hash
suffix. Check hash-suffix collisions during generation. Package identity
must not change when the source revision changes.

`info` and search JSON expose the source ID, raw source revision, path, and
exclusion reason. Existing profile storage remains authoritative. The moving
generated flake reference stays the install origin, so upgrades see later
catalog commits and rollback returns to the prior native generation.

Removing a source does not uninstall its packages. A later upgrade may report
that the old attribute is unavailable. Never move that package to another tap
with the same token. No compatibility migration database is introduced.

## 9. Repository changes and deletion budget

| Location | Change |
| --- | --- |
| `tools/cask-catalog/src/main.rs` | Extend the existing maintainer commands; keep CLI wiring small. |
| `tools/cask-catalog/src/source.rs` | Own registry, lock, identities, and captured-source decoding. |
| `tools/cask-catalog/export/` | Small pinned-upstream Ruby adapter and runner recipe. No vendored Homebrew fork. |
| `effective.rs`, `classify.rs`, `plan.rs`, `emit.rs` | Reuse one classifier and one plan builder; adapt inputs before classification. |
| `nix/casks/catalog/` | Replace the one-input pin with registry/lock/export artifacts and catalog v3. |
| `nix/casks/flake.nix` | Source-qualified package keys and v3 index projection. No Ruby, fetch-on-eval, or IFD. |
| `crates/pkg-cli/src/catalog/{id,cask,search}.rs` | Qualified identities, exact-token ambiguity, and source display. |
| `crates/pkg-cli/src/nix/manifest.rs` | Strict v3 decoding and required source provenance. |
| Existing verification tools | Accept qualified IDs; replay unchanged artifact plans. Reuse these scripts. |

Delete the hardcoded Brew API repository/hash defaults when registry-driven
fetch replaces them. Delete the old `input.json` consumer path, schema-2
decode, and mirror-specific CI URL assertion at cutover. Keep JSON input
support only through the common source interface. Do not add a generic plugin
system, dependency closure engine, per-tap subclasses, or second classifier.

## 10. Maintainer update workflow

Proposed commands, not commands available in the merged client:

```sh
cargo run -p cask-catalog -- fetch --sources sources.toml --lock sources.lock.json
cargo run -p cask-catalog -- export --lock sources.lock.json --out-dir captured
cargo run -p cask-catalog -- generate --lock sources.lock.json \
  --exports captured --out-dir nix/casks/catalog
```

`fetch` is the only phase that resolves moving refs. `export` dispatches to
the declared isolated native runners and records export hashes. `generate`
is offline Rust. Exact flag names may follow the existing Clap conventions;
the phase separation and failure rules are binding.

Review added/removed identities, source moves, newly excluded records, active
operations, and eligibility changes. Never write a token allowlist to make
coverage numbers larger. Updating source pins is a normal Git change.
No daemon or scheduled task is required by this plan.

## 11. Delivery order and acceptance

Use three implementation PRs after the plan. Each has a usable result.

### PR A — exporter and source interface

First prove the raw export shape, isolation, native target behavior, staging
guard, active-step preservation, and reproducibility limits on small inputs.
Then add registry/lock handling and captured exports to the Rust tool. Keep
the default published catalog unchanged while the new path is exercised.

Gate: a source can be fetched, exported for both targets, and classified
offline without an application download. One broken record is visible; a
source-wide failure cannot publish partial output. If this gate fails, stop
expanding the importer and record the unsupported contract precisely.

### PR B — namespaced catalog and client cutover

Change generation, Nix keys, source provenance, decoding, and exact-name
resolution together. Publish schema 3. Remove the schema-2 path and the
mirror-only fetch assumptions. Package names and original moving references
must survive update and rollback without crossing source identities.

Gate: two sources with the same token coexist; shorthand is ambiguous;
qualified installs select the requested source; offline catalog regeneration
is byte-identical; ordinary search never forces a derivation.

### PR C — public-tap pilot and official source cutover

Run the official tap plus at least three representative public taps through
the tool. Choose sources by supported examples and documented edge cases,
not by popularity alone. Source inspection candidates include Aerospace and
Wine taps; their active actions may correctly remain excluded.

Compare source inventory and normalized metadata with the pinned upstream
exporter. Use the previous top-1,000 ranking for an explicit coverage diff.
Evaluate every eligible derivation with IFD disabled. Replay plans with tiny
payloads. Run native install/use/remove and update/rollback for a small
representative set: one macOS app with CLI, one relocatable pkg if present,
one Linux binary, and one AppImage. Cap new vendor downloads at four; record
unavailable shapes instead of filling the set with unrelated apps.

Gate: no unexplained record disappearance, no silent loss of required steps,
all source/collision fixtures pass, and every coverage claim separates load,
eligibility, derivation evaluation, synthetic replay, and real installs.
Only then make raw Homebrew source the default and retire the default mirror.

## 12. Focused failure cases

Retain checks at the public import/compile interface. Do not add another
large assurance framework or duplicate the classifier in test code.

| Case | Required result |
| --- | --- |
| Interpolation, `arch`, OS blocks, version helpers, local `require_relative` | Metadata agrees with the pinned upstream reader under the selected context. |
| Two taps publish the same token | Both qualified IDs exist; shorthand is ambiguous. |
| Same source emits a duplicate token or falsifies source identity | Update fails; prior catalog is unchanged. |
| Ruby attempts network access, reads outside the allowed tree, forks without bound, or hangs | Isolation/limits contain it and a failure is recorded. |
| A cask uses staging data to construct artifacts | Excluded as payload-dependent; no guessed empty artifact list. |
| Structured or legacy required install actions | Preserved and excluded unless a generic builder implements the exact action. |
| Missing `supported_platforms` or incomplete target evidence | Excluded or source schema failure; no guessed platform support. |
| Helper-only commit, exporter bump, target context change | Source export cache invalidates. |
| Failed source, truncated output, missing target | No partial successful catalog replaces the published one. |
| Source disappears or moves | Existing installs remain; no automatic switch to another source. |

Use E2B GLM 5.3 for bounded implementation work as before. The parent owns
integration, source review, native macOS verification, and final evidence.
Keep model credentials out of the raw-source export runners. A prebuilt clean
tool image reduces setup cost; it is never a snapshot of an evaluated tap.

## 13. What this plan does not promise

There is no guarantee that every public tap can load offline, that every
loaded cask can become a Nix package, or that an eligible package has passed
GUI testing. Arbitrary Ruby, dynamic payload inspection, privileged steps,
unsupported dependencies, and host-specific behavior can all prevent import.
The design makes those limits visible and gives compatible taps one generic
path to native Nix packages.
