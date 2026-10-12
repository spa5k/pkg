# Design

## Current product

The native Nix profile remains the source of installed package state. Discovery
data remains disposable. The client does not manage the Nix installation.
Public tap assessment and launcher ownership checks still run before writes.

## Module ownership

The app module coordinates launcher sync. Its internal modules own private
storage and payloads, bundle discovery, vendor assessment, helper execution,
and destination ownership. They share the current generated payload layout.

The Nix adapter decodes native profile and source metadata separately from
generated catalog indexes. Catalog identity and numeric version checks use
the existing owners. The index decoder keeps strict identity and status checks.

Query command handlers select sources once. Search result storage and info
rendering have separate modules. Source selection defines which official and
saved sources a query reads. Handlers do not repeat source alias conditions.
Tap commands use a separate staging module for sandbox setup and validation.
Updates borrow source records from the registry held under the global lock.

## Removed code

The old managed runtime experiments are outside the workspace and CI. Their
executable code is removed. Findings, evidence, and pinned Git links remain.
The client reads only the current namespaced private app payload layout.
The client no longer cleans up cache files from the removed per-query cache.

## Measurement

Count physical lines in tracked Rust, Python, shell, Nix, and Ruby source files.
Exclude generated JSON, lock files, documentation, build output, and binaries.
Report experiment deletion separately from changes in the current product.
File moves have zero net effect on these counts.
