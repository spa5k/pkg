# Local public-tap import: end-to-end design

Status: updated 29 September 2026 to the accepted implementation contract.
This document replaces the earlier maintainer-only design. Nothing in it is
implemented yet. The [implementation contract](implementation-contract.md)
is authoritative where they could disagree.

## 1. Decision

Import public taps **locally, after explicit consent, through Nix-provided
native Ruby inside an ordinary sandboxed Nix derivation**. Rust keeps
validation and catalog generation. Nix keeps builds, installed state, and
the package lifecycle. The client keeps consent, the tap registry, source
routing, and publishing.

| Approach | What we must own | Decision |
| --- | --- | --- |
| Rust parses and evaluates Cask Ruby | DSL rules, Ruby expressions, helpers, platform branches, upstream changes | Reject. This becomes a partial Ruby interpreter. |
| Hosted API service that pre-converts taps | A service, its trust, availability, and every tap's privacy | Reject. No service ships. |
| WASM conversion runtime in the client | A second runtime, sandbox engineering, Ruby gem ports | Reject. Not part of this change. |
| Install Homebrew/Brew on the user's machine | Brew state and a second package lifecycle | Reject. Nix retains installed state. |
| Local import: pinned public fetch plus sandboxed Nix-provided Ruby | Consent, fetch policy, one sandbox contract, one importer library | Accepted contract. |

The Ruby is pinned by Nix, together with the upstream Homebrew reader
dependencies. Tap Ruby never runs in a Nix shell, in the client process, in
a derivation that fetches, or with relaxed sandbox settings. It runs in a
plain sandboxed derivation whose inputs are fixed before evaluation starts.

## 2. Scope and user flows

Supported targets stay `aarch64-darwin` and `x86_64-linux`. Casks must
actually support the selected target.

The supported artifact subset is generic: GitHub release archives, raw
release binaries, app bundles, and the existing safe artifact plans.
Installer scripts, payload-dependent metadata, unsupported dependencies,
and active installer actions are rejected explicitly with recorded reasons.
A source repository being public does not imply support, and this plan does
not promise that every public tap imports successfully.

**Consumer:** approve a tap once, then search saved catalogs and install
qualified entries through the existing native lifecycle. Search never runs
Ruby and never prompts. No raw repository is evaluated during
`pkg install`.

**Tap owner of last resort — the user:** in the first version, `SOURCE`
is an `owner/tap` repository on public GitHub, and only that form. `pkg
tap add` accepts it after consent, fetch policy, and the sandbox contract.
Arbitrary HTTPS URLs, other hosts, and non-GitHub sources are out of
scope for the first version; they are not promised and must not be
implied. Taps are added through configuration and consent, not by adding
app names to Rust or Nix code.

**Independent publisher:** the retained official JSON source and the same
generated-flake mechanism still allow a publisher to commit a catalog flake
that clients consume through their existing Cask source configuration. No
central hosted API is required for that flow.

## 3. One data flow

```text
pkg tap add SOURCE [--revision SHA] [--trust]
        |
  consent gate                no Ruby before it
        |
  credential-free public fetch        network, policy-checked
        |
  pinned source tree + pinned Nix Ruby + pinned Homebrew reader
        |
  ordinary sandboxed Nix derivation   network denied in the derivation
  runs the upstream reader over the pinned tree
        |
  captured metadata (untrusted bytes) + bounded diagnostics
        |
  offline Rust validation / classification     no Ruby
        |
  schema-3 catalog + generated flake in a caller-owned staging dir
        |
  client validates, then publishes by atomic directory exchange
        |
  ordinary pkg / Nix lifecycle
```

Fetching is separate from Ruby evaluation. The fetch phase obtains the
public source tree with policy checks and no inherited credentials. The
evaluation phase runs pinned Ruby in a sandboxed derivation with no network.
Validation and generation consume captured bytes only. A failure anywhere
before publication leaves the previous catalog intact; publication swaps
one complete source directory in a single atomic step (see section 10).

## 4. Library seam and ownership

The backend exposes the importer as library code usable by the client; no
separately installed generator executable is required. The seam is
`cask_catalog::tap`.

- The request includes the canonical source, an optional exact revision
  (otherwise the current default branch is resolved), the native target,
  the Nix executable, and a caller-owned staging output directory.
- The successful result contains the source identity, the exact revision,
  captured metadata provenance, the generated flake path, and
  eligible/excluded counts.
- The importer never publishes the client's live registry. The client
  publishes only after validation succeeds.

Exact Rust signatures are agreed and recorded; the
[implementation contract](implementation-contract.md) stays authoritative:

```text
cask_catalog::tap::{
    ImportRequest {
        source: String,           // owner/tap on public GitHub
        revision: Option<String>, // exact 40-hex commit, else default branch
        system: String,           // x86_64-linux or aarch64-darwin
        nix: PathBuf,             // real Nix executable path
        output_dir: PathBuf,      // caller-owned staging directory
    },
    import(&ImportRequest) -> Result<ImportResult, String>,
}
```

The CLI mirror is `cask-catalog import-tap --source owner/tap --revision
<40hex> --system x86_64-linux --nix /abs/nix --out /abs/staging`. On
success, stdout is `ImportResult` JSON containing `flake_path`; the
catalog is at `flake_path/catalog/catalog.json`. There is no
fixture-runner or production local-source bypass interface; verification
must not invent one. No integration result may be recorded as run until
the parent executes the real interface in a clean sandbox. The two worker
leads must also exchange the precise schema-3 envelope before changing
decoders.

Ownership under the accepted contract:

| Area | Owner |
| --- | --- |
| `tools/cask-catalog`, `nix/casks`, raw-reader Nix/Ruby assets, backend fixtures | Backend worker |
| `crates/pkg-cli`: consent, tap registry, source routing, native lifecycle integration, client docs, focused client tests | Client worker |
| Integration, Cargo.lock reconciliation, source review, native verification, evidence, PR | Parent |
| This OpenSpec change, readable plan and HTML mirror, `tools/public-tap-check`, verification pages | Documentation worker |

Do not revert another worker's changes.

## 5. Tap registry, consent, and client behavior

Client commands:

```text
pkg tap add SOURCE [--revision SHA] [--trust]
pkg tap list
pkg tap update [SOURCE] [--revision SHA]
pkg tap remove SOURCE
```

- `add` resolves and shows the canonical repository and the approval scope,
  obtains first consent, then imports. Interactive consent is remembered per
  origin. A noninteractive run must pass `--trust` explicitly; it never
  silently proceeds.
- `list` shows saved sources with their pinned revisions.
- `update` refreshes one source or all saved sources, optionally pinned to
  an exact revision.
- `remove SOURCE` prevents future use of that source but does not remove
  installed packages.

Consent precedes import. Refusing consent runs no Ruby and changes nothing.
A qualified install that names an unknown source may offer or instruct the
explicit first-source `add` flow; it never reinterprets an unknown qualified
source as Nix code, and it does not need to autoimport it.

State is split by design. The tap registry stores origin and consent only,
never revision state. Each published generation stores the exact revision
and capture provenance for its source. An upgrade records a new generation;
it does not rewrite consent.

No prompt and no Ruby execution occur during ordinary search of saved
catalogs. Bare cask tokens resolve only when unique across all saved
catalogs and official entries, and uniqueness counts excluded matches too.
An excluded entry must not cause silent selection of another source, and
no source ordering creates precedence. Multiple matches return qualified
choices. A qualified ID resolves only within its named source and never
falls back to another source. A saved source catalog that is unreadable or
malformed makes bare-token resolution fail for the whole pass, even when
exactly one other saved source has the token and is eligible: the client
reports the failed source and must not ignore it and choose the healthy
one.

## 6. Fetch policy

The fetch phase obtains public HTTPS source trees. It does not inherit
credentials: no hooks, external filters, submodules, or ambient
authentication ride along, and no credentials reach public repository or
source input fetches.

- Resolve and record the canonical repository. Verify redirect and
  destination policy with a real URL parser; do not let DNS or redirect
  checks be bypassed by string tricks.
- Refuse local and private-network destinations for untrusted URLs.
- Disable local Git hooks and external checkout filters. Do not initialize
  submodules.
- Pin whole source content, runtime, policy, and target together. The cache
  unit is the whole source tree: a helper-only commit changes identity and
  invalidates cached output. Do not cache per cask file.
- Record source-relative file provenance when reading Ruby. Reject paths
  that escape the source root and duplicate tokens within one source.

The retained official JSON source flows through the same source identity
rules as schema-3 source `homebrew/cask`. It keeps its existing fetch
contract; it is not forced to a raw-official replacement.

## 7. Sandboxed Ruby evaluation

Nix supplies pinned Ruby and the upstream Homebrew reader dependencies. The
reader runs inside an ordinary sandboxed Nix derivation:

- The derivation is not a fixed-output derivation. Sandbox is enabled.
  Refuse sandbox fallback, relaxed mode, and added host paths.
- Verify effective daemon isolation with behavior probes on each supported
  platform before claiming safety. Do not infer macOS policy from Linux.
- Verified requirements (parent Nix sandbox evidence, 2026-09-29; raw logs
  to be copied under `docs/verification/public-taps-2026-09-29` as
  `sandbox-linux.log`, `sandbox-macos-negative.log`, and
  `sandbox-macos-positive.log`). On Linux, an ordinary strict build denied
  a world-readable host-canary read, a world-writable outside-directory
  write, and a positively controlled live host-loopback connect. On macOS,
  the daemon as shipped ran with `sandbox = false`, an ordinary user's
  `--option sandbox true` was ignored, and all three probes were allowed;
  the final disposable-VM proof denied all three **only** after the daemon
  ran with `sandbox = true`, `sandbox-fallback = false`, a minimal
  `sandbox-paths` set, the derivation set `__darwinAllowLocalNetworking =
  false`, and the LaunchDaemon `EnvironmentVariables` gave builds a
  private `TMPDIR` (`/nix/var/nix/builds`). The default Darwin global tmp
  is granted to builds and can expose `/tmp` file reads and writes.
  Nested `sandbox-exec` failed and is not a solution.
- These daemon settings are documented prerequisites, applied by the
  person who owns the host. The product never edits host daemon
  configuration automatically and never adds root-equivalent
  `trusted-users` entries as a workaround. It fails closed and explains
  the prerequisites when isolation is insufficient.
- A behavioral gate probes denial (writes, out-of-build reads, network)
  before **every** raw import, using a fresh nonce and fresh paths, so a
  cached earlier success can never mask a changed daemon. If denial is
  insufficient, the reader does not run and no import result is produced.
- Do not pass user home, secrets, daemon sockets, or model credentials into
  builds. Public source and source-input fetches must not inherit
  credentials.
- Apply bounded execution and output limits: wall time, memory, processes,
  writable disk, captured diagnostics, and result size.
- A failure must not create a successful empty catalog. A timeout or crash
  is recorded as a failure with bounded diagnostics, never as an eligible
  empty record.
- Captured JSON remains untrusted input. Only the fixed Nix builders
  consume validated data. Never evaluate Nix files or configuration
  supplied by a tap.

Strict sandbox proof runs on real native hosts before any safety claim.
Worker commands carrying model credentials never execute raw tap Ruby;
separate clean verification sandboxes without Pi/model keys do.

## 8. Conversion rules and the supported subset

Eligible entries use one of the generic shapes: GitHub release archives,
raw release binaries, app bundles, or the existing safe artifact plans.
Payload checksums are required. Archive traversal and symlink protection
and script rejection are retained. There is no sudo, no silent quarantine
removal, and no replacement of vendor signatures. On macOS, assessment
happens at the actual app exposure/launch path; metadata approval never
certifies the app or provides application runtime sandboxing.

Excluded with recorded reasons, never silently eligible:

- Installer scripts and active installer actions.
- Payload-dependent metadata. The upstream staging accessor used during
  DSL evaluation makes the reader load fail; the importer maps that to an
  excluded `ruby-load-error` entry whose bounded detail contains
  `staged_path` / `staged-path-during-source-eval`, sticky even when the
  cask rescues the error. Do not download an application to make this
  phase succeed. The guard is conservative; it may refuse harmless
  staging-path use, and it does not prove general Ruby purity.
- Unsupported dependencies, custom download strategies, credentials, live
  network lookups, helpers outside the pinned tree, and missing required
  metadata.

Legacy flight blocks and structured `preflight_steps`/`postflight_steps`
are preserved in metadata as explicit capability flags when the record
loads. The reader runs in normal mode: `HOMEBREW_DEVELOPER` is
explicitly unset; upstream may still print a developer-command warning,
which is not a failure, and internal path loading is explicitly
permitted for our staged trusted reader. Under normal mode the legacy
preflight/postflight callbacks load; the final actual converter excludes
such records as `unsupported-artifact` with a detail naming the callback
keys (not `installer-script`, not `ruby-load-error`). Structured steps
and installer scripts are excluded as `installer-script`. Only metadata
that actually loads can carry callback or structured-step markers.
Required install actions that no generic builder reproduces
yield an exclusion; they never become eligible. A successful load is
not sufficient for eligibility. Uninstall and zap behavior
stay outside our lifecycle and are never executed. Per-record load errors
become excluded entries with stable error codes; source-wide failures abort
the refresh.

Platform and language context is explicit. OS, arch, and language branches
evaluate under the declared native target and recorded language policy.
Missing or incomplete target evidence is excluded or fails the source; it
is never guessed.

## 9. Identity and catalog schema 3

Every entry has separate fields:

```text
identity:  owner/tap/token     (map key)
source:    owner/tap
token:     bare token
origin:    source-relative file path + file hash
targets:   the existing per-system status and plan shape
```

- `entries` is keyed by the identity. `inputs` is keyed by source and
  carries lock provenance. `pkg-cask-catalog/3` is the only supported
  envelope. No schema-2 compatibility layer is required.
- The identity is exposed in Nix as one quoted attribute segment holding
  the deterministic **escaped full identity**, for example
  `packages.aarch64-darwin."example/tap/tool@1_d_2"`. Escape in one pass
  over the original characters of the identity: `_` becomes `_u_`, `.`
  becomes `_d_`, and every other character stays unchanged. Never
  re-replace underscores inserted by the pass. Example:
  `example/tap/tool@1.2` maps to the physical attribute
  `example/tap/tool@1_d_2`. A normal token such as `owner/tap/token`
  needs no escaping. The mapping is reversible, so catalog and public
  search map keys keep the original identity. Do not turn `/` into a
  filesystem path or nested lookup. Validate each identity segment and
  use one quoting routine for CLI installables. Profile human labels are
  decoded for display.
- Store package names stay separate: a safe token plus a stable source
  suffix. Package identity does not change when the source revision
  changes.
- Two sources may both publish the same bare token; both identities exist.
  The same full identity twice is a whole-input integrity error. A source
  rename changes identity; there is no automatic migration to a similarly
  named repository.
- `info` and search expose the source, revision, path, and exclusion
  reason.

## 10. Native lifecycle

Each saved source publishes into one stable, **real** directory. Nix
rejects a flake whose root is a symlink (parent probes: Nix 2.35.2 with
Determinate 3.22.5 on Linux and 3.22.1 on macOS), so the client never
publishes a symlinked flake root.

Publication exchanges the complete new source directory with the previous
one in a single atomic step: `renameat2` with `RENAME_EXCHANGE` on Linux
and `renameatx_np` with `RENAME_EXCHANGE` on macOS, from same-filesystem
staging, while holding the source lock. The old directory is preserved.
If the filesystem does not support the exchange, publication fails. There
is deliberately no two-rename fallback, because it opens a window where
the source path is missing or partial.

Installation, upgrade, rollback, and removal use the native profile and the
existing lifecycle. Updates use stable generated flake references, so
upgrades see later catalog commits while rollback returns to the prior
native generation. Old generation outputs are preserved. Removing a tap
prevents future source use but does not remove installed packages; a later
upgrade may report that the old attribute is unavailable, and no package is
ever moved to another tap with the same token. No compatibility migration
database is introduced.

Parent native probes on both operating systems established: Nix rejects a
symlink flake root, accepts a stable real directory, and one install, an
upgrade to a second revision, and a rollback preserve the original
installable URL. A later probe pair (C0.7) passed native install,
upgrade, and rollback with the escaped physical attribute on both Linux
and macOS. A native manifest strips quotes from attributes, so a raw
dotted full identity installs but breaks native upgrade as a nested
lookup; the escaped attribute avoids that. These are **Nix-only
lifecycle observations**. They are not app integration, not proof of
the atomic exchange, and not sandbox evidence. The product's own
encoding tests remain unimplemented; the escaped-attribute product test
(task 3.4) stays open.

## 11. Repository changes and deletion budget

| Location | Change | Owner |
| --- | --- | --- |
| `tools/cask-catalog` (library `cask_catalog::tap` and friends) | Importer seam, fetch policy, sandboxed reader invocation, capture, classification, schema-3 emission. | Backend |
| `nix/casks` | Pinned Ruby/reader environment, ordinary sandboxed derivation for reader evaluation, fixed generic builders unchanged in artifact contract, schema-3 index projection, retained official JSON source in schema 3. | Backend |
| `crates/pkg-cli` | `pkg tap add/list/update/remove`, consent store, tap registry, source routing, strict schema-3 decode, qualified identity resolution, atomic publish, native lifecycle integration. | Client |
| `tools/public-tap-check` | Data-only adversarial fixtures and metadata matrix; reuses `tools/cask-build-check`. | Documentation |
| `docs/plans`, `docs/verification`, OpenSpec change, HTML mirror | This scope, evidence pages, verification instructions. | Documentation |

Delete at cutover: the schema-2 decoder and its obsolete fixtures, the
single-input runtime assumptions that conflict with `inputs`, and any
mirror-only assertions that block the retained official source. Do not
delete the official JSON reader; it remains a source through the common
interface. Do not add a generic plugin system, dependency closure engine,
per-tap subclasses, a second classifier, or schema-2 migration.

## 12. Verification and delivery

Delivery is one reviewable implementation PR. Do not merge or release
without a later instruction.

Verification uses E2B GLM 5.3 for implementation, the prepared Rust/Nix
snapshot, and — for raw Ruby execution — separate clean verification
sandboxes without Pi/model keys. The parent verifies code and integration
independently.

Exercise: real pinned public taps, helper-only changes, GitHub release
binaries, platform branches, token collisions, cache invalidation, consent
refusal, atomic refresh failure, malicious Ruby, URL redirects, filesystem
and network denial, malformed archives, unsupported scripts, and the native
lifecycle. Use metadata-only catalog comparisons and synthetic payloads for
broad coverage. Limit real vendor downloads to a small native sample.
Strict sandbox proof on real native hosts: denied network, denied
host-home read, denied supplied-credential sentinel, output exhaustion
containment, and helper-only cache invalidation have all passed on both
supported platforms with positive controls (evidence records sections
CRED, R2, DL, and C5; logs `sandbox-linux.log`,
`sandbox-macos-negative.log`, `sandbox-macos-positive.log` under
`docs/verification/public-taps-2026-09-29`). Parent Nix sandbox evidence
stays Nix-only background. The final normal-mode whole import of all
seven pinned sources passed on BOTH hosts (Linux: 30 records, 13
eligible / 17 excluded; macOS: 30 records, 7 eligible / 23 excluded,
first three sources before the GitHub rate-limit reset and the last
four after it; every row's offline checker and derivation evaluation
returned 0). The converter capture regressions (7/7), the
backend suite, the client unit suites on both OS, the interactive
consent input gate on both hosts, the actual Linux product negative
gate (C6/C7 Linux), the official schema-3 retention and replay,
and the evaluation of every eligible official derivation (3303, zero
payloads, identical result sha256 after the formatting follow-up) all
passed. The final native macOS C6/C7 negative-gate record also passed
(4 cases: strict local config refused `sandbox = false`, relaxed, and
fallback before any Ruby; the deliberately public daemon `TMPDIR=/tmp`
caused the behavioral reader probe to report FAIL — host read ALLOWED —
with refusal before export, no flake, daemon restored, vendor downloads
0; harness setup failures are not product results). The independent
parent review is complete (all requested fixes done). The only pending
item is the implementation PR. Reader-stage success is not source acceptance.

The data-only adversarial fixtures, the adversarial case matrix, the
seven-source public-tap metadata matrix (`public-sources.json`), and the
evidence instructions live in `tools/public-tap-check` and
`docs/verification/public-taps-2026-09-29`. Execution status is
recorded in the evidence pages: the final stages passed (final
whole imports on both hosts, reader and gate probes, converter and
client suites,
consent gate, lifecycle, official retention, replay, derivation
evaluation, and the final CI and post-format lint records); the
remaining pending checks are listed there.

Run Rust, Ruby, Python, Nix, and workflow lint for changed files, strict
OpenSpec validation, docs and static HTML checks, normal CI, and
independent review. Record actual evidence and limits.

## 13. Focused failure cases

| Case | Required result |
| --- | --- |
| Interpolation, `arch`, OS blocks, language blocks, local `require_relative` | Metadata agrees with the pinned upstream reader under the declared context; whole-tree identity tracks the helper. |
| Two taps publish the same bare token | Both qualified identities exist; bare shorthand is ambiguous. |
| One tap has an eligible entry and another an excluded entry with the same bare token | Bare shorthand stays ambiguous; no silent selection. |
| Same source emits a duplicate token or falsifies source identity | Refresh fails; previous catalog is unchanged. |
| Ruby attempts network access, reads the host home, reads a supplied credential sentinel, exhausts output, forks, or hangs | The sandbox and limits contain it; a bounded failure is recorded; no sentinel appears in captured output; never an empty success. |
| A cask uses staging data to construct artifacts | Excluded `ruby-load-error` with detail containing `staged_path` / `staged-path-during-source-eval`, sticky across rescue; no guessed empty artifact list. |
| Structured or legacy required install actions, installer scripts | Preserved as metadata and excluded; installer scripts rejected. |
| Missing platform evidence | Excluded or source failure; no guessed support. |
| Helper-only commit, exporter bump, target or policy change | Cached export invalidates. |
| Failed source, truncated output, missing target | No partial successful catalog replaces the published one. |
| One saved source catalog is unreadable or malformed | Bare-token resolution fails for the pass; the failed source is reported; no fallback to another source with the same token. |
| Consent refused or noninteractive without `--trust` | No Ruby runs; nothing changes. |
| Tap removed | Source blocked from future use; installed packages remain. |

## 14. What this plan does not promise

There is no guarantee that every public tap can be imported, that every
loaded cask becomes a Nix package, or that an eligible package has passed
GUI testing. Arbitrary Ruby, dynamic payload inspection, privileged steps,
installer scripts, unsupported dependencies, and host-specific behavior can
all prevent import. The sandbox contract is verified on real native hosts
before safety claims; until then no sandbox pass is claimed. The design
makes the limits visible and gives compatible taps one generic path to
native Nix packages.
