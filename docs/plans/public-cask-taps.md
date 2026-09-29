# Public cask taps: local approved import

Status: updated 29 September 2026 to the accepted implementation contract.
The [implementation contract](../../openspec/changes/import-public-cask-taps/implementation-contract.md)
is authoritative and supersedes the earlier maintainer-only plan, its forced
raw-official source replacement, and its three-PR delivery order. Baseline
plan code: merged PR #81, `f6f99a748653861302876dfe127dfc6bf498d323`.

Read the [offline HTML plan](../../artifacts/public-cask-taps-plan.html).
The full [OpenSpec design](../../openspec/changes/import-public-cask-taps/design.md)
is authoritative. The [task list](../../openspec/changes/import-public-cask-taps/tasks.md)
contains the implementation work; the sandbox-proof, replay, and
native-sample tasks that have final actual evidence are marked complete
there, and the rest stay open.

## Recommendation

Import public taps locally with explicit approval. Nix supplies pinned
native Ruby and the upstream Homebrew reader. Ruby runs only inside our
ordinary sandboxed Nix derivation. Rust validates captured metadata and
generates the catalog. Nix keeps builds and the installed state. The client
never installs Brew, never runs a hosted API, and never loads a WASM
runtime.

This avoids re-implementing Ruby and Homebrew semantics in Rust. It also
keeps the official JSON catalog: the raw-official replacement and mirror
retirement from the old plan are dropped.

## User-visible behavior

```text
pkg tap add SOURCE [--revision SHA] [--trust]
pkg tap list
pkg tap update [SOURCE] [--revision SHA]
pkg tap remove SOURCE
```

`add` shows the canonical repository and approval scope, then asks for
first consent. Consent is remembered per origin. Noninteractive runs pass
`--trust` explicitly or stop before any Ruby runs. Search of saved
catalogs never prompts and never runs Ruby.

In the first version, `SOURCE` is an `owner/tap` repository on public
GitHub, and only that form. Arbitrary HTTPS URLs and other hosts are not
promised. The tap registry stores origin and consent only; each published
generation stores the exact revision and capture provenance.

Qualified identities look like `owner/tap/token`. In Nix each identity is
exposed as one quoted attribute segment holding the deterministic
escaped full identity. Escape in one pass over the original characters:
`_` becomes `_u_`, `.` becomes `_d_`, and all other characters stay
unchanged; inserted underscores are never re-replaced. A normal token
such as `owner/tap/token` needs no escaping. The mapping is reversible,
and catalog and public search map keys keep the original identity.
Users still type `cask:owner/tap/token`; the internal encoding stays out
of the basic command flow. Profile human labels show the decoded
identity. A bare token resolves only when it is unique across every
saved catalog and the official entries, counting excluded entries as
matches; otherwise the client lists the qualified choices. A saved source
catalog that is unreadable or malformed makes bare-token resolution fail
for the whole pass: the client reports the failed source and never ignores
it in favor of another source with the same token. Removing a tap blocks
future use of that source but keeps installed packages.

The importer seam is agreed: `cask_catalog::tap::{ImportRequest, import}`
and the CLI mirror `cask-catalog import-tap --source owner/tap --revision
<40hex> --system <system> --nix <abs-nix> --out <abs-staging>`, whose
success prints `ImportResult` JSON with `flake_path`. Each saved source
publishes into a stable real directory (Nix rejects a symlinked flake
root) and is swapped by one atomic directory exchange with no two-rename
fallback.

## Supported subset

Eligible entries use generic shapes: GitHub release archives, raw release
binaries, app bundles, and the existing safe artifact plans. Payload
checksums are required. Installer scripts, payload-dependent metadata
(including staging-derived file lists), unsupported dependencies, and
active installer actions are excluded with recorded reasons. There is no
promise that every public tap imports.

## Safety boundaries

Fetching is separate from Ruby evaluation. Public fetches inherit no
credentials and are policy-checked against redirects and private-network
destinations. The reader derivation is an ordinary sandboxed derivation:
not fixed-output, no sandbox fallback, no relaxed mode, no added host
paths, no user home, secrets, sockets, or model credentials, and bounded
execution and output. A failure never becomes a successful empty catalog.
Captured metadata is untrusted input; only fixed generic Nix builders
consume validated data, and tap-supplied Nix is never evaluated.

Verified sandbox requirements (parent Nix evidence, 2026-09-29): on
Linux, an ordinary strict build denied a world-readable host-canary read,
a world-writable outside-directory write, and a positively controlled
live host-loopback connect. On macOS, the daemon as shipped ran with
`sandbox = false`, an ordinary user's `--option sandbox true` was
ignored, and all three probes were allowed; the final disposable-VM
proof denied all three only after the daemon ran with `sandbox = true`,
`sandbox-fallback = false`, a minimal `sandbox-paths` set, the derivation
set `__darwinAllowLocalNetworking = false`, and the LaunchDaemon gave
builds a private `TMPDIR` (`/nix/var/nix/builds`). The default Darwin
global tmp is granted to builds and can expose `/tmp` reads and writes;
nested `sandbox-exec` failed and is not a solution. Those daemon settings
are documented prerequisites applied by the host owner. The product fails
closed and explains the prerequisites, re-probes with a fresh nonce and
fresh paths before every raw import so a cached success can never mask a
changed daemon, never adds root-equivalent `trusted-users` entries, and
never alters a user's host daemon automatically. Raw logs will be copied
under `docs/verification/public-taps-2026-09-29` as `sandbox-linux.log`,
`sandbox-macos-negative.log`, and `sandbox-macos-positive.log`.

## Delivery

One reviewable implementation PR. The backend exposes the importer through
the `cask_catalog::tap` library seam; the client owns consent, the tap
registry, and atomic publication by directory exchange; the parent owns
integration and native verification. Verification executes the prepared
[`tools/public-tap-check`](../../tools/public-tap-check) fixture matrix
and the seven-source metadata matrix (`public-sources.json`, 30 casks,
payload budget 0) in clean sandboxes without model keys, reuses the
existing [`tools/cask-build-check`](../../tools/cask-build-check) archive
and plan tests, and records evidence under
[`docs/verification/public-taps-2026-09-29`](../verification/public-taps-2026-09-29).
No merge or release happens without a later instruction.

## Important limits

Tap Ruby can run code while it loads. Metadata approval never certifies an
application. macOS assessment happens at the real app exposure path, not in
the importer. GUI behavior stays untested by this flow. Final actual evidence
(2026-09-29): the native raw reader and its gate ran on both hosts over
all seven pinned taps and over the 20-case fixture matrix in normal
mode (`HOMEBREW_DEVELOPER` explicitly unset; the upstream
developer-command warning may still print); reader-gate network and
host-home denial, credential-sentinel containment, output-exhaustion
containment, per-cask deadline kill, and helper-only cache invalidation
all passed on both hosts; the actual native client lifecycle ran on both
hosts through the real `pkg` client (consent refusal without `--trust`,
qualified add/info/search/install, failed-update preservation, update,
explicit native upgrade, history/rollback, corrupt-capture repair, tap
removal with the installed binary retained, removed-source upgrade
guard and bulk skip, native uninstall); the converter capture
regressions (7/7), the backend suite, and the client unit suites passed
on both hosts; the interactive consent input gate passed on both hosts
as a real PTY test; the actual importer refuses Linux
`sandbox-fallback = true` before any Ruby; the official catalog
retention in schema 3, its full synthetic replay, and the evaluation of
every eligible official derivation (3303, zero payloads) passed; the
final normal-mode whole import of all seven pinned sources passed on
BOTH hosts (Linux: 30 records, 13 eligible / 17 excluded; macOS: 30
records, 7 eligible / 23 excluded, first three sources before the
GitHub rate-limit reset and the last four after it; every row's
offline checker and derivation evaluation returned 0). See the evidence
records: sections R, R2, DL, CRED, B, CONS, CLT, PKG, OR, DRV, and APP.
The negative product gates passed on both hosts (C6/C7 Linux and macOS;
the macOS record refused all three unsafe local configs before any Ruby
and failed closed on the deliberately public daemon `TMPDIR=/tmp` with
no flake, daemon restored, vendor downloads 0). The independent parent
review is complete with all requested fixes; only the implementation
PR stays open.
The public app assessment gate for public-tap vendor `.app` bundles
requires `spctl --status` to report `assessments enabled` before
codesign/spctl assessment; the exact-enabled status gate is unit-proven
on both OS, the actual native status was recorded as `assessments
enabled` with codesign and spctl assessment of the cached signed app
accepted (Notarized Developer ID), and the final check4 run proved
signed-Raycast exposure,
unsigned rejection, and launcher preservation. No native
globally-disabled-Gatekeeper retest is claimed (the reject-disabled
clause is unit-proven), and no GUI launch is claimed;
full GUI/runtime/app malware analysis is out of scope, not pending. A parent probe
established Nix-only lifecycle facts on both operating systems (Nix
2.35.2, Determinate 3.22.5 Linux and 3.22.1 macOS): a symlinked flake root
is rejected, a stable real directory is accepted, and install, upgrade,
rollback preserve the original installable URL. That observation is not
app integration, not atomic-exchange proof, and not sandbox evidence. The
[source research](../research/2026-09-29-raw-ruby-public-taps.md) and the
[security research](../research/2026-09-29-public-tap-security.md) record
concrete upstream interfaces, examples, and limits.
