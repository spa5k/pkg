# Rust Cask catalog design

## Context

See [proposal.md](proposal.md) for scope and the
[research note](../../../docs/research/2026-09-28-dynamic-casks-and-linux.md)
for verified metadata mechanics. The architecture is chosen, not open:
a Rust catalog generator runs at catalog-update time; Nix fetches,
builds, and installs from generated data; the client converts nothing
per install.

Current code this design replaces (traced at the alpha.2 baseline):
`nix/casks/flake.nix` (pinned brew-nix input, Nix classifier, curated
token lists, `cursor` override, `caskSupport` output), client
`CASK_SYSTEM`/`classify_cask` in `catalog/cask.rs`, `gate_cask` in
`commands/lifecycle.rs`, cask search riding `nix search` in
`catalog/search.rs`, and `pkg-cask-support/1` decoding in
`nix/manifest.rs`. Native profile lifecycle, apps launcher sync,
cache, and config are reused unchanged.

Non-goals: formula bottles, fonts (host activation; separate future
change), services/drivers/privileged installers, a hosted API or
database, running Brew or Ruby, macOS Intel or Linux arm64 targets,
per-install client-side conversion, per-package Nix source files, a
committed per-formula package map.

## Goals

- Broad automatic conversion of Homebrew Cask metadata into native Nix
  packages with no token allowlists, no hardcoded app names, no manual
  package maps.
- One maintainer tool with a small, testable core; deterministic
  output; reviewed committed data.
- Client stays thin: status comes from generated data; Nix does all
  fetching, building, installing, upgrading.
- Two verified target systems: `aarch64-darwin` and `x86_64-linux`.

## Decisions

### D1. One maintainer tool as a workspace member: `tools/cask-catalog`

Add `tools/cask-catalog` as a second workspace member beside
`crates/pkg-cli`. The generator and the catalog schema evolve together
with the Nix builders and the client format gate, so one review unit
and one lockfile is the lean shape, and the workspace already enforces
the pinned toolchain, lint policy, and `deny.toml` on every member.
Rust is a maintainability and testability choice for this logic — the
same checks are expressible in Nix; owning them in Rust keeps the
generator unit-testable offline and keeps Nix evaluation thin. The
tool is maintainer-only and never ships to users (see D13).

**Client packaging isolation is a required fix, not an inherited
property.** Today `tools/release/package_client.sh` collects license
texts from `cargo metadata --locked --filter-platform <triple>` — that
enumerates the *whole workspace* resolve graph, so with a second
member the client archive would wrongly carry the generator's
dependency licenses (and imply shipping the tool). The script owner
(RC-01, release-script ownership) must change the collector to
traverse only the resolve graph reachable from `pkg-cli` for the
packaged target — for example by seeding from the `pkg-cli` package id
in the filtered metadata and walking `deps` edges, or by deriving the
crate set from `cargo tree -p pkg-cli --target <triple>` — excluding
every workspace member and dependency not in that graph. Acceptance:
the archive's `licenses/` index contains exactly the client's runtime
dependency set, and a dedicated check verifies no `cask-catalog`
binary or license text appears in the archive.

### D2. Offline generation: pinned input in, deterministic catalog out

Two strictly separated modes:

- `cask-catalog fetch` (network): validates the 40-hex revision, the
  `owner/name` repository, and the expected SHA-256 (default: the known
  pinned hash) before touching paths or the network; downloads with
  curl (`--fail --location --silent --show-error --connect-timeout 20
  --max-time 120`) to a temporary file; verifies the bytes against the
  expected hash BEFORE any pin or cache file is replaced; and records
  the pin in `nix/casks/catalog/input.json`: source URL, exact
  revision, content SHA-256, upstream data license attribution. The
  snapshot cache lives under `/tmp/cask-catalog`, never in the repo.
- `cask-catalog generate` (offline, deterministic): reads the pinned
  input plus the pin file and writes `nix/casks/catalog/catalog.json`.
  Same pin plus same generator version always produce byte-identical
  output: sorted keys, compact serialization, no timestamps, no
  iteration-order-derived data. CI regenerates from the committed pin
  and fails on any diff.

Generation is atomic: output is written to a temporary file and moved
into place only after every integrity check (D7) passes; any failure
leaves existing outputs untouched. Generated data and the pin are
committed together in one reviewed change. The metadata is pinned
exactly once (in `input.json`); Nix never re-pins raw metadata because
it reads only the generated catalog. No server, no scheduler.

Provenance is embedded in `catalog.json`: schema id, generator
name and version, input URL, revision, SHA-256, target systems, and
the declared macOS baseline. Snapshot field coverage is NOT recorded
in provenance; the coverage summary prints to the terminal for the
maintainer and is recorded in the dated verification note only.

### D3. Output shape: one compact normalized catalog, not per-package files

`catalog.json` (schema `pkg-cask-catalog/2`) is the ONE committed
document: a compact JSON file with sorted keys, published atomically
through a unique same-directory temporary file that is persisted over
the target only after generation succeeded. The Nix source derives its
`catalogIndex` and `catalogStatus` outputs from this file; the
generator does not commit projected copies. Coverage counts print to
the terminal for the maintainer. Per-token entry:

```json
{
  "token": "cursor",
  "name": "Cursor",
  "description": "AI-first IDE",
  "version": "3.17.19",
  "homepage": "https://cursor.com",
  "targets": {
    "aarch64-darwin": {
      "status": "eligible", "kind": "app+cli", "reason": null,
      "detail": null, "version": "3.17.19",
      "homepage": "https://cursor.com",
      "plan": {
        "source": { "url": "https://down.../Cursor.dmg", "sha256": "..." },
        "archive": { "kind": "auto" },
        "artifacts": [
          { "kind": "app", "source": "Cursor.app", "target": "Cursor.app" },
          { "kind": "binary",
            "source": "$APPDIR/Cursor.app/Contents/Resources/app/bin/cursor",
            "target": "cursor" }
        ],
        "minMacos": null
      }
    },
    "x86_64-linux": {
      "status": "eligible", "kind": "appimage", "reason": null,
      "detail": null, "version": "3.17.19",
      "homepage": "https://cursor.com",
      "plan": {
        "source": { "url": "https://down.../Cursor-x64.AppImage", "sha256": "..." },
        "archive": { "kind": "appimage" },
        "artifacts": [
          { "kind": "appimage", "source": "cursor.AppImage", "target": "cursor" }
        ],
        "minMacos": null
      }
    }
  }
}
```

An excluded target carries `"status": "excluded"`, `"kind": null`,
`"reason": "<code>"`, `"detail": "<string|null>"`, effective
`version`/`homepage` (string or null), and `"plan": null`. Archive
kinds are `auto` (content sniff), `raw-binary` (explicit naked
container only), and `appimage`; nothing is inferred from file
extensions, and unsupported container shapes are excluded as
`unsupported-container`. A token absent from `entries` is unknown
(D10). Anchor rules: every artifact is one uniform
`{kind, source, target}` object; `target` is null only for `pkg`. An
`app.source` names a bundle within the extraction root and its
`target` is the renamed bundle ending `.app`. A `binary.source` may
start with `$APPDIR` (the only recognized install anchor) and its
`target` is the link name from the metadata rename or the source
basename. An `appimage.source` names the image file and its `target`
is the derived basename without `.AppImage`. Nix reads the file with `builtins.fromJSON` (plain data; no
IFD, no compiled code at evaluation) and builders consume the parsed
attrsets. The tool never emits Nix syntax and never emits one file per
package.

Status is exactly `eligible` or `excluded` per target. `eligible` is a
metadata claim: a generic builder can express the record. Build
validation happens lazily at request time and may still fail (D8).
`verified` is not a status; verification results are observational
records in docs/verification, never an install gate or allowlist.

### D4. The Nix source: standard flake, generic builders, laziness

`nix/casks/flake.nix` keeps one input: `nixpkgs` (pinned). Outputs:

- `packages.<system>.<token>` — one derivation per eligible token per
  target system, from mapping generic builders over entries with
  `lib.attrsets.mapAttrs`. Lazy: one token evaluates one plan.
- `catalogIndex` — the whole client index envelope (exact shape in
  D10), DERIVED BY THE FLAKE from the committed `catalog.json`: same
  provenance, `systems.<system>.entries.<token>` rows without plans.
  One cheap eval output, no per-system attribute, so a missing target
  is detectable by reading `targets`, never by a failed attribute
  lookup that could be mistaken for a fetch failure.
- `catalogStatus` — provenance plus per-system total/eligible/excluded/
  by-reason counts, also derived by the flake for reporting.

Builders live in `nix/casks/lib/builders.nix`, generic over plan data:
`buildAppArchive`, `buildBinary`, `buildPkgPayload`, `buildAppImage`,
plus shared extraction and safety phases. `buildAppImage` uses the
pinned Nixpkgs `appimageTools.wrapType2` (its API verified in the
locked nixpkgs `567a49d1`); the helper owns any internal extraction
and patching, and builders never hand-roll `APPIMAGE_EXTRACT_AND_RUN`
tricks or blanket `autoPatchelf` over the image. They own the logic
ported from brew-nix's `casks.nix` (archive dispatch, bundle
placement, binary linking), with brew-nix's MIT license preserved in
`nix/casks/lib/README.md` naming the ported revision. After the port,
the `brew-nix` flake input is deleted; the attribution stays. The
flake must keep evaluating with import-from-derivation disabled.

### D5. Platform scope: strict required `supported_platforms`, fail closed

Target set is exactly `aarch64-darwin` and `x86_64-linux`. The
accepted input schema *requires* the `supported_platforms` field — the
pinned snapshot `245947c0` carries it on every entry — plus `token`,
`version`, `artifacts`, `url`/`sha256` (or per-target equivalents
after variation merge). `aarch64-darwin` maps to the exact platform
tag `arm64_sequoia` (the baseline 15.7.7 is Sequoia); any other arm64
tag is not sufficient. `x86_64-linux` needs the exact `x86_64_linux`
tag. Rules:

- Snapshot-level: a snapshot where NO record carries
  `supported_platforms` (the field family is gone) fails generation
  atomically before any output is replaced (D7). The mirror HEAD
  dropping `supported_platforms` is exactly this case; a bump must not
  silently widen or narrow scope.
- Record-level: one record missing the field is isolated and excluded
  as `malformed-record`. No default OS scope is guessed.
- OS/arch intent comes only from explicit complete constraints:
  exact `supported_platforms` membership (D5 tags) and `depends_on`
  OS/arch entries. Variation presence (`*_linux` keys) and artifact
  kinds are never evidence of OS support — the research note proves
  both directions of that inference wrong.
- A fallback for snapshots without `supported_platforms` would require
  explicit complete OS constraints per record; none is implemented in
  this change.

Per target, the generator computes the effective record — base merged
with the target's own variation when one exists (`arm64_sequoia` for
aarch64-darwin because the base record may be newer; `x86_64_linux`
for Linux) by a generic object extend: every key the variation carries
replaces the base value wholesale, explicit null included, artifacts
array replaced never concatenated; `token` and `variations` are never
taken from a variation; a variation that is not an object is an
isolated `malformed-record`. A missing target variation is not
evidence of anything; the base payload may serve that system. Eligibility then applies, in order:
disabled/deprecated rejected; OS/arch intent; effective artifact kinds
within the allowed set for that OS (macOS: `app`, `binary`, `pkg`;
Linux: `binary`, `app_image`; inert `zap`/`uninstall`/completion
stanzas never execute); effective URL and checksum present and not
`no_check`; executable installer stanzas (`installer`, preflight,
postflight, service stanzas) absent; dependencies resolvable (D9);
declared `minMacos` compatible with the baseline (D11). Every artifact
in the effective record must classify; one bad stanza excludes the
token on that target with a reason naming it.

Bounded reason vocabulary (exactly the codes the generator emits,
verified against the committed catalog): `installer-script`,
`unsupported-artifact`, `unsupported-container`,
`no-installable-artifact`, `missing-url`, `missing-checksum`,
`minimum-os`, `unsupported-platform`, `arch-unsupported`, `disabled`,
`deprecated`, `formula-dependency`, `cask-dependency-integration`,
`unsupported-download-spec`, `malformed-record`. There is no
`native-installer` reason code. Unknown artifact kinds and
malformed records exclude that token only, with a recorded reason;
they never abort the catalog (whole-catalog failures are D7).

### D6. Safety: vendor data is data, never syntax; validate before writes

Two invariants, enforced in the generator and the builders:

1. No vendor string becomes Nix or shell syntax. Generator output is
   JSON read as data. Builders construct every shell command from a
   fixed vocabulary; plan fields are interpolated only as
   `lib.strings.escapeShellArg`-quoted arguments, environment values,
   or a plan file consumed at build time. The generator validates
   path-ish fields against the anchor rules in D3: relative to a
   recognized anchor, no absolute paths, no `..` traversal, no
   unresolved `$VAR` expansions beyond `$APPDIR`, no NUL/newline in
   link names. Violations exclude the token as `malformed-record`
   naming the field.
2. Nothing from metadata executes at build time beyond the fixed
   builder phases. `installer`, `script`, preflight/postflight,
   service stanzas, and installer choices are never translated; a
   record needing them is excluded. Only declarative extraction,
   copy, rename, symlink, wrapper, and patch steps exist, entirely
   within the Nix output.

Path and link safety runs **before writes, then again after**:

- Pre-extraction (before any payload is written into the output):
  the extraction step validates archive member paths and symlink
  targets as they are read — members must stay inside the staging
  root (normalized, no absolute or escaping members), and vendor
  symlink targets must resolve within the staging tree. Legitimate
  in-bundle relative links that traverse up with `..` and land back
  inside the tree (framework `Versions/Current` chains) are allowed
  when the normalized resolution stays in-tree.
- Post-install verification: builder-created links may point inside
  `$out` or at declared Nix store inputs (dependency outputs,
  wrapped runtime tools) — ordinary Nix practice — but no link in a
  *vendor payload* may point anywhere else, and no copied path may
  escape `$out`. A violation fails the build.

`zap` and `uninstall` are parsed only as inert metadata; Homebrew
cleanup hooks never run.

### D7. Whole-catalog integrity and atomic publication

Generation fails, with a clear error and **no output replacement**,
when: the input contains duplicate tokens; a token is not a usable key
(`[a-z0-9][a-z0-9+._@-]*` without `..`; versioned tokens such as
`1password-cli@1` and `+`-suffixed tokens are real data and valid);
the input's actual SHA-256 does not match the committed pin; the
snapshot root is not a JSON array, is empty, or lacks the
`supported_platforms` field family entirely (D5); or a record carries
no usable token string. There is no "conflicting versions" check
(wholesale variation replacement makes conflicts impossible) and no
generator schema self-check. Output is staged to a
temporary file and atomically renamed only after all checks pass, so
a failed run can never leave a half-written or mixed catalog.
Package-level unsupported records become excluded entries; they are
never whole-catalog failures. CI's regenerate-and-compare check (D2)
is the published-state twin of this rule.

### D8. `.pkg` under a structural, relocatable-layout rule

`.pkg` artifacts are eligible when metadata passes D5, but the build
must prove the payload. `buildPkgPayload` (ported from brew-nix:
`xar` + `pbzx`/`gzip` + `cpio`) accepts **only relocatable app-bundle layouts**: a payload
`Applications` directory of `.app` bundles, a top-level `.app`
bundle, or a bare `Contents` directory rebuilt as a bundle from its
`Info.plist` `CFBundleName`. Plain files and every other component
shape fail the build naming the component; nothing is silently
placed. The build fails, naming the offending part, when the xar
carries installer scripts (`Scripts` non-empty), a `Distribution`
stanza (choice-requiring installers are unsupported), nested
component pkgs, plugins, or payload components outside the supported
layouts — system `Library`/`System` files, launchd services, drivers,
kexts, privileged helpers. Essential payload is never silently
dropped: if a component cannot be placed under the supported layouts,
the build fails rather than installing a partial result. This is the
precise hard limit — structural, not a per-token list. It never runs
`/usr/sbin/installer` or any vendor privileged installer, service, or
driver setup.

Resolved 2026-09-28 (final generator review): twelve pkg records
whose upstream `Distribution` requires choices are now excluded at
generation as `installer-script`; `meta-quest-remote-desktop` (naked
container plus pkg) is excluded as `unsupported-container`. The same
review tightened strictness: malformed dependency, option, and
variation shapes now reject; duplicate artifact targets reject;
macOS constraint shapes are validated on all targets, while baseline
comparison and `minMacos` apply only to `aarch64-darwin`, never to
Linux. See the verification note.

This is where *metadata eligible* and *build validated* visibly
differ: the catalog marks a pure-`pkg` record eligible from metadata;
the first build attempt proves or refutes the payload, and laziness
keeps that cost per-token. If most pkg payloads fail the rule,
coverage counts (D12) will show it honestly.

### D9. Dependencies: excluded, not resolved

There is no dependency graph and no profile expansion in this change.
A record with any nonempty Homebrew formula dependency is excluded as
`formula-dependency`; no nixpkgs attribute is guessed and no per-formula
map is committed. A record with any nonempty cask dependency is excluded
as `cask-dependency-integration`, because generic builders cannot prove
the vendor integration a dependency implies. The client installs one
package per token through the ordinary native profile path; the native
Nix lifecycle stays single-package. (A future change may design reviewed
dependency support; that is not this change.)

Linux dynamic ELF still uses Nixpkgs `autoPatchelfHook` with a bounded
generic runtime-library closure in `builders.nix`; unresolved sonames
fail the build naming the missing library. AppImages are not blanket
patched; the pinned `appimageTools.wrapType2` helper owns them.

### D10. Client integration: one index envelope, explicit search semantics

The client stops classifying casks. The classifier lives only in the
generator. Protocol — the flake exposes exactly:

```
nix eval --json <casks-source>#catalogIndex
```

returning one envelope (no per-system attribute):

```json
{
  "schema": "pkg-cask-catalog/2",
  "generator": { "name": "cask-catalog", "version": "0.1.0" },
  "input": { "url": "…", "revision": "<40-hex>", "sha256": "<64-hex>",
              "license": "…" },
  "targets": ["aarch64-darwin", "x86_64-linux"],
  "macosBaseline": "15.7.7",
  "systems": {
    "aarch64-darwin": { "entries": {
      "cursor": { "token": "cursor", "name": "Cursor",
                   "description": "AI-first IDE",
                   "version": "1.6.44", "homepage": "https://cursor.com",
                   "status": "eligible", "kind": "app+cli",
                   "reason": null, "detail": null },
      "zoom":    { "token": "zoom", "name": "Zoom", "description": null,
                   "version": "7.1.5", "homepage": null,
                   "status": "excluded", "kind": null,
                   "reason": "installer-script", "detail": null } }
    },
    "x86_64-linux": { "entries": { } }
  }
}
```

Null rules: `name`, `description`, `version`, `homepage` are string or
null (metadata may lack them). `eligible` ⇒ `kind` non-null string,
`reason` null, `detail` null. `excluded` ⇒ `kind` null, `reason`
non-null code, `detail` string or null. There is no `depends` field. A token missing from `entries` is
unknown. The client checks `targets` membership **before** touching
`systems`: a system outside `targets` is platform-skipped from the
envelope alone, with no attribute evaluation that could be mistaken
for a network failure. The API stays on the flake; no hosted service.

Client changes:

- Search semantics are explicit. The client has no regex engine
  today; native Nixpkgs search passes the pattern to `nix search`
  unchanged. Decision: add the `regex` crate to `pkg-cli` for the
  catalog lane, with documented semantics — catalog search accepts a
  Rust-regex pattern, matched locally against index tokens and names;
  the Nixpkgs lane is unchanged and passes patterns through; the docs
  state both grammars. This is a small, declared client dependency,
  not a silent semantic difference, and no literal-matching surprise.
- `catalogIndex` replaces `caskSupport`: `cask.rs` becomes index
  lookup; `CASK_SYSTEM` and `casks_supported_on` are deleted; platform
  capability comes from `targets`. Search reads the envelope once per
  query (existing cache, source identity, and stale/fresh rules apply
  unchanged) and filters locally; no derivation is forced; one bad
  record cannot fail the query set.
- `info` and `install` resolve to the ordinary
  `packages.<system>.<token>` attribute. `gate_cask` keeps its role:
  refuse `excluded` (reason shown) and unknown tokens; nothing
  invalid becomes eligible. Install adds exactly the requested token;
  there is no closure expansion (D9).
- `manifest.rs` decodes the envelope with a strict gate: an unknown
  schema fails naming the schema found and the schema supported. The
  fixture is regenerated from real generator output.
- Native lifecycle untouched: installs keep the original moving
  reference; upgrade re-resolves natively to the then-current revision
  with pinned inner data; source identity at discovery stays
  consistent; no ephemeral install paths, no custom state, no
  Homebrew cleanup hooks.

### D11. macOS minimums: declared baseline, honest comparison

The declared baseline is exactly `15.7.7`; comparisons are numeric on
major, minor, and patch, with missing components treated as zero. Two generic layers:

- Generation: `depends_on macos` constraints are compared against the
  baseline by major and minor version (patch components compared when
  present). A token requiring a newer macOS is excluded with
  `minimum-os` carrying the required version.
- Build: the generic app builder reads the installed bundle's
  `Info.plist` `LSMinimumSystemVersion` read-only (signatures
  preserved) with the same major/minor comparison, and fails clearly
  when the bundle demands more than the baseline.

History corrected: the alpha Raycast exclusion was discovered through
the *hidden* bundle plist found in on-device verification, not through
visible cask metadata — which is exactly why the build-time check
exists and why metadata eligibility is not a launch promise.

### D12. Verification approach and coverage reporting

Focused tests at the seams we own: generator unit tests for variation
merge (base+override, artifacts-array replacement, sha-only
variation), multi-artifact plans with renames, path validation
rejections, dependency exclusion (formula and cask — there is no
closure engine and no cycle handling to test, D9), determinism,
duplicate-token failure, strict platform-field rules; small builder
smoke plans
built for real on both systems.

Real journeys: Linux in the E2B sandbox; macOS in the parent's Tart
VM. Recorded in docs/verification with systems, revisions, outcomes.
In-flight observational probes (second sandbox): the `1password-cli`
binary and the `koreader` AppImage; results land in evidence when
done — they are not claims yet. Linux verification is headless: do not
claim GUI/AppImage graphical launches there; verify runnable CLI
behavior and wrapper integrity only. Full-catalog evaluation reports
total, eligible, and excluded-by-reason counts per system — an
eligibility statement, never a compatibility promise.

### D13. Deletion, ownership, and packaging boundaries

Delete with this change: the brew-nix flake input and curated token
machinery in `nix/casks/flake.nix`; `CASK_SYSTEM`,
`casks_supported_on`, and `pkg-cask-support/1` decoding in the client;
the old fixture. Keep: native lifecycle, cache, source identity,
launcher sync, Nix runtime policy. Release packaging: apply the D1
license-collector fix (ownership: the RC-01 implementer owns
`package_client.sh` for this change) and verify the archive contains
only client outputs — no generator binary, no maintainer-tool license
texts, no catalog data. Docs (`docs/casks.md`, PRODUCT/CONTEXT where
they name the curated set) are rewritten. The client's scope and
packaging modules stay minimal: the only client additions are
the index decode and the declared `regex` dependency. Nothing in this change runs a vendor installer, installs
Nix on a Mac, or executes Homebrew.

## Risks / Trade-offs

- **Snapshot review weight** — normalized catalog keeps only
  builder/index fields; review reads the coverage summary. Splitting
  files is a later option, not pre-built.
- **Strict field requirement** — a mirror bump that drops
  `supported_platforms` fails generation until the input source is
  switched or the schema rule is revisored. That is the intended
  fail-closed behavior, not an outage.
- **Formula exclusions are broad** — every formula-dependent cask is
  excluded until a reviewed resolution design exists. Honest breadth
  trade: no guessed names.
- **Eligible ≠ works** — laziness confines failures; coverage counts
  expose the gap; `verified` stays observational.
- **autopatchelf misses** — unresolved sonames fail naming the
  library; the generic closure grows by review, not guessing.
- **AppImage fidelity** — `appimageTools.wrapType2` handles common
  type-2 images; hostile images fail the wrap clearly. GUI launches
  are only claimed where actually verified.
- **Eval cost of one big JSON import** — measured in the full-catalog
  run; index and plans are separate outputs already.
- **Two-system scope** — other systems need new verification, not a
  list edit.

## Migration Plan

No compatibility layer. The casks flake's outputs change shape in one
breaking step; the client format gate makes old-client-meets-new-
catalog (and the reverse) a clear error naming both schemas.
Installed alpha profiles referencing old attributes are not migrated
— remove and reinstall; the product has not launched. Homebrew is
never installed.
