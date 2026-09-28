# Raw Ruby casks and public taps

Status: proposed, 29 September 2026. PR #81 is merged at
`f6f99a748653861302876dfe127dfc6bf498d323`. This plan starts from that code.

Read the [offline HTML plan](../../artifacts/public-cask-taps-plan.html).
The full [OpenSpec design](../../openspec/changes/import-public-cask-taps/design.md)
is authoritative. The [task list](../../openspec/changes/import-public-cask-taps/tasks.md)
contains the implementation work. All tasks remain open.

## Recommendation

Use Homebrew's pinned Ruby reader only in isolated catalog-update runners.
Rust validates captured metadata and generates one catalog. Nix builds and
installs it. The client does not install Brew or run Ruby.

This avoids implementing Ruby and Homebrew semantics in Rust. It also avoids
a hosted API or package allowlist. A tap is added through source configuration.
The source's compatible apps are discovered automatically.

## Delivery

1. **PR A:** prove raw export and isolation; add source pins and captured
   metadata. Prove that payload-dependent metadata and required install steps
   are visible and excluded. Keep the current default catalog while this runs.
2. **PR B:** add catalog schema 3 and qualified identities across the Rust
   generator, Nix, and client. Same-token taps coexist. Remove the old schema.
3. **PR C:** run the official tap and three public taps; compare coverage;
   repeat cheap full-catalog checks; run a bounded native sample; switch the
   default source only after the acceptance checks pass.

## User-visible behavior

The proposed full ID is `cask:vendor/tools/editor`. `cask:editor` works only
when one source has that token. Ambiguous names show full choices. Existing
native upgrade, rollback, and removal remain in use.

An independent publisher can generate a flake from public taps. Users select
that flake with the existing `sources.casks` setting. A raw Ruby tap does not
become an instant runtime install source.

## Important limits

Raw Ruby can run code while it loads. Homebrew simulation does not emulate
all host behavior. Native target runners, fixed toolchain pins, captured
output hashes, and source isolation are required.

Some casks need a downloaded payload or active install steps to define their
result. These remain excluded. There is no promise to support every public
tap, formula bottle, privileged installer, or GUI. The
[research note](../research/2026-09-29-raw-ruby-public-taps.md) records concrete
examples and upstream source links.
