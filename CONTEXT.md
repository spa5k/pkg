# Package Lifecycle

This context defines the language for turning user package intent into trusted, recoverable package state. It separates the vendor-owned Base Nix Lifecycle from the product-owned Package Lifecycle.

## Intent and Identity

**Package Selector**:
The durable statement of what a user wants, including version, output, source, and pin preferences.
_Avoid_: Package request, installable, package identity

**Realization**:
The exact package result selected for one Package Selector. Its identity is its Store Path, not its display name or version.
_Avoid_: Package, artifact, `name@version`

**Store Path**:
The unique identity of a Realization in the Nix store.
_Avoid_: Package path, install path

**Catalog**:
The exact, pinned package source that selectors resolve against.
_Avoid_: Package set, package database

**Index**:
The disposable, derived package metadata used only for search and discovery. It is not authoritative.
_Avoid_: Search database, metadata cache

**Build Plan**:
The canonical, private statement of the derivations, outputs, policy, and host facts for one possible local build.
_Avoid_: Build request

**Build Preview**:
The public, sanitized statement of one possible local build, shown to the user before approval.
_Avoid_: Build summary, build report

**Build Approval**:
Permission for one exact Build Plan under one policy version. It does not authorize later or different builds.
_Avoid_: Confirmation, global approval

## State and Recovery

**Manifest**:
The desired state. It holds one user's set of Package Selectors and their constraints.
_Avoid_: Package list, requirements

**Lock**:
The realized state. It binds each Package Selector to its exact Realization.
_Avoid_: Lock file, resolution

**Lifecycle State**:
The coherent desired and realized package state for one user in the Package Lifecycle.
_Avoid_: Current state

**Generation**:
An immutable snapshot of Lifecycle State that can be activated or retained. Rollback produces a new Generation; it does not reuse a retained one.
_Avoid_: Version, release, snapshot

**Activation Forest**:
The package view exposed by one Generation.
_Avoid_: Profile, environment, generation directory

**GC Root**:
A reference that pins one realized output so garbage collection keeps it.
_Avoid_: GC pin, keep-alive link

**Lifecycle Operation**:
One recoverable attempt to change Lifecycle State in the Package Lifecycle.
_Avoid_: Command, transaction, job

**Base Nix Handoff**:
The durable product evidence that a Base Nix installation is accepted or is not safe to trust.
_Avoid_: Handoff, install journal, Vendor Receipt

**Unknown Base Nix Outcome**:
A Base Nix result for which the product cannot prove success or failure.
_Avoid_: Failure, interrupted install, recoverable install

**Package Repair**:
The user-initiated, verified restore of damaged store content. It is not atomic.
_Avoid_: Repair, Base Nix repair, self-heal, auto-repair

**Product Asset**:
A release-authenticated file that belongs to the installed product rather than to Base Nix or a user package.
_Avoid_: Package asset, Nix asset

**Product Asset Repair**:
The user-initiated replacement of damaged Product Assets with authenticated bytes from the same product release.
_Avoid_: Package Repair, Base Nix repair, product reinstall

## Lifecycle Ownership

**Base Nix**:
The machine-wide Nix installation that provides the shared store and package realization runtime.
_Avoid_: Managed Nix, private Nix, system Nix

**Base Nix Lifecycle**:
The install, repair, update, and explicit uninstall of Base Nix.
_Avoid_: Package Lifecycle, package maintenance

**Package Lifecycle**:
The product-owned work that selects, realizes, activates, repairs, and removes packages for a user.
_Avoid_: Base Nix Lifecycle, Nix lifecycle

## Trust and Authority

**Channel**:
An authenticated, monotonic release of product policy, Catalog identity, product assets, and vendor-owned Base Nix assets.
_Avoid_: Repository, feed, branch

**Broker**:
The authenticated authority that mediates Package Lifecycle work and machine-wide package admission.
_Avoid_: Daemon, server, engine

**Root Helper**:
The authenticated authority for a closed set of privileged Package Lifecycle host changes.
_Avoid_: Installer, root process, sudo wrapper
