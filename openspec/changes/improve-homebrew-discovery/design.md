# Design

## Native Formula support

`pkg` never installs, embeds, or invokes Brew. Formulae and Casks are
ordinary Nix packages in the dedicated pkg profile. A Formula request uses
`brew:homebrew/core/NAME`. Aliases resolve to one canonical package ID.
The configurable Formula source defaults to
`github:spa5k/pkg/main?dir=nix/formulae`.

A maintainer tool reads a bounded, hash-pinned official Core API snapshot.
It does not evaluate Formula Ruby. Disabled and deprecated recipes are
excluded first. On each supported system, it checks Nixpkgs candidates
from the Formula name and aliases. A matching upstream homepage and
numeric version major are required. The catalog records the Nixpkgs
attribute and actual version. The generated flake verifies that version
against its pinned Nixpkgs input.
An explicit `@VERSION` name also requires that version family.

When no equivalent exists, the importer accepts a restricted bottle.
It requires no dependencies, macOS-provided dependencies, requirements,
post-install hook, service, conditional pour rule, or Homebrew relocation.
Only `:any_skip_relocation` bottles with fixed SHA-256 digests from the
official Core registry are accepted. The oldest supported macOS bottle
sets the minimum macOS version. Other recipes have a recorded refusal
reason. No dependency graph or Homebrew runtime is imported.

The bottle builder uses the existing bounded public fetch policy. Its
fixed anonymous registry header is limited to the Core registry request.
Archive extraction permits only bounded files, directories, and internal
links in one Cellar root. Ruby recipes and Brew receipts are discarded.
The builder rejects remaining Homebrew paths, external links, and outputs
with no executable. Linux stages ELF files for Nix's ordinary ELF patch hook.
Every output gets a final path and link audit after fixup. Remaining
Homebrew paths still refuse the build, including in ELF files.
Metadata eligibility does not promise a successful build.

Install validates every requested Formula and Cask before a single native
profile operation. Info and list read native installed state. Remove,
upgrade, history, rollback, and prune use that same profile. Upgrade checks
the current index at each entry's original reference before mutation.
There is no second installed-package ledger.

## Local discovery

Setup, update, and the daily user job evaluate `formulaIndex` at a locked
Git revision. Publication is atomic. A failed refresh retains the last
complete snapshot. Normal search runs no network, Nix, or Brew process.
The generated index has a separate schema from the Cask index and uses
the same checked provenance, identity, availability, and host-range model.

Aliases return canonical IDs. Normal search hides unsupported packages
and declared libraries. Exact names and `--unavailable` expose refusal
reasons. Broad, library, and resource queries can reach non-executable
packages. Eligible rows suggest `pkg install`; excluded rows do not.

## Sections and limits

Human results have Nixpkgs, Homebrew Formulae, Homebrew Casks, and separate
saved tap sections. Each section shows its match count. Rows retain their
relevance order within the section. A global limit is shared across nonempty
sections in rounds. Spare places return to sections that have more rows.
Small limits can show a section with zero displayed rows and its full count.

The temporary spool retains at most the requested limit per source section.
Memory stays bounded. JSON keeps its flat global relevance order and global
limit. A literal match in any source removes all earlier typo alternatives.

`--source brew` and `--source homebrew/core` select formulae. `--source homebrew`
selects formulae and all cask catalogs. The existing cask filters stay available.
Qualified `brew:` queries select formulae. Qualified `homebrew:` queries
select the combined Homebrew sources. Scoped Formula commands ignore
unrelated broken tap state.
