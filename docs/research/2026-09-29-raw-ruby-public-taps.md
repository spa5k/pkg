# Raw Ruby Casks and public taps

Checked on 29 September 2026. Plan only, after PR #81. No third-party
Ruby ran. No application archive was downloaded.

## Recommendation

Use a small maintainer exporter over a pinned Homebrew checkout. Keep Rust
for input validation, eligibility rules, and catalog generation. Keep Nix
as the client installation engine. Commit the exported input and generated
catalog. This needs no Brew or Ruby on user machines and no hosted catalog
API. Discover all Cask files in each configured source. Do not add package
allowlists.

This is a maintenance judgment, not a measured result. Homebrew already
owns Ruby evaluation, DSL behavior, and serialization. Reusing that code
avoids a second Ruby evaluator in Rust. It adds a Ruby build environment
and a small adapter to private APIs. It does **not** make all public taps
convertible. Some definitions depend on installed files or host state.

The earlier note rejected raw repositories because official JSON covered
the original scope. Public taps change that tradeoff. Their source can
contain Casks that the official JSON catalog does not publish.

## Verified upstream path

All Homebrew source links below use commit
`cc9ff034b1d98cdd4061f68cf715bb967c483a8f` (28 September 2026).
Context7 was queried first. Source inspection resolved the exact behavior.

The official `generate-cask-api` command:

1. Opens `CoreCaskTap`, so this command alone is not a public-tap exporter.
2. Uses `Homebrew::API.with_no_api_env` and `Cask::Cask.generating_hash!`.
3. Simulates the newest supported macOS release on ARM.
4. Calls `Cask::CaskLoader.load(path)` for each Cask file.
5. Calls `cask.to_hash_with_variations` and `JSON.pretty_generate(hash)`.

Our proposed adapter would replace source enumeration and output handling.
It should use explicit local paths, never resolve a token through a moving
API. [Official exporter][export]

At this pin, the single-target Cask method is **`to_h`**. The variation
method is `to_hash_with_variations`. Do not assume a Cask `to_hash` or
`to_json` method provides the required schema. The `info` command also
serializes `to_h` results through `JSON.pretty_generate`.
[Cask data methods][cask] [Info command][info]

Generation mode replaces common Homebrew and home paths with placeholders.
Use a fresh process because it changes global state. Preserve those
placeholders until the Rust adapter maps supported paths. Do not replace
arbitrary absolute paths with guessed Nix paths. [Generation mode][hashable]

These are private integration points. Homebrew explicitly says the loader
API can change without notice. Pin the checkout and runtime dependencies.
Check adapter behavior before each pin update. A single copied loader file
is insufficient; it imports the Homebrew runtime and other modules.
[API status][api-status] [Loader dependencies][loader]

## Ruby execution and installation are separate

The path loader checks tap trust, reads the file, and calls `instance_eval`.
It clears selected sensitive environment variables, but this is not a
sandbox. Raw Ruby can call `require`, load tap helpers, inspect files, or
start processes. Retain the complete pinned tap tree and its paths; a
standalone Cask file can lose required helpers. Keep tap trust confined to
the disposable compiler environment. [Path loader][loader] [Trust gate][trust]

The DSL runs selected language blocks. It evaluates the `livecheck`
declaration block and `caveats` blocks during metadata loading at this pin.
This differs from the cookbook's general statement that caveats blocks are
deferred. No livecheck update scan is needed for export, but arbitrary Ruby
inside these declarations can still run. [DSL implementation][dsl]

Legacy flight blocks are stored as callbacks. Their bodies run in install
or uninstall phases. JSON records their presence with a null value; it
does not preserve their code. An `installer script:` declaration stores a
command; the installer runs it in its installation phase.
[Flight callbacks][flights] [Installer artifact][installer]

Structured `preflight_steps` and `postflight_steps` survive as step data.
The DSL evaluates a block to build that data. The step runner executes it
later. Thus `run` inside a steps block describes a future command; it is
not permission to run that command during export. Keep these records.
Initially exclude active install steps and legacy install callbacks unless
the generic builder implements their full behavior. Never silently drop
them. [Steps DSL][steps-dsl] [Steps serialization and phases][steps]

Recommended boundary: a disposable VM, no credentials or host home mount,
read-only source inputs, no network during evaluation, bounded resources,
and a small output channel. Fetch pinned sources before evaluation. Never
call the Cask installer. A blocked helper or network request must produce
an explicit error, not an incomplete eligible package.

## Targets and platform evidence

The relevant simulation inputs are:

| pkg target | Homebrew simulation | Homebrew tag |
| --- | --- | --- |
| `aarch64-darwin` with macOS 15 baseline | `os: :sequoia, arch: :arm` | `arm64_sequoia` |
| `x86_64-linux` | `os: :linux, arch: :intel` | `x86_64_linux` |

`SimulateSystem` accepts OS symbols and ARM/Intel symbols. `OnSystem` reads
that state. A generic `:macos` means the oldest version for release
conditions; it is unsuitable for our baseline. Simulation selects a
release family, not macOS patch version `15.7.7`.
[Simulation][simulate] [Target conditions][on-system]

Homebrew supports macOS simulation on Linux. This supports its DSL, not
general host emulation. It does not change `RUBY_PLATFORM`, provide macOS
commands, or make arbitrary filesystem tests target-correct. Prefer a
matching macOS ARM worker and Linux x86 worker for the first proof. Export
each target in a fresh process. Do not promise that one Linux worker can
evaluate every macOS tap. [Linux simulation extension][linux-simulate]

`to_hash_with_variations` adds `supported_platforms`; `to_h` does not.
Support checks include declared OS, architecture, macOS limits, nonempty
version/checksum/URL, and an installable artifact. They do not reject
`:no_check` or prove a payload works. Variations are partial overrides.
Missing Linux variations do not prove lack of Linux support. Keep explicit
target eligibility checks after export. [Platform checks and variations][cask]

The variation exporter re-evaluates the stored Cask block. It does not
reload all top-level file statements for each target. A value computed
outside that block can therefore retain the initial host selection.
This is a source-based reason to load each target afresh and test output
agreement for supported DSL cases. [Refresh implementation][cask]

For a native-per-target adapter, a narrower source-backed sequence is
available. Inside no-API mode, generation mode, one language setting, and
the selected `SimulateSystem` context, load the explicit path. Then call
`cask.platform_supported?(tag, installable: true)` and serialize `cask.to_h`.
This predicate is the one used by upstream variation export, but it does not
refresh the cask. Capture the target/tag and boolean as adapter evidence;
do not label it a full upstream `supported_platforms` array. Avoid variation
and language expansion in this path. This sequence is a proposed integration
based on source inspection, not an executed prototype. [Platform predicate][cask]

## Public tap layout and identity

A tap is normally a Git repository. `owner/homebrew-name` has tap identity
`owner/name`. Casks are discovered recursively under `Casks/**/*.rb`.
Both `Casks/token.rb` and `Casks/t/token.rb` occur. Formula directories are
a different input class and must not enter this compiler.
[Tap guide][tap-guide] [Tap discovery][tap]

Keep a canonical source-qualified identity, such as
`nikitabobko/tap/aerospace`. Homebrew's `full_token` is qualified for
third-party taps, but not its core Cask tap. Do not key a merged catalog
only by bare token. Reject duplicate tokens within one tap rather than
adopt Homebrew's path-length preference. Bare-name collisions across taps
need an ambiguity error or explicit source selection.
[Full token][cask] [Duplicate path behavior][tap]

Three source-only samples show why export success is not enough:

| Public tap and exact source | Observation | Plan consequence |
| --- | --- | --- |
| [nikitabobko/tap, `aerospace.rb`][aerospace] at `9ac0bfc08904719c52a63101fa7e1e133a23bda2` | App, binaries, `postflight_steps`, and a top-level `Dir` glob under `staged_path` for manpages. | Empty staging can silently omit manpages. Metadata-only export does not reconstruct payload-derived declarations. Preserve the steps and report the limitation. |
| [gcenx/wine, `game-porting-toolkit.rb`][wine] at `f5bd24f0d90edb887c3f4ca8d078dee285bf8838` | App and binary links; postflight commands remove quarantine and re-sign the app. | Exclude unsupported active steps. An app stanza alone is insufficient. |
| [hashicorp/tap, `hashicorp-boundary-desktop.rb`][boundary] and [Vagrant][vagrant] at `95b4b4c61bdf90b784b958483d13df167d4ee4fa` | Boundary uses macOS/architecture DSL and an app. Vagrant uses a native package and an uninstall script. | The source works with the proposed loader shape; eligibility still depends on artifact semantics. |

The official tap uses sharded paths, for example
[`Casks/1/1password-cli.rb`][onepassword] at
`a14500be9b8daa5c1cdcb4741df1ae993624483f`. That file uses OS and
architecture substitutions for both target systems.

### A narrow staged-path guard

A possible pinned adapter hook is `Cask::DSL#staged_path`
(`dsl.rb`, lines 682–686). It returns the Caskroom path joined with the
version. Cask exposes the same accessor through delegation. An exporter
could override this accessor while it evaluates source, mark the record,
and raise an export error. Keep the mark even if source code rescues the
exception. This would stop Aerospace's observed glob before it returns an
empty list. [Accessor and DSL method list][dsl]

This is a conservative compatibility rule, not a purity check. It also
rejects harmless staged-path interpolation. Code can use `caskroom_path`,
construct its own path, use helper I/O, or replace methods. The hook does
not cover those cases. Use a reason such as `staged-path-during-metadata`.
Prove the hook with controlled fixtures before adoption. Do not build a
new Ruby evaluator or add per-package exceptions to avoid its exclusions.

## Provenance and proof before implementation

Record the tap URL, canonical tap name, exact Git commit, tree/content
hash, Cask path and hash, and license. Also record the Homebrew commit,
Ruby/runtime lock, adapter version, target, language, fixed environment,
and evaluation date. Homebrew exports source-path/checksum and tap-head
fields, but the outer compiler must verify identity and hashes itself.
Preserve notices per source; public taps need not share one license.

Pinned Ruby inputs alone do not guarantee deterministic output. Code can
read time, random data, host state, or installed state. Use clean inputs,
control available state, and compare repeated exports. Keep failures
visible by source and target. Treat raw output as untrusted data before
Rust validation. Repeated agreement is useful evidence, not proof of
arbitrary Ruby purity.

The first proof should cover source-qualified collisions, nested paths,
tap-local helpers, language selection, target branches, blocked load-time
side effects, legacy callbacks, structured steps, and the staged-file
case above. Use small controlled fixtures for executable cases. Compare
official definitions with upstream export results. Do not download all
application payloads to make metadata generation work.

Open limit: there is no general metadata-only method that proves arbitrary
Ruby did not omit required payload-dependent behavior. Keep that limit
explicit. Do not advertise full compatibility with every public tap.

[export]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/dev-cmd/generate-cask-api.rb
[cask]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/cask.rb
[info]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cmd/info.rb
[hashable]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/api_hashable.rb
[api-status]: https://docs.brew.sh/rubydoc/Cask/CaskLoader.html#load-class_method
[loader]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/cask_loader.rb
[trust]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/trust.rb
[dsl]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/dsl.rb
[flights]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/artifact/abstract_flight_block.rb
[installer]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/artifact/installer.rb
[steps-dsl]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/install_steps.rb
[steps]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/artifact/install_steps.rb
[simulate]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/simulate_system.rb
[on-system]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/on_system.rb
[linux-simulate]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/extend/os/linux/simulate_system.rb
[tap-guide]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/docs/How-to-Create-and-Maintain-a-Tap.md
[tap]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/tap.rb
[aerospace]: https://github.com/nikitabobko/homebrew-tap/blob/9ac0bfc08904719c52a63101fa7e1e133a23bda2/Casks/aerospace.rb
[wine]: https://github.com/gcenx/homebrew-wine/blob/f5bd24f0d90edb887c3f4ca8d078dee285bf8838/Casks/game-porting-toolkit.rb
[boundary]: https://github.com/hashicorp/homebrew-tap/blob/95b4b4c61bdf90b784b958483d13df167d4ee4fa/Casks/hashicorp-boundary-desktop.rb
[vagrant]: https://github.com/hashicorp/homebrew-tap/blob/95b4b4c61bdf90b784b958483d13df167d4ee4fa/Casks/hashicorp-vagrant.rb
[onepassword]: https://github.com/Homebrew/homebrew-cask/blob/a14500be9b8daa5c1cdcb4741df1ae993624483f/Casks/1/1password-cli.rb
