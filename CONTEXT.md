# pkg Language

This glossary defines packages, sources, profile state, and launchers in pkg.

## Products and Sources

**Package**:
One installable thing a user asks for by name, with its source.
_Avoid_: Package Selector, installable, flake ref

**Source**:
A named origin of package definitions, such as Nixpkgs or a Homebrew Cask tap.
A Source can have more than one revision.
_Avoid_: Catalog (for a single source), channel

**Source Reference**:
A locator that selects package definitions from a Source.
It can follow changes or stay fixed.
_Avoid_: Source identity (for a locator), package name

**Source Revision**:
The recorded identity of one version of a Source's package definitions.
Discovery Data and installed packages can refer to different Source Revisions.
_Avoid_: Source (for one revision), package version

**Moving Reference**:
A Source Reference that can select a later Source Revision during upgrade.
_Avoid_: Pin, installed revision

**Package ID**:
A source-qualified name that identifies a Package choice.
It does not identify one installed occurrence of that Package.
_Avoid_: Profile Entry, native entry key

**Package Resolution**:
The selection of an unambiguous Package ID and Source Reference from a
requested name. It does not establish Host Availability or installed state.
An excluded Package still counts as a choice when a name is ambiguous.
_Avoid_: Package installation, installed entry selection

**Discovery Data**:
Disposable, derived metadata used to search packages, show package information,
and resolve package names. It does not establish installed package state.
_Avoid_: Search database, metadata cache

**Catalog Index**:
The generated record of Source provenance, target systems, and eligible or
excluded package entries. It is a status claim, not proof of a successful build.
_Avoid_: Support manifest, classifier, hosted API

**Target System**:
An operating-system and processor-architecture combination for which package
definitions are published.
_Avoid_: Host version, processor architecture (without the operating system)

**Catalog Eligibility**:
The generated claim that a Package has a supported plan for a Target System.
It does not establish Host Availability or a successful build.
_Avoid_: Verified package, installed package

**Catalog Exclusion**:
A recorded refusal to produce a Package plan for a Target System.
It records one reason and can include a detail.
_Avoid_: Failed build, Host Availability

**Host Availability**:
The assessment of whether a Package's requirements permit installation on the
current host. It can be unknown when requirements or host information cannot be read.
_Avoid_: Catalog Eligibility, runtime verification

**Fixed Reference**:
A Source Reference that does not advance during upgrade.
_Avoid_: Pin, pinned version

## Taps

**Tap**:
A public repository of additional Homebrew Cask definitions.
Tap Approval applies to its repository origin.
_Avoid_: Formula repository, installed package collection

**Tap Approval**:
The user's permission to import a Tap from a specific repository origin,
including later updates from that origin.
_Avoid_: Package installation, revision approval

**Tap Registry**:
The record of Tap Approvals by repository origin.
An approval record does not establish that a Tap Snapshot is available.
_Avoid_: Package inventory, catalog revision record

**Tap Snapshot**:
A published set of generated package definitions and its matching Catalog
Index for one exact Tap revision. It is separate from a profile Generation.
_Avoid_: Generation (without qualification), Discovery Data cache

**Saved Catalog**:
The locally retained Catalog Index of a Tap Snapshot, bound to its origin
and revision.
_Avoid_: Tap Approval, installed package state

**Tap Publication**:
The change that makes a validated Tap Snapshot active for future package
choices. It does not itself install a Package.
_Avoid_: Package installation, profile Rollback

**Tap Import**:
The preparation of generated package definitions and a matching Catalog Index
from one Tap revision. It is separate from Tap Publication and package installation.
_Avoid_: Package installation, Tap Approval

## State

**pkg Profile**:
The one native Nix profile that holds a user's packages. Nix owns its
entries, generations, and roots.
_Avoid_: Manifest, lock, package state engine

**Generation**:
An immutable snapshot of the native pkg Profile.
It is separate from a Tap Snapshot.
_Avoid_: Version, release, product snapshot

**Profile Entry**:
One installed occurrence recorded in the pkg Profile, with its source and
realized package outputs. Its native identity is separate from a Package ID.
_Avoid_: Package ID, catalog entry

**Profile Change**:
A change to installed Profile Entries, the active Generation, or retained
Generations. The native pkg Profile determines what changed.
_Avoid_: Catalog refresh, Tap Publication, launcher sync

**Rollback**:
The selection of the previous or a specified Generation as the active
pkg Profile.
_Avoid_: Migration, state restore

**Upgrade**:
The package operation that advances selected Profile Entries through their
recorded Source References. A Fixed Reference does not advance.
_Avoid_: Source refresh, Tap Publication

**Launcher**:
A pkg-owned macOS app entry point derived from an active Profile Entry.
_Avoid_: App install, app copy

**Launcher Refresh**:
The update of pkg-owned Launchers to match active Profile Entries.
It can fail after a Profile Change has completed.
_Avoid_: Package installation, profile repair

**Restored App Copy**:
A disposable app copy that retains the vendor's external signatures.
Its presence does not establish that the package is installed.
_Avoid_: Package inventory, repaired signature

## Runtime

**Nix Runtime**:
The external Nix installation owned and managed by the user.
_Avoid_: Managed Nix, Base Nix, bundled Nix

**Client**:
The standalone pkg executable that provides package commands and output.
_Avoid_: Installer, agent, service
