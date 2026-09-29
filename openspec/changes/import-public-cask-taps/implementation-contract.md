# Accepted local public-tap implementation contract

Status: accepted for implementation on 29 September 2026. This records the
user's decisions after the original maintainer-only plan. This contract
supersedes conflicting text in that plan until its documents are updated.

## Outcome

`pkg` can add a public Homebrew Cask tap, obtain approval before its Ruby
runs, and convert a pinned revision locally into native Nix packages.
Nix supplies pinned Ruby and upstream Homebrew reader dependencies. Ruby
runs inside our ordinary sandboxed Nix derivation, not in a Nix shell.
No Brew installation lifecycle, hosted service, WASM runtime, per-app code,
or formula-to-cask conversion is required. Nix retains installed state.

Support ARM64 macOS and x86-64 Linux. A supported generic artifact subset
is sufficient: GitHub release archives, raw binaries, app bundles, and
existing safe artifact plans. Reject unsupported dependencies, active
installer actions, and payload-dependent metadata explicitly. Retain the
existing generated official catalog; replacing its JSON source with a raw
official export is not required to deliver local public taps.

## Ownership and shared interface

- Backend: `tools/cask-catalog`, `nix/casks`, raw-reader Nix/Ruby assets,
  and backend fixtures. Expose the importer as library code usable by the
  client; do not require a separately installed generator executable.
- Client: `crates/pkg-cli`, client docs, and focused client tests. Own
  consent, tap registry, source routing, and native lifecycle integration.
- Parent: integration, Cargo.lock reconciliation, source review, native
  verification, evidence, and PR. Documentation worker owns this OpenSpec
  change and the readable plan. Do not revert another worker's changes.

Use `cask_catalog::tap` as the library seam. Agree exact Rust signatures
between workers before integration. Its request includes canonical source,
optional exact revision (otherwise resolve the current default branch),
native target, Nix executable, and a caller-owned staging output directory.
Its successful result contains source identity, exact revision, captured
metadata provenance, generated flake path, and eligible/excluded counts.
It never publishes the client's live registry. The client publishes only
after validation succeeds. Failed refreshes preserve the previous source.

Emit schema `pkg-cask-catalog/3`. Use `inputs` with source provenance.
Each entry has `source`, bare `token`, and an identity `owner/tap/token`
as its map key. Expose that identity in Nix as one quoted attribute
segment holding the deterministic escaped full identity. Escape in one
pass over the original characters: `_` becomes `_u_`, `.` becomes `_d_`,
and every other character stays unchanged. Never re-replace underscores
inserted by the pass. Example: `example/tap/tool@1.2` becomes the physical
attribute `example/tap/tool@1_d_2`. A normal token such as
`owner/tap/token` needs no escaping. The mapping is reversible, so
catalog and public search map keys keep the original identity. The
physical attribute is never a claim that the identity equals the
attribute. Record source-relative file provenance when reading Ruby. Both worker
leads must exchange the precise envelope before changing decoders.
Generate the official JSON catalog in schema 3 with source `homebrew/cask`.
No schema-2 compatibility layer is required.

## Client behavior

Provide `pkg tap add`, `list`, `update`, and `remove`. A qualified install
may offer first-source import before resolution. Show canonical repository
and approval scope before executing any tap Ruby. Interactive consent is
remembered per origin; automation must explicitly opt in. No prompt or
Ruby execution occurs during ordinary search of saved catalogs. Removing
a tap prevents future source use but does not remove installed packages.

Keep profile human labels decoded: profiles display the decoded
identity, not the escaped physical attribute. A native manifest strips
quotes from attributes, so a raw dotted full identity installs but
breaks native upgrade as a nested lookup; the escaped attribute avoids
that. Parent Nix-only probes (2026-09-29) passed native install, upgrade,
and rollback with the escaped physical attribute on Linux and macOS.
This is a standalone Nix proof. Product coverage of the escaped attribute
comes from the `cask_catalog` module tests plus the native encoded-attribute
lifecycle runs on both hosts (install, update, upgrade, rollback —
`lifecycle-*-encoded.log`, `lifecycle-*-safeattr.log`); source/token
accepted characters do not broaden.

Use stable generated flake references for explicit updates and the native
profile for installation, upgrade, rollback, and removal. Preserve old
generation outputs. Bare cask tokens resolve only when unique across all
saved catalogs and official entries, including excluded matches. Never
silently choose a source by ordering. Unknown qualified sources fail or
enter the explicit first-source flow; never reinterpret them as Nix code.

## Security requirements

Separate fetching from Ruby evaluation. Fetch public HTTPS source trees
without hooks, external filters, submodules, or inherited credentials.
Pin whole source content, runtime, policy, and target. Verify redirect and
destination policy. No local/private-network fetches from untrusted URLs.
Use a real URL parser and prevent DNS/redirect checks from being bypassed.

Keep Ruby in a non-fixed-output derivation with sandbox enabled. Refuse
sandbox fallback, relaxed mode, and added host paths. Verify the effective
daemon isolation with behavior probes on each supported platform. Do not
pass user home, secrets, daemon sockets, or model credentials into builds.
Apply bounded execution and output limits. A failure must not create a
successful empty catalog. Captured JSON remains untrusted input.

Only our fixed Nix builder consumes validated data. Never evaluate Nix
files or configuration supplied by a tap. Require payload checksums and
retain archive traversal/symlink protection and script rejection. No sudo,
silent quarantine removal, or replacement of vendor signatures. Verify
macOS assessment at the actual app exposure/launch path; metadata approval
does not certify the app or provide application runtime sandboxing.

## Verification and delivery

Use E2B GLM 5.3 for implementation. Use the prepared Rust/Nix snapshot.
Do not run arbitrary taps in a worker command carrying model credentials.
Use separate clean verification sandboxes without Pi/model keys for raw
Ruby execution. Parent verifies code and integration independently.

Exercise real pinned public taps, helper files, GitHub release binaries,
platform branches, token collisions, cache invalidation, consent refusal,
atomic refresh failure, malicious Ruby, URL redirects, filesystem/network
denial, malformed archives, unsupported scripts, and native lifecycle.
Use metadata-only catalog comparisons and synthetic payloads for broad
coverage. Limit real vendor downloads to a small native sample. Run Rust,
Ruby, Python, Nix and workflow lint for changed files, strict OpenSpec,
normal CI and independent review. Record actual evidence and limits.
Open a reviewable PR. Do not merge or release without a later instruction.
