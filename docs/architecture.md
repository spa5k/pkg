# Module map

The client uses one external Nix runtime and one native profile.
Nix owns installed state and generations.
Catalog data is disposable.

```mermaid
flowchart TD
    CLI[cli: parse arguments] --> Commands[commands: dispatch and session]
    Commands --> Search[search: source selection and results]
    Commands --> Info[info and list: package queries]
    Commands --> Lifecycle[lifecycle: parse and resolve change requests]
    Lifecycle --> Change[change: lock, preflight, mutate, postflight]
    Search --> Catalog[catalog: IDs, routing, snapshots, matching]
    Info --> Catalog
    Lifecycle --> Catalog
    Catalog --> Nix[nix: native command interface]
    Lifecycle --> Nix
    Nix --> Process[process: supervise children and cancellation]
    Process --> Runtime[External Nix]
    Change --> Apps[apps: assess and sync launchers]
    Apps --> Process
    Apps --> Profile[Native profile outputs]
    Runtime --> Profile
    Tap[tap: approved sources and saved catalogs] --> Catalog
    Generator[cask-catalog: capture, classify, plan, emit] --> Tap
```

## Find the owner

Paths below start at the repository root.

| Responsibility | Owner |
| --- | --- |
| Argument grammar | `crates/pkg-cli/src/cli.rs` |
| Command dispatch and session | `crates/pkg-cli/src/commands/mod.rs` |
| Typed command errors and signal exit codes | `crates/pkg-cli/src/commands/error.rs` |
| Search source groups and prefixes | `crates/pkg-cli/src/commands/search/source.rs` |
| Search collection | `crates/pkg-cli/src/commands/search/mod.rs` |
| Bounded result storage and output | `crates/pkg-cli/src/commands/search/results.rs` |
| Package information and installed entries | `crates/pkg-cli/src/commands/info.rs`, `list.rs` |
| Package request resolution and history | `crates/pkg-cli/src/commands/lifecycle.rs` |
| Locked native package changes and app refresh ordering | `crates/pkg-cli/src/commands/change.rs` |
| Name resolution and disposable snapshots | `crates/pkg-cli/src/catalog/` |
| External Nix discovery | `crates/pkg-cli/src/nix/runtime.rs` |
| Native command interface | `crates/pkg-cli/src/nix/mod.rs` |
| Child supervision and cancellation | `crates/pkg-cli/src/nix/process.rs` |
| Native profile and source decoding | `crates/pkg-cli/src/nix/manifest.rs` |
| Generated index types and validation | `crates/pkg-cli/src/nix/index/` |
| Tap approval and atomic publication | `crates/pkg-cli/src/tap/` |
| Tap commands and import validation | `crates/pkg-cli/src/commands/tap/` |
| App sync coordination | `crates/pkg-cli/src/apps/mod.rs` |
| Bundle discovery and private payload preparation | `crates/pkg-cli/src/apps/scan.rs`, `payload.rs` |
| Public tap app assessment | `crates/pkg-cli/src/apps/assessment.rs` |
| Launcher destination ownership | `crates/pkg-cli/src/apps/destination.rs` |
| App helper execution and cache storage | `crates/pkg-cli/src/apps/helper.rs`, `storage.rs` |
| Shared source and token identity rules | `tools/cask-catalog/src/token.rs` |
| Shared numeric version rules | `tools/cask-catalog/src/version.rs` |
| Cask classification and build plans | `tools/cask-catalog/src/classify/`, `plan/` |

## Follow a change

Search selects sources once, then reads local snapshots.
Its SQLite result store bounds memory use for large catalogs.
An exact package query resolves an ID through the catalog.
A package mutation takes the profile lock and calls Nix.
The command reads native state after a failure or cancellation.
On macOS, app sync assesses bundles and checks destination ownership.
It accepts only the current `pkg-cask-apps/2` private payload layout.

The client and generator use the same identity and numeric version rules.
The generator owns classification and build plans.
The client validates generated indexes and reports their recorded status.
Saved catalogs and query views share immutable indexes with `Rc`.
Tap cache writes require a mutable borrow.
Metadata rows use a tagged enum with a required payload for each kind.
Native, configuration, and cache errors keep their causes until command reporting.

Historical experiment findings remain under `spikes/`.
Their source links point to the last revision before removal.
Current work follows [Simplify the native client](../openspec/changes/simplify-client/design.md).
Rust ownership and error rules follow
[Rust design](../openspec/changes/improve-rust-quality/design.md).
