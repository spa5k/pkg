# Plan: Rust Cask catalog generator and native Nix source

Date: 28 September 2026. Status: **plan only, not implemented.**
Authority: the OpenSpec change
[`generate-cask-catalog-with-rust`](../../openspec/changes/generate-cask-catalog-with-rust/proposal.md)
— its [design](../../openspec/changes/generate-cask-catalog-with-rust/design.md)
resolves the open points and its [tasks](../../openspec/changes/generate-cask-catalog-with-rust/tasks.md)
stage the work. This page is the readable end-to-end view. The
[HTML reading copy](../../artifacts/rust-cask-catalog-plan.html) mirrors
this plan.

Baselines: workspace `f5c2270` (`0.2.0-alpha.2`, branch
`feat/rust-cask-catalog`); upstream brew-nix `16131ae4` (MIT); pinned
metadata snapshot `245947c0` (carries `supported_platforms` on every
record). Ground truth for metadata mechanics is the
[research note](../research/2026-09-28-dynamic-casks-and-linux.md).

## Summary

Today `nix/casks` converts three hand-picked Cask tokens on
`aarch64-darwin` through the pinned brew-nix converter, with a Nix-side
classifier, curated token lists, one hand-written override, and a
client-side platform constant (`CASK_SYSTEM`) plus a manifest gate.

The chosen architecture replaces that with:

1. **A maintainer Rust tool** (`tools/cask-catalog`) that runs at
   catalog-update time. It reads one pinned Homebrew Cask JSON snapshot
   offline and emits one deterministic, compact, normalized catalog
   (`pkg-cask-catalog/2`): per-target eligibility, bounded exclusion
   reasons, typed artifact plans, resolved cask dependencies,
   provenance. Rust is a maintainability/testability choice; the same
   logic is expressible in Nix.
2. **A standard Nix flake** (`nix/casks`) that reads the committed
   generated data and builds packages with a small set of generic,
   data-driven builders (app archive, binary, pkg payload, AppImage via
   Nixpkgs `appimageTools.wrapType2`). No per-package Nix files, no
   brew-nix input after the port, no import-from-derivation, no
   compiler at evaluation, lazy per-token derivations, targets
   `aarch64-darwin` and `x86_64-linux`.
3. **A thinner client**: cask status comes from one cheap generated
   index envelope (`catalogIndex`, with an explicit target list so an
   untargeted system is detected without attribute probing); broad
   search filters locally with a declared Rust-regex grammar; `info`
   and `install` point at the ordinary flake attribute and expand the
   cask dependency closure; `CASK_SYSTEM`, the client classifier, and
   the `pkg-cask-support/1` schema are deleted. The native profile
   lifecycle is untouched.

Goals: broad automatic conversion, no token allowlists, no hardcoded
app names, no manual package maps, no hosted API/database/Brew/Ruby
runtime, low maintenance. Breaking changes are accepted; no
compatibility layers.

## Dataflow

```
 maintainer (reviewed change)                    every user machine
 ================================               =====================
 brew-api snapshot @ exact rev
        │  cask-catalog fetch (only network
        │  step; verifies sha256)
        ▼
 nix/casks/catalog/input.json   (pin: url, rev,
        │                       sha256, license)
        │  cask-catalog generate (offline,
        │  deterministic, atomic)
        ▼
 nix/casks/catalog/catalog.json ── committed together, reviewed ──┐
   schema pkg-cask-catalog/2                                     │
   entries[token].targets[sys] =                                 │
     eligible (typed plan) | excluded (reason)                   │
        │                                                        ▼
        │                              nix/casks flake (nixpkgs only input)
        │                                ├─ packages.<sys>.<token>   (lazy)
        │                                │    └─ generic builders read
        │                                │       plan as data; fetch/build/
        │                                │       install/upgrade by Nix
        │                                ├─ catalogIndex   (one envelope:
        │                                │    targets + per-system status)
        │                                └─ catalogStatus  (coverage)
        │                                                        │
        ▼                                                        ▼
 CI: regenerate from pin, fail on diff                pkg client
 (atomic: no output replaced on any                    ├─ read targets first;
  integrity failure)                                   │  untargeted system →
                                                       │  platform-skipped
                                                       ├─ search: envelope +
                                                       │  local Rust-regex
                                                       │  filter; existing
                                                       │  cache and source
                                                       │  identity; Nixpkgs
                                                       │  lane unchanged
                                                       ├─ info/install:
                                                       │  ordinary flake attr;
                                                       │  gate on generated
                                                       │  status; install
                                                       │  expands cask closure
                                                       └─ native profile
                                                          lifecycle unchanged
```

Key properties: the client never runs the generator; Nix never runs a
compiler at evaluation; vendor strings are always data; one metadata
pin exists; unknown tokens never become eligible; OS support comes
only from explicit constraint fields (`supported_platforms`,
`depends_on`), never from variation keys or artifact kinds; `verified`
is observational evidence, never an install gate.

## Phases

Phases mirror the [task groups](../../openspec/changes/generate-cask-catalog-with-rust/tasks.md).

| Phase | Group | Content | Depends on |
| --- | --- | --- | --- |
| 0 | RC-01 | Workspace member `tools/cask-catalog`; **packaging isolation fix** (license collector walks only pkg-cli's target resolve graph; archive check); attribution; CI wiring | — |
| 1 | RC-02 | Generator core: fetch+pin, strict required-field schema (fail closed), variation merge, eligibility+reasons, typed plans, path safety, dependency closure (formula deps excluded, no map), atomic integrity, determinism | RC-01 |
| 2 | RC-03 | First committed catalog; flake over committed data; macOS builders (app+CLI, pre-write link checks, plist minimum check, pkg relocatable-layout rule, completions); CI regeneration gate | RC-02 |
| 3 | RC-04 | Linux builders (autoPatchelfHook with bounded generic closure; `appimageTools.wrapType2`); concrete runtime dependencies; laziness checks | RC-03 |
| 4 | RC-05 | Client: envelope decode + format gate + targets check, declared `regex` search grammar, `info`/`install` on generated status with closure expansion, delete `CASK_SYSTEM` and old schema | RC-03 (parallel with RC-04) |
| 5 | RC-06 | Delete brew-nix input and curated machinery; delete old manifest path; rewrite docs incl. search grammar | RC-03, RC-05 |
| 6 | RC-07 | Evidence: full-catalog evaluation coverage counts; Linux E2B journeys (headless, no GUI claims); macOS Tart VM journeys (app+CLI launch + codesign); release integrity | RC-04–RC-06 |

## File ownership

New files (created by the implementer; nothing here edits production
code yet):

| Path | Owner | Content |
| --- | --- | --- |
| `tools/cask-catalog/**` | generator | Rust workspace member; `fetch`, `generate`, `self-test`; focused tests |
| `tools/release/package_client.sh` (fix) | RC-01 implementer | license collector walks only pkg-cli's resolve graph for the packaged target; archive content check |
| `nix/casks/catalog/input.json` | generator (committed) | pin: source URL, revision, sha256, license attribution |
| `nix/casks/catalog/catalog.json` | generator (committed) | normalized catalog, schema `pkg-cask-catalog/2` |
| `nix/casks/lib/builders.nix` | Nix source | generic data-driven builders ported from brew-nix (MIT kept) |
| `nix/casks/flake.nix` (rewrite) | Nix source | outputs over committed data; nixpkgs-only inputs |
| `crates/pkg-cli/src/catalog/cask.rs` (rewrite) | client | envelope lookup; no classifier, no `CASK_SYSTEM` |
| `crates/pkg-cli/src/nix/manifest.rs` (edit) | client | `pkg-cask-catalog/2` decode + schema gate |
| `crates/pkg-cli/src/catalog/search.rs` (edit) | client | cask lane rides the envelope with declared regex filtering |
| `crates/pkg-cli/Cargo.toml` (edit) | client | declared `regex` dependency for catalog search |
| `crates/pkg-cli/tests/fixtures/cask-support.json` (replace) | client | regenerated from real generator output |
| `.github/workflows/ci.yml` (edit) | CI | catalog regeneration check |
| `docs/casks.md` (rewrite) | docs | generated-catalog operation, update procedure, search grammar |

Untouched by design: lifecycle commands, profile paths, apps launcher
sync, cache format (schema 2 stays), Nix runtime policy. Client scope
stays minimal: the only client additions are the envelope decode,
closure expansion, and the declared `regex` dependency.

## Acceptance

The change is done when all of the following hold. Each maps to a
capability spec:

- Determinism, pinning, strict platform scope, atomic integrity:
  [cask-catalog-generation](../../openspec/changes/generate-cask-catalog-with-rust/specs/cask-catalog-generation/spec.md)
  — CI regeneration from the committed pin is byte-identical;
  provenance names input revision, hash, generator version; a snapshot
  losing `supported_platforms` fails closed with no output replaced;
  duplicate tokens or pin mismatch abort atomically; OS support never
  inferred from variations or artifact kinds.
- Builders and safety:
  [generated-cask-packages](../../openspec/changes/generate-cask-catalog-with-rust/specs/generated-cask-packages/spec.md)
  — index and per-token builds evaluate with IFD disabled; one token
  builds without forcing others; app+CLI, pkg relocatable-layout,
  pre-write link validation, bounded autoPatchelf closure,
  `appimageTools.wrapType2`, and runtime dependency presence hold; no
  system or vendor installer is ever invoked; no essential pkg payload
  is silently dropped.
- Client:
  [catalog-status-discovery](../../openspec/changes/generate-cask-catalog-with-rust/specs/catalog-status-discovery/spec.md)
  — status and search rows share one locked revision; broad search
  forces no derivations with a declared regex grammar; excluded and
  unknown tokens are refused with reasons; missing targets are
  detected from the envelope; schema gate names both schemas on
  mismatch; installs expand the cask closure; native upgrade still
  follows the moving reference; no Homebrew cleanup hooks run.
- Evidence (RC-07): real journeys recorded in `docs/verification/` —
  Linux binary + AppImage runs (E2B, headless: runnable claims only),
  macOS app+CLI launch with `codesign --verify --deep --strict`
  (parent Tart VM), coverage counts labeled as eligibility (not
  compatibility), client archive free of generator artifacts and
  maintainer-tool licenses (RC-01 fix verified).
- CI green: build, test, fmt, clippy, docs, deny over the two-member
  workspace; docs-linkcheck green with updated active-change links.

Explicitly not acceptance criteria: universal Cask support, formula
bottles or formula resolution, fonts, services/drivers, macOS Intel or
Linux arm64 targets, GUI launch claims from headless runs, or a fixed
eligible-count target.

## Risks and outstanding constraints

- **Snapshot review weight.** The normalized catalog keeps only
  builder/index fields; still, updates touch one large generated file.
  Mitigation: coverage summary in the change description; splitting
  the file is a later option, not pre-built.
- **Strict field requirement.** A mirror bump that drops
  `supported_platforms` fails generation until the input source is
  switched or the rule is revisored. Intended fail-closed behavior.
- **Formula exclusions are broad.** Every formula-dependent cask is
  excluded in this change; a reviewed resolution design may come
  later. No guessed names, no committed map.
- **Eligible ≠ works.** Many eligible records will be untested; builds
  may fail on first request (lazily, per token). Coverage counts make
  the gap visible; `verified` stays observational. Pending probes
  (`1password-cli` binary, `koreader` AppImage) are running in the
  second sandbox; they are not claims yet.
- **autopatchelf and AppImage pass rates.** Out-of-closure sonames and
  wrap-hostile images fail with clear errors; the generic closure grows
  by review only; GUI launches are claimed only where verified.
- **pkg pass rate may be low.** The relocatable-layout rule is precise
  but strict; widening it is a future reviewed change, not per-token
  overrides.
- **Client release licensing.** The RC-01 packaging fix is a
  prerequisite, not an afterthought: without it the workspace-wide
  license collector would ship maintainer-tool licenses with the
  client. Re-verified in RC-07.
- **Verification split.** Linux evidence is headless (E2B); macOS
  evidence depends on the parent's Tart VM. Claims stay within what
  each environment can prove.
- **Two-system scope.** Only `aarch64-darwin` and `x86_64-linux` are
  targets; adding systems requires new verification, not a list edit.

Constraints honored throughout: no token allowlists, no hardcoded app
names, no manual package maps, no hosted API/database, no Brew/Ruby
runtime, no vendor installer execution, no Homebrew cleanup hooks, no
per-package Nix files, no compiled Rust shipped to users,
import-from-derivation stays disabled, original moving references
preserved, declared macOS baseline 15.7 compared by major and minor.
