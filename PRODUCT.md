# Product

<!-- impeccable:product-schema 1 -->

## Platform

Linux x86_64 and macOS on Apple silicon (terminal CLI). macOS Intel and
Linux arm64 are not supported.

## Stack

**Current source:** Rust. `pkg` is one client crate over one native Nix
profile per user. The Nix runtime is an external prerequisite: the user
installs it from a vendor, and the tested baseline is Determinate 3.22.1
with Nix 2.35.2. The proven macOS host is 15.7.7 on arm64. Breaking
changes can occur before v1.

The active design is the OpenSpec change
[Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md).
macOS app exposure uses the external `mac-app-util` helper through Nix in a
separate helper profile. It is pinned at revision
`039f33deef21782d4db97087f426504951239887`, is AGPL-3.0-only, and is not
bundled with the client archive.

## Users

Developers and terminal users who want familiar brew/paru-style package
management without learning or configuring Nix.

## Product Purpose

Give terminal users safe, familiar package workflows while Nix stays
outside the normal product interface. Success means a user can search,
install, upgrade, and recover without using raw Nix.

## Positioning

Familiar package commands over one native Nix profile. Nix owns package
state, generations, rollback, and roots. `pkg` owns names, commands,
output, and desktop app launchers. `pkg` runs no daemon, no root helper,
and no package channel.

## Operating Context

Daily terminal tasks: search, info, install, list, remove, update,
upgrade, history, rollback, prune, doctor, shellenv, and macOS app sync.

## Current Alpha Capabilities and Constraints

- The native Nix redesign is in progress. Do not claim finished native
  client journeys on clean hosts yet.
- `pkg` requires an external Nix installation. It never installs, updates,
  repairs, or removes Nix, and it never uses sudo.
- Packages resolve against exactly pinned sources. No floating channels
  or overlays.
- macOS app exposure supports a small set of packages with simple app
  archives or standalone executables. Current Cask conversion candidates
  are iterm2, chromedriver, and cursor. Raycast 2.0.5 requires macOS 26
  and is excluded. Packages that need a native installer or helper are
  out of scope.
- Target platforms: `x86_64-linux` and `aarch64-darwin` only.

## Brand Commitments

- Name: `pkg` is a working codename, not final.
- Voice: concise, calm, factual.

## Evidence on Hand

- The [plan index](plans/README.md) keeps historical records only. The
  OpenSpec change is the design authority.
- The Rust product is an internal alpha. It has not launched. See
  `README.md` for current platform status.

## Product Principles

1. Familiar surface.
2. Hidden complexity.
3. Atomic, recoverable state.
4. Honest trust boundaries.
5. Broad Nixpkgs discovery without a curated catalog.
