## Purpose

A standard Nix flake turns the generated catalog into ordinary Nix
packages with generic data-driven builders on both target systems,
without Brew, Ruby, per-package files, import-from-derivation, or a
compiler at evaluation time.

## ADDED Requirements

### Requirement: Data-driven generic builders over committed data

The Cask source SHALL read the committed generated catalog as plain
JSON data and SHALL expose one derivation per eligible token and target
system through generic builders. The source SHALL NOT contain
per-package Nix files, token lists, or per-token overrides. Vendor
strings SHALL reach shell commands only as quoted data from the plan,
never as generated Nix or shell syntax.

#### Scenario: One token builds without the catalog

- **WHEN** a user requests one package attribute
- **THEN** only that token's plan is evaluated and built and no other
  catalog entry is forced

### Requirement: Lazy evaluation without import-from-derivation

The whole source SHALL evaluate with import-from-derivation disabled.
Requesting the catalog index or status SHALL NOT force any derivation
build. One broken or unbuildable record SHALL NOT fail evaluation of
other records or the index.

#### Scenario: Index read with IFD disabled

- **WHEN** the client evaluates the catalog index with
  `allow-import-from-derivation false`
- **THEN** the index returns without building any package

### Requirement: macOS artifacts keep bundles intact and expose apps and CLIs

The macOS app builder SHALL place app bundles intact under the output
Applications directory, expose declared binaries with their declared
link names, and link declared completions and manpages when present.
Extraction, copying, renaming, and wrapping SHALL be declarative steps
inside the Nix output. The builder SHALL NOT patch bundle contents in
ways that break signatures.

#### Scenario: App plus CLI install

- **WHEN** an eligible plan declares an app and a renamed binary
- **THEN** the output contains the intact bundle and a runnable CLI
  link with the declared name, with no per-token override

### Requirement: Generic macOS minimum check at build time

The macOS builder SHALL read the installed bundle's declared minimum
system version read-only, compare major and minor versions against the
declared baseline, and SHALL fail with a clear message when the bundle
requires a newer system. This check exists because the constraint can
live in the bundle's hidden plist, invisible to cask metadata.

#### Scenario: Bundle demands a newer macOS

- **WHEN** an eligible bundle's Info.plist minimum exceeds the declared
  baseline
- **THEN** the build fails naming the required version and no
  partially usable output is published

### Requirement: Structural pkg payload rule

The pkg builder SHALL accept an archive only when build-time inspection
shows a relocatable app-bundle layout: a payload `Applications`
directory of `.app` bundles, a top-level `.app` bundle, or a bare
`Contents` directory rebuilt as a bundle. Plain files and any other
component shape fail the build; there are no installer scripts,
plugins, nested component pkgs, or choice-requiring distribution
logic, and no system locations such as Library files, services,
drivers, or privileged helpers. Essential payload components SHALL NOT
be silently dropped: a component that cannot be placed under the
supported layouts SHALL fail the build. The builders SHALL NOT invoke
the system installer, vendor installers, privileged helpers, services,
or driver setup.

#### Scenario: pkg with preinstall script

- **WHEN** a pkg archive contains a preinstall script
- **THEN** the build fails naming the script and the system installer
  is never invoked

#### Scenario: pkg with system payload

- **WHEN** a pkg payload contains a launchd service definition or
  driver outside the supported layouts
- **THEN** the build fails naming the component instead of installing a
  partial result

### Requirement: Linux binaries are patched with a bounded generic closure

The Linux binary builder SHALL accept static executables unchanged and
SHALL patch dynamic executables with Nixpkgs auto-patching against a
bounded generic runtime-library closure. Unresolved libraries SHALL
fail the build naming the missing library. Built binaries SHALL run
against store paths and SHALL NOT depend on host `/usr` or `/lib`
libraries.

#### Scenario: Dynamic ELF binary

- **WHEN** an eligible Linux binary needs shared libraries available in
  the generic closure
- **THEN** the built binary runs against store paths with no host
  library dependence

#### Scenario: Unknown library

- **WHEN** a binary needs a library outside the closure
- **THEN** the build fails naming the missing library rather than
  claiming host provision

### Requirement: AppImages wrap through the pinned Nixpkgs helper

The AppImage builder SHALL wrap payloads with the pinned Nixpkgs
`appimageTools.wrapType2`, whose API is verified in the locked nixpkgs
revision. Builders SHALL NOT hand-roll extraction environment tricks
or apply blanket auto-patching over the image; the helper owns that
behavior. Runnability claims SHALL be limited to what was actually
verified; graphical launches SHALL NOT be claimed from headless
verification.

#### Scenario: AppImage wraps and runs

- **WHEN** an eligible Linux AppImage is built and executed in a
  verified environment
- **THEN** the exposed launcher runs the image through the Nixpkgs
  helper, not a bare unrunnable copy

### Requirement: Single-package lifecycle

Plans SHALL carry no dependency output. A record with any cask or
formula dependency is excluded at generation time, so every build is a
single package with no closure expansion, no wrapper sibling logic,
and no second dependency manager.

#### Scenario: Dependent cask is refused

- **WHEN** a user asks to install a token whose record declared a cask
  dependency
- **THEN** it is refused with the recorded exclusion reason before any
  build and the native lifecycle stays single-package

### Requirement: Path and link safety before and after writes

Builders SHALL validate extraction paths and vendor symlink targets
before any payload is written: members must stay inside the staging
root and vendor links must resolve within the staged tree, allowing
legitimate in-tree relative links. Builder-created links MAY point
inside the output or at declared Nix store inputs. After installation,
builders SHALL verify that no vendor payload link or copied path
escapes the output or declared inputs, failing the build otherwise.

#### Scenario: Escaping archive member

- **WHEN** an archive member or vendor link would resolve outside the
  staging tree
- **THEN** the build fails before the escaping path is written

#### Scenario: Wrapper link to a Nix input

- **WHEN** a builder-created launcher links to a wrapped runtime tool
  or dependency output in the Nix store
- **THEN** the link is accepted as a declared input, not treated as an
  escape
