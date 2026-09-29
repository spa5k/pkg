# Local public-tap import: end-to-end design

Status: accepted 29 September 2026, implemented, and verified
([summary](../../../docs/verification/public-taps.md)). This document is
the concise technical source of truth. It keeps the accepted security
semantics. The full implementation contract is preserved at the immutable
baseline commit
<https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/openspec/changes/import-public-cask-taps/implementation-contract.md>.

## 1. Decision

Import public taps **locally, after explicit consent, through Nix-provided
native Ruby inside an ordinary sandboxed Nix derivation**. Rust keeps
validation and catalog generation. Nix keeps builds, installed state, and
the package lifecycle. The client keeps consent, the tap registry, source
routing, and publishing.

Rejected alternatives: parsing or evaluating Cask Ruby in Rust (a partial
Ruby interpreter), a hosted API conversion service, a WASM runtime in the
client, and installing Brew on the user's machine. The Ruby is pinned by
Nix together with the upstream Homebrew reader dependencies. Tap Ruby
never runs in a Nix shell, in the client process, in a derivation that
fetches, or with relaxed sandbox settings.

## 2. Scope and user flows

Supported targets stay `aarch64-darwin` and `x86_64-linux`. Casks must
actually support the selected target. The supported artifact subset is
generic: GitHub release archives, raw release binaries, app bundles, and
the existing safe artifact plans. Installer scripts, payload-dependent
metadata, unsupported dependencies, and active installer actions are
rejected explicitly with recorded reasons. A public repository does not
imply support, and not every public tap imports successfully.

**Consumer:** approve a tap once, then search saved catalogs and install
qualified entries through the existing native lifecycle. Ordinary search
and install never run Ruby and never prompt. No raw repository is
evaluated during `pkg install`; only explicit `tap add`/`tap update`
import.

**Source form:** `SOURCE` accepts `owner/tap` or its canonical
`https://github.com/owner/homebrew-tap` URL. GitHub is the only approved
canonical origin.
Arbitrary HTTPS URLs, other hosts, and non-GitHub sources are out of
scope; they are not promised and must not be implied. Taps are added
through configuration and consent, not by adding app names to Rust or
Nix code.

**Publisher:** the retained official JSON source and the same
generated-flake mechanism allow a publisher to commit a catalog flake.
No central hosted API is required.

## 3. Data flow

```text
pkg tap add SOURCE [--revision SHA] [--trust]
  consent gate                no Ruby before it
  credential-free public fetch        network, policy-checked
  pinned source tree + pinned Nix Ruby + pinned Homebrew reader
  ordinary sandboxed Nix derivation   network denied in the derivation
  captured metadata (untrusted bytes) + bounded diagnostics
  offline Rust validation / classification     no Ruby
  schema-3 catalog + generated flake in caller-owned staging dir
  client validates, then publishes by atomic directory exchange
  ordinary pkg / Nix lifecycle
```

Fetching is separate from Ruby evaluation. Validation and generation
consume captured bytes only. A failure anywhere before publication leaves
the previous catalog intact.

## 4. Library seam

The seam is `cask_catalog::tap`, exposed as library code usable by the
client; no separately installed generator executable is required.

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

The successful result contains source identity, exact revision, captured
metadata provenance, the generated flake path, and eligible/excluded
counts. The importer never publishes the client's live registry; the
client publishes only after validation succeeds. The CLI mirror is
`cask-catalog import-tap --source owner/tap --revision <40hex> --system
<system> --nix <abs-nix> --out <abs-staging>`; stdout is `ImportResult`
JSON containing `flake_path`. There is no fixture-runner or
production local-source bypass interface; do not invent one.

## 5. Registry, consent, and client behavior

- `add` resolves and shows the canonical repository and the approval
  scope, obtains first consent, then imports. Interactive consent is
  remembered per origin. First approval in a noninteractive run requires `--trust`
  explicitly; saved consent applies to later imports.
- `list` shows saved sources with their pinned revisions.
- `update` refreshes one source or all saved sources, optionally pinned
  to an exact revision.
- `remove SOURCE` prevents future use of that source but does not remove
  installed packages.

Consent precedes import. Refusing consent runs no Ruby and changes
nothing. The tap registry stores origin and consent only, never revision
state. Each published generation stores the exact revision and capture
provenance for its source. An upgrade records a new generation; it does
not rewrite consent.

No prompt and no Ruby execution occur during ordinary search. Bare cask
tokens resolve only when unique across all saved catalogs and official
entries, and uniqueness counts excluded matches too. An excluded entry
must not cause silent selection of another source, and no source ordering
creates precedence. Multiple matches return qualified choices. A
qualified ID resolves only within its named source and never falls back
to another source. A qualified install that names an unknown source
instructs the explicit first-source `add` flow only; it never reinterprets
an unknown qualified source as Nix code, and it does not
offer or autoimport it. A saved source catalog that is unreadable or malformed
makes bare-token resolution fail for the whole pass, even when exactly
one other saved source has the token and is eligible; the failed source
is reported and never silently ignored.

## 6. Fetch policy

The fetch phase obtains public HTTPS source trees without credentials: no
hooks, external filters, submodules, or ambient authentication ride
along.

- Resolve and record the canonical repository. Verify redirect and
  destination policy with a real URL parser over HTTPS on port 443 for
  public destinations; do not let DNS or redirect checks be bypassed by
  string tricks (pin the TLS socket, including DNS caching).
- Refuse local and private-network destinations for untrusted URLs.
- Pin whole source content, runtime, policy, and target together. The
  cache unit is the whole source tree: a helper-only commit changes
  identity and invalidates cached output. Do not cache per cask file.
- Record source-relative file provenance when reading Ruby. Reject paths
  that escape the source root and duplicate tokens within one source.

The retained official JSON source flows through the same source identity
rules as schema-3 source `homebrew/cask` and keeps its existing fetch
contract.

## 7. Sandboxed Ruby evaluation

Nix supplies pinned Ruby and the upstream Homebrew reader dependencies.
The reader runs inside an ordinary sandboxed Nix derivation:

- The derivation is an ordinary Nix derivation, not fixed-output, not a
  Ruby fetch. Sandbox is enabled. Refuse sandbox fallback, relaxed mode,
  and added host paths; there is no fallback through relaxed host paths.
- Verify effective daemon isolation with a behavioral probe on each
  supported platform before **every** raw import, using a fresh nonce and
  fresh paths, so a cached earlier success can never mask a changed
  daemon. If denial is insufficient, the reader does not run and no import
  result is produced.
- Do not infer macOS policy from Linux. On macOS, a private build
  `TMPDIR` for the daemon is a documented prerequisite, applied by the
  person who owns the host. The product never edits host daemon
  configuration automatically and never adds root-equivalent
  `trusted-users` entries. It fails closed and explains the prerequisites.
- Do not pass user home, secrets, daemon sockets, or model credentials
  into builds. Public source and source-input fetches must not inherit
  credentials.
- Apply bounded execution and output limits: wall time, memory,
  processes, writable disk, captured diagnostics, and result size.
- A failure must not create a successful empty catalog. A timeout or
  crash is recorded as a failure with bounded diagnostics, never as an
  eligible empty record. Empty, error, and inventory states fail closed.
- Captured JSON remains untrusted input. Only the fixed Nix builders
  consume validated data. Never evaluate Nix files or configuration
  supplied by a tap.

## 8. Conversion rules

Eligible entries use one of the generic shapes: GitHub release archives,
raw release binaries, app bundles, or the existing safe artifact plans.
Payload checksums are required. Archive traversal and symlink protection
and script rejection are retained. There is no sudo, no silent quarantine
removal, and no replacement of vendor signatures. On macOS, assessment
happens at the actual app exposure/launch path and is read-only: a
public vendor app is exposed only when `spctl --status` reports
`assessments enabled`, `codesign --verify --deep --strict` succeeds, and
the Gatekeeper assessment passes; the product never changes assessment
policy, quarantine attributes, or vendor signatures. Metadata approval
never certifies the app or provides application runtime sandboxing.

Excluded with recorded reasons, never silently eligible: installer
scripts and active installer actions (structured steps and installer
scripts are excluded as `installer-script`); payload-dependent metadata
(the upstream staging accessor makes the reader load fail; the importer
maps that to an excluded `ruby-load-error` whose bounded detail contains
`staged_path` / `staged-path-during-source-eval`, sticky even when the
cask rescues the error; legacy preflight/postflight loads are excluded as
`unsupported-artifact` with a detail naming the callback keys);
unsupported dependencies, custom download strategies, credentials, live
network lookups, helpers outside the pinned tree, and missing required
metadata.

Legacy flight blocks and structured `preflight_steps`/`postflight_steps`
are preserved in metadata as explicit capability flags when the record
loads. The reader runs in normal mode: `HOMEBREW_DEVELOPER` is explicitly
unset; upstream may still print a developer-command warning, which is not
a failure, and internal path loading is explicitly permitted for our
staged trusted reader. A successful load is not sufficient for
eligibility. Uninstall and zap behavior stay outside our lifecycle and
are never executed. Per-record load errors become excluded entries with
stable error codes; source-wide failures abort the refresh. OS, arch, and
language branches evaluate under the declared native target and recorded
language policy; missing or incomplete target evidence is excluded or
fails the source; it is never guessed.

## 9. Identity and catalog schema 3

```text
identity:  owner/tap/token     (map key)
source:    owner/tap
token:     bare token
origin:    source-relative file path + file hash
targets:   per-system status and plan shape
```

- `inputs[source].url` is the canonical approved repository URL;
  `kind` is `raw-ruby-tap`; `raw.archiveUrl` is the exact codeload URL;
  `raw.archiveSha256` and `raw.captureSha256` are recorded with the exact
  input revision; `targets` covers native targets only. Each result's
  `metadata_sha256` equals `raw.captureSha256`. The retained official
  source carries `raw = null`.
- `entries` is keyed by the identity. `inputs` is keyed by source and
  carries lock provenance. `pkg-cask-catalog/3` is the only supported
  envelope; there is no schema-2 compatibility layer.
- The identity is exposed in Nix as one quoted attribute segment holding
  the deterministic escaped full identity. Escape in one pass over the
  original characters: `_` becomes `_u_`, `.` becomes `_d_`, and every
  other character stays unchanged. Never re-replace underscores inserted
  by the pass. Example: `example/tap/tool@1.2` maps to the physical
  attribute `example/tap/tool@1_d_2`. The mapping is reversible, so
  catalog and public search map keys keep the original identity. Profile
  human labels are decoded for display.
- Store package names stay separate: a safe token plus a stable source
  suffix. Package identity does not change when the source revision
  changes.
- Two sources may both publish the same bare token; both identities
  exist. The same full identity twice is a whole-input integrity error. A
  source rename changes identity; there is no automatic migration.

## 10. Native lifecycle

Each saved source publishes into one stable, **real** directory. Nix
rejects a flake whose root is a symlink, so the client never publishes a
symlinked flake root. Publication exchanges the complete new source
directory with the previous one in a single atomic step: `renameat2` with
`RENAME_EXCHANGE` on Linux and `renameatx_np` with `RENAME_EXCHANGE` on
macOS, from same-filesystem staging, while holding the source lock. The
old directory is preserved. If the filesystem does not support the
exchange, publication fails. There is deliberately no two-rename
fallback, because it opens a window where the source path is missing or
partial.

Installation, upgrade, rollback, and removal use the native profile and
the existing lifecycle. Update references percent-encode the stable
path's filesystem bytes and leave path separators as separators; native
profile labels are decoded for display. Old generation outputs are
preserved. Removing a tap prevents future
source use but does not remove installed packages; an explicit upgrade
naming a removed-source entry refuses, and bulk upgrade passes only the
remaining exact entry IDs to Nix (removed-source upgrade guard), and no
package is ever moved to another tap with the same token.
No compatibility migration database is introduced.

## 11. Repository layout

| Location | Change |
| --- | --- |
| `tools/cask-catalog` | Importer seam, fetch policy, sandboxed reader invocation, capture, classification, schema-3 emission. |
| `nix/casks` | Pinned Ruby/reader environment, ordinary sandboxed derivation, fixed generic builders, schema-3 index projection, retained official JSON source. |
| `crates/pkg-cli` | `pkg tap add/list/update/remove`, consent store, tap registry, source routing, strict schema-3 decode, qualified identity resolution, atomic publish, native lifecycle integration. |
| `tools/public-tap-check` | Data-only adversarial fixtures and metadata matrix; reuses `tools/cask-build-check`. |

Deleted at cutover: the schema-2 decoder and its obsolete fixtures, the
single-input runtime assumptions that conflict with `inputs`, and
mirror-only assertions that block the retained official source. Do not
delete the official JSON reader. Do not add a generic plugin system,
dependency closure engine, per-tap subclasses, a second classifier, or
schema-2 migration.

## 12. Verification

Verification is summarized in
[docs/verification/public-taps.md](../../../docs/verification/public-taps.md).
Raw logs, archives, and the dated reports are immutable history at the
pinned baseline commit `913dedb3131a3f1db4c3ab385e3dc4ca495c21f3`. The
fixture harness, its safety contract, and the reusable checker commands
live in [`tools/public-tap-check/README.md`](../../../tools/public-tap-check/README.md).
Delivery is one reviewable implementation PR; do not merge or release
without a later instruction.

## 13. Focused failure cases

| Case | Required result |
| --- | --- |
| Interpolation, `arch`, OS blocks, language blocks, local `require_relative` | Metadata agrees with the pinned upstream reader under the declared context; whole-tree identity tracks the helper. |
| Two taps publish the same bare token | Both qualified identities exist; bare shorthand is ambiguous. |
| One tap eligible, another excluded, same bare token | Bare shorthand stays ambiguous; no silent selection. |
| Duplicate token or falsified source identity | Refresh fails; previous catalog unchanged. |
| Ruby attempts network, host-home read, credential sentinel, output exhaustion, fork, hang | Sandbox and limits contain it; bounded failure recorded; no sentinel in output; never an empty success. |
| Cask uses staging data | Excluded `ruby-load-error` with `staged_path` / `staged-path-during-source-eval` detail, sticky across rescue. |
| Structured or legacy required install actions, installer scripts | Preserved as metadata and excluded; installer scripts rejected. |
| Missing platform evidence | Excluded or source failure; no guessed support. |
| Helper-only commit, exporter bump, target or policy change | Cached export invalidates. |
| Failed source, truncated output, missing target | No partial successful catalog replaces the published one. |
| One saved source catalog unreadable or malformed | Bare-token resolution fails for the pass; the failed source is reported; no fallback. |
| Consent refused or noninteractive without `--trust` | No Ruby runs; nothing changes. |
| Tap removed | Source blocked from future use; installed packages remain. |

## 14. What this design does not promise

There is no guarantee that every public tap can be imported, that every
loaded cask becomes a Nix package, or that an eligible package has passed
GUI testing. Arbitrary Ruby, dynamic payload inspection, privileged
steps, installer scripts, unsupported dependencies, and host-specific
behavior can all prevent import. The design makes the limits visible and
gives compatible taps one generic path to native Nix packages.
