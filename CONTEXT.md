# pkg Language

This context defines the words used for the native Nix design. The active
design authority is the OpenSpec change
[simplify-to-native-nix](openspec/changes/simplify-to-native-nix/proposal.md).

## Products and Sources

**Package**:
One installable thing a user asks for by name, with its source.
_Avoid_: Package Selector, installable, flake ref

**Source**:
A pinned package source that names resolve against, for example a pinned
Nixpkgs revision.
_Avoid_: Catalog (for a single source), channel

**Discovery Data**:
The disposable, derived data used only for search and info. It is not
authoritative for installed packages.
_Avoid_: Search database, metadata cache

**Catalog Index**:
The generated status envelope the cask source exposes as `catalogIndex`:
provenance, target systems, and per-system eligible/excluded entries keyed
by token. It is generated data, not a client-side classification and not
the discovery cache.
_Avoid_: Support manifest, classifier, hosted API

**Fixed Reference**:
A package reference that does not advance during upgrade. It replaces the
removed pin state.
_Avoid_: Pin, pinned version

## State

**pkg Profile**:
The one native Nix profile that holds a user's packages. Nix owns its
entries, generations, and roots.
_Avoid_: Manifest, lock, package state engine

**Generation**:
An immutable native profile snapshot. Rollback selects an earlier
generation; it does not restore product state.
_Avoid_: Version, release, product snapshot

**Rollback**:
The native return to the previous or a selected generation. Launcher sync
follows it.
_Avoid_: Migration, state restore

**Launcher**:
A pkg-owned macOS app trampoline derived from an active native package.
_Avoid_: App install, app copy

**Restored App Copy**:
A disposable copy that retains a vendor app's external signatures.
Its presence does not establish that the package is installed.
_Avoid_: Package inventory, repaired signature

## Runtime

**Nix Runtime**:
The external Nix installation the user owns and installs from a vendor.
pkg never installs, updates, repairs, or removes it.
_Avoid_: Managed Nix, Base Nix, bundled Nix

**Client**:
The standalone `pkg` executable delivered as one archive per supported
system, with completions, license, and notices.
_Avoid_: Installer, agent, service

## Deleted Concepts

The channel, broker, root helper, build approval, product assets, and
product repair are deleted by design. Do not reintroduce these words or
the machinery behind them.
