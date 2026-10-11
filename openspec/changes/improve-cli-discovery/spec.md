# Requirements

## Installed state

The client MUST report unknown installed state when the native profile
cannot be read. It MUST NOT report that a package is absent in that case.

## Package selection

List SHALL expose the package ID and native entry ID. Remove and upgrade
SHALL accept a unique installed package name or package ID. Ambiguous names
MUST fail before mutation. Explicit entry selection SHALL remain available.

## Discovery

Search SHALL use literal text by default and explicit regular expressions.
Search SHALL rank name matches before description matches. Normal search
SHALL match words and name prefixes, rather than incidental short substrings.
All query words MUST match. Normal search SHALL hide known development
namespaces and explicit variants unless their attribute, namespace, or
variant is requested. Packages with library descriptions SHALL be hidden
unless their exact attribute or library category is requested.
`--broad` SHALL expose those packages and unrestricted substring matches.
Conservative name-only typo matches SHALL be shown only when no literal
results exist. `--no-fuzzy` SHALL disable them. Install MUST NOT select a typo
match automatically. Search SHALL combine Nixpkgs, official Homebrew Casks,
and registered cask taps by default, and accept source-qualified queries.
Bare-name alias resolution MUST NOT select a hidden development package or
variant without an exact package ID. All exact candidates SHALL still count
for ambiguity, including hidden aliases and unavailable casks.
Human search SHALL have a bounded default result count. Search MUST NOT contact sources
or run Nix when reading prepared catalogs. Missing catalogs SHALL provide
an actionable setup instruction. Old catalogs SHALL carry an age warning.

## Refresh

Installation SHALL prepare discovery data when a runtime is present unless
preparation is explicitly disabled. A user job SHALL refresh official data
daily unless background setup is disabled. A failed refresh MUST preserve
the previous complete catalog. Refresh MUST NOT mutate the package profile.
