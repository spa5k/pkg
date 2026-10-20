# Commands

`pkg` uses one native Nix profile per user. The profile lives at
`~/.local/state/nix/profiles/pkg`. Nix owns package state, generations,
rollback, and roots.

Global options: `--help`, `--version`, `--no-color`, `--verbose`.

Human output shows work stages and a verified result. Install first shows
`Resolving`, then the package name in `Installing`. Terminal progress shows
elapsed seconds during native work. A mutation reports its result only after
the native profile operation and macOS launcher refresh finish. The result
has one line for each changed package, with its name and version:

```text
Installed ripgrep 15.2.0
Removed hello 2.12.1
Upgraded ripgrep 15.1.0 → 15.2.0
```

The version comes from the store path name. A package without a version in
its store path shows its name only. A package with new outputs but the same
version shows `(new build)`. If the profile changes but launcher refresh
fails, the command reports `Partial`, the cause, and `pkg apps sync` as the
next step. Use `--verbose` to see native commands and detailed output.
On a terminal, long operations show one status line that changes in place.
Logs and pipes get plain lines.
Query commands keep JSON output and shell commands keep script-only stdout.

`pkg add` is an alias of `pkg install`. `pkg uninstall` is an alias of
`pkg remove`. Help and shell completions include both aliases.

After installation, `Commands` lists up to ten visible executable names that the
package exposes in the profile. It reads local files and adds no network work.
For example, `nixpkgs:fff` provides the `fff` command. It does not provide `ff`.

Install, remove, and upgrade use the same output filter. Normal output
omits native progress chatter and native summaries, for example
`this path will be fetched`, `removed 1 packages, kept 3 packages`, and
`upgrading '…' from flake '…'`. Native messages marked `error (ignored)` are
also omitted. They are nonfatal native diagnostics. Warnings, trust prompts,
and actual failure causes remain visible. Use `--verbose` to see all native
output.

If two packages provide the same file, installation fails. Normal output
names both sources and the file, for example:

```text
Cause: nixpkgs:bat (Nixpkgs) is already installed and provides bin/bat.
Requested: brew:homebrew/core/bat (Homebrew Formula).
```

An installed package reports `Already installed` with its version, without a
failure. An active entry from the same source and attribute counts as
installed, even when the source moved since the install. `pkg` does not add a
second entry for it. When a newer build exists, the output names
`pkg upgrade NAME`. Use `--verbose` for the native diagnostic. `pkg` does not
select a package or change its priority.

| Command | Behavior |
| --- | --- |
| `pkg search QUERY` | Search local catalogs. Shows ranked package IDs and availability. |
| `pkg info ID` | Show source, attribute, metadata revision, and known limits. |
| `pkg install ID…` or `pkg add ID…` | Resolve all names, then run one native profile add. |
| `pkg list` | Read the dedicated profile. Shows name, version, and source, sorted by name. The source is empty for Nixpkgs. Entry IDs show only for duplicate packages or with `--verbose`. |
| `pkg remove ID… [--entry]` or `pkg uninstall ID… [--entry]` | Resolve unique installed names or package IDs, then remove the selected entries in one native operation. Several entries of one package from one source are removed together. |
| `pkg update` | Update the installed client to the newest release, then refresh official catalogs and saved tap catalogs under their recorded consent. Does not change installed packages. |
| `pkg upgrade [ID…] [--entry]` | Without IDs, upgrade all packages (`--all` does the same). Follow each entry's original reference. Reports fixed references. When nothing changes, prints `Everything is up to date.` After `tap remove`, upgrade of all packages skips removed-source references and forwards the remaining entries by explicit native entry IDs (not native `--all`); upgrading a removed-source entry refuses. |
| `pkg history` | Show retained generations, newest first, with the date and the package changes of each. `*` marks the current generation. A new profile prints `No changes yet.` Reads local files only. `--verbose` prints the native history. |
| `pkg rollback [N]` | Use Nix's native previous or selected generation. Prints the destination and the package changes. Refreshes launchers. |
| `pkg prune --older-than Nd` | Remove only eligible non-current generations. Requires an explicit positive age. |
| `pkg doctor` | Read-only runtime, profile, launcher, daily job, and `PATH` status. Repairs nothing. |
| `pkg self uninstall [--yes]` | Remove the client, completions, shell setup, daily job, settings, and search data. Nix and installed packages stay. Without a terminal, `--yes` is required. |
| `pkg apps sync` | Rebuild pkg-owned macOS launchers from the active profile. |
| `pkg apps setup` | Create private app storage on the Nix volume. Requests administrator access once. |
| `pkg tap add SOURCE [--revision SHA] [--trust]` | Save a public tap. Ask approval before any fetch, import, or Ruby runs. |
| `pkg tap list [--json]` | List saved taps: source, origin, revision, and consent time. |
| `pkg tap update [SOURCE] [--revision SHA]` | Re-import saved tap catalogs at a pinned revision. Runs tap Ruby under the same rules as `add`. |
| `pkg tap remove SOURCE` | Block future use of a source. Installed packages stay. |

Hidden commands: `pkg background enable/disable/status` controls the daily
job. `pkg shellenv` prints Bash/Zsh `PATH` settings. `pkg completion SHELL`
prints completions. The installer uses them. They do not show in help.

Removed commands: `setup`, `pin`, `unpin`, `outdated`, `repair`, `gc`, and
`system uninstall`. Removed options: global dry-run, build approval,
`--keep-going`, and JSONL progress. Use normal CLI usage errors for them.

## Public cask taps

`pkg tap` manages saved public Homebrew Cask taps. A saved tap is converted
locally into ordinary Nix packages. Saved taps live in a per-user state
directory. Catalog format, publication, and sandbox prerequisites are in
[casks](casks.md). The accepted behavior is specified in the OpenSpec
change
[Import public cask taps locally](../openspec/changes/archive/2026-10-12-import-public-cask-taps/proposal.md).

- `SOURCE` is a public GitHub `owner/tap` repository. The canonical
  repository is `owner/homebrew-<tap>`; the common short source
  `owner/tap` maps to `owner/homebrew-tap`. Arbitrary URLs are not
  accepted in this version.
- The first `tap add` for an origin shows the canonical repository and the
  approval scope, then asks for consent. Consent is remembered per origin.
  No fetch, import, or Ruby runs before it. A noninteractive `add` without
  consent must pass `--trust` explicitly. Otherwise it stops and changes
  nothing.
- `tap add`, `tap update`, and the saved-tap phase of `pkg update` execute tap Ruby. The Ruby runtime and
  the Homebrew reader are pinned and supplied by Nix. They run inside an
  ordinary sandboxed Nix derivation. `tap update` may execute source
  revision Ruby under the same restrictions.
- `pkg update` refreshes official catalogs and saved taps. The daily user
  job updates the client and refreshes official catalogs only. It runs no
  tap Ruby.
- Normal `search`, `info`, and `install` of saved catalogs run no source
  Ruby and never prompt.
- `tap remove` blocks future use of a source. It does not uninstall
  packages. After a removal, `pkg upgrade --all` skips removed-source
  references and upgrades the remaining entries by explicit native entry
  IDs; an explicit upgrade of a removed-source entry refuses.
- Updating tap metadata does not upgrade installed packages. Run
  `pkg tap update`, then `pkg upgrade ENTRY` or `--all`.
- Install, upgrade, rollback, and removal stay on the native Nix profile.
  There is no second package state engine.

Example with a real public tap:

```sh
pkg tap add goreleaser/tap
pkg info cask:goreleaser/tap/mcp
pkg install cask:goreleaser/tap/mcp   # only if info reports it eligible
```

This example shows the accepted flow. The final verification passed the
whole import on both hosts (Linux 13 eligible / 17 excluded; macOS
7 eligible / 23 excluded) and the negative product gates on both hosts
(C6/C7). The parent review is complete. The implementation is in [PR #82](https://github.com/spa5k/pkg/pull/82). See
the [plan](../openspec/changes/archive/2026-10-12-import-public-cask-taps/design.md) and the
[verification status](verification/public-taps.md).

## JSON query output

`--json` works only with the five query commands: `search`, `info`,
`list`, `tap list`, and `doctor`. With any other command it is a usage
error (exit code 2). Every JSON result uses the same versioned envelope,
with this shape:

```json
{
  "schema": 1,
  "command": "list",
  "result": []
}
```

`schema` is the envelope version; it is `1` today. Fields listed as
nullable are `null` when the value is unknown or does not apply.

### `search`

The result holds `system`, `sources`, `total`, `approximate`, and `results`. `total` counts
all matches before a limit. If sources fail and no usable rows remain,
search exits nonzero. Healthy sources can still return rows when another
source is unavailable. Warnings go to stderr.

Search uses literal search words by default, without case sensitivity.
Every query word must match a name or description. Exact package IDs come
first. Name matches precede description matches. General CLI/client families
precede helper tools; newer major versions in those families precede older
ones. An identity match precedes a match in the display name alone.
Library descriptions lower relevance unless the query requests them.
Short terms match whole words or name prefixes: `aws` does not match
`claws-mail` or the word `draws`.

Normal search hides known language, plugin, and build dependency namespaces.
It also hides Nixpkgs library descriptions, unwrapped or development variants,
and versioned `@` casks. Exact attribute IDs, namespace
queries, and explicit variant queries can reach them. Use `--broad` to include
them and unrestricted substring matches. `--all` changes the result count;
it keeps the normal relevance rules.

Library queries such as `aws sdk` or `aws library` can show relevant libraries
without `--broad`. The visibility rules use package names and descriptions;
they do not inspect built files or promise that every row exposes a command.

If no literal result exists, search can show similar package names. The
fallback uses one edit for names of four to seven characters, and up to two
for longer names. An adjacent transposition is one edit. Three-character
terms such as `aws` do not use fuzzy matching. Fuzzy matching never searches
descriptions. `approximate` is true only for that fallback. Human output
labels the similar names. Use `--no-fuzzy` to disable it. Install still requires
an exact name. A failed short name shows up to five local suggestions and
never starts a full catalog scan.
A short alias that only refers to a hidden library or variant requires an
exact package ID. Hidden aliases still count for source ambiguity. `pkg install
aws` will suggest a tool such as `nixpkgs:awscli2` instead of selecting a library
through its internal name.

Use `--regex` for a Rust
regular expression. Invalid regex input is a usage error before any file
or runtime lookup. Regex searches include development packages and variants.
`^$ --regex` can match an empty description.

Human output has separate Nixpkgs, Homebrew Formulae, Homebrew Casks, and
saved tap sections. Each section shows its match count. Results stay ranked
within each section. The global limit is shared across nonempty sections,
so a large source does not hide the others. A small limit can show a section
with zero displayed rows and its full count.

Human output shows 20 results by default. Use
`--limit N` or `--all` to change the count. JSON includes all matches unless
`--limit` is supplied. These two options cannot be combined.

Search includes Nixpkgs, official Homebrew Formulae, Homebrew Casks, and registered cask taps
by default. Use `--source nixpkgs`, `--source cask`, `--source homebrew/cask`,
or `--source owner/tap` to select Nixpkgs, all casks, official casks, or one
registered tap. `nixpkgs:aws`, `cask:aws`, and `cask:owner/tap/aws` are also
search queries. A conflicting `--source` is a usage error. Public GitHub
flake IDs remain installable directly; arbitrary remote flakes are not a
global searchable source.

Use `--source brew` or `--source homebrew/core` for formulae. Use
`--source homebrew` for formulae and all cask sources. `brew:aws` and
`homebrew:aws` select the same groups. Formula aliases return the canonical
`brew:homebrew/core/NAME` ID. JSON stays a flat list with global relevance
order and a global limit.

Formulae install through Nix into the pkg profile. Use `pkg install brew:NAME`
or the full `brew:homebrew/core/NAME` ID. Search aliases return this canonical ID.
Eligible Formula rows include `install_command`, such as
`["pkg", "install", "brew:homebrew/core/awscli"]`.

The generated source first checks for a Nixpkgs package with a matching name
or alias, upstream project, and version major. These packages use Nixpkgs
versions. `pkg info brew:NAME` shows `nixpkgs-equivalent` in `formula_kind`.
An explicit `@VERSION` Formula name also requires that version family.
The second route imports a pinned Homebrew bottle through Nix.
It accepts only dependency-free bottles that need no Homebrew relocation,
install hook, service, or special requirement. It checks archive paths,
links, and remaining Homebrew paths before publishing the Nix output.
The bottle importer supports Apple silicon macOS. Linux Formulae use
Nixpkgs equivalents. Linux bottles are refused with `unsupported-bottle-platform`.
`formula_kind` is `formula-bottle` for this route. No Formula Ruby runs.
No Brew command runs. Eligibility is a metadata decision; a build can
still refuse a bottle whose contents fail the checks.

Install, list, info, remove, upgrade, history, and rollback use the native
Nix profile for Formulae. Remove accepts the canonical ID or `brew:NAME`.
Aliases are resolved by search, info, and install. A mixed install checks
all requested Casks and Formulae before it changes the profile.

Normal Formula search hides unsupported packages and declared libraries.
Exact names and `--unavailable` show unsupported packages with their reason.
Exact names, `--broad`, and library or resource queries can show packages
with no declared executables. An absent executable field is not treated as
an empty list. Casks use the same host availability rules in search, info,
and install.

The daily job prepares local catalogs. The first search prepares missing
catalogs itself. `pkg update` refreshes them manually. Normal search uses
local files without Nix or network access. Data older than three days is
marked stale. Verbose output includes catalog age and
revision. A failed refresh keeps the previous complete snapshot.
Formula preparation evaluates the generated `formulaIndex` at a locked Git
revision. It does not fetch the Homebrew API on the user's machine.
The daily catalog job refreshes this source. Maintainers refresh the pinned
Core input with [the Formula generator](../tools/formula-catalog/README.md).

Preparation evaluates metadata in batches of up to 512 attributes.
Encoded path lists stay within 64 KiB. Each evaluator
has a 512 MiB RSS budget. The memory guard stops at 480 MiB. A stopped
batch is split and retried. An entry that still exceeds the budget fails
the refresh. Catalog publication is atomic for each source. A source can
finish when another source fails.

SQLite holds native catalog metadata with its source and revision. Search
uses an 8 MiB page cache. A temporary SQLite spool orders matching rows
with bounded memory. The native Nix profile owns installed state and history.

Source report fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `source` | string | The moving reference queried. |
| `display` | string, nullable | Human source identity. |
| `locked_reference` | string, nullable | Locked native reference. |
| `revision` | string, nullable | Native source revision. |
| `status` | string | `cached`, `stale`, `failed`, or `skipped-platform`. `cached` means no live source check was made. |
| `detail` | string, nullable | Failure or limitation detail. |
| `support_detail` | string, nullable | Why support badges are absent. |

Row fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Source-qualified ID such as `nixpkgs:fd` or `brew:homebrew/core/awscli`. |
| `attribute` | string | Full native attribute. |
| `name`, `version`, `description` | string | Prepared metadata. `description` can be empty. |
| `source` | string | `nixpkgs`, `brew`, or `cask`. |
| `reference` | string, nullable | Locked native reference. |
| `revision` | string, nullable | Native source revision. |
| `system` | string | Search target system. |
| `stale` | boolean | Local catalog is older than three days, or its age is unknown. |
| `support` | object, nullable | `{"status":"eligible"}`, `{"status":"excluded","reason":…,"detail":…}`, or `{"status":"unknown","reason":…}` for Casks and Formulae. Nixpkgs rows have `null`. Eligible is a metadata status, not an installation promise. |
| `install_command` | array of strings, optional | Suggested pkg install command for eligible Formulae. Absent for unsupported Formulae and other rows. |

### `info`

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Qualified ID; an explicit GitHub installable keeps `github:owner/repo#attribute`. |
| `source` | string | Display identity, or the reference when metadata was unreadable. |
| `reference` | string | The moving source reference used. |
| `locked_reference`, `revision` | string, nullable | The locked source Nix resolved. |
| `system` | string | System the query ran for. |
| `attribute` | string | The requested attribute or token. |
| `matched_attribute` | string, nullable | The actual full native attribute; `null` when only recorded cask metadata answered. |
| `name` | string | Package name or the requested attribute. |
| `version` | string, nullable | Evaluated or recorded version. |
| `description` | string | Evaluated description; can be empty. |
| `installed_state` | string | `installed`, `absent`, or `unknown`. A failed profile read produces unknown and exit code 1. |
| `installed_error` | string, nullable | Profile read failure detail. |
| `availability` | object, nullable | Cask host decision: `available`, `unavailable`, or `unknown`, with a reason when needed. |
| `installed` | object, nullable | `{"entry_id":…,"original_url":…,"locked_url":…}` when an installed entry matches the full attribute and the canonical source; else `null`. |
| `cask` | object, nullable | Cask classification, below; `null` for non-cask IDs. |

`cask` is one of:

- `{"status":"eligible","kind":…,"homepage":…}` — the generated index
  marks the token eligible on this system. This is a metadata claim; the
  build proves the payload.
- `{"status":"excluded","reason":…,"detail":…}` — the recorded exclusion
  reason and detail.
- `{"status":"unknown"}` — the token is not in the generated catalog for
  this system.
- `{"status":"unsupported-platform","system":…,"targets":[…]}` — this
  system is outside the catalog's target list.

### `list`

The result is an array with one row per **active** profile entry:

| Field | Type | Meaning |
| --- | --- | --- |
| `package_id` | string | Package ID accepted by remove and upgrade. Duplicate entries of one package from one source select together; other duplicates require an explicit entry choice. |
| `entry_id` | string | Exact native entry ID. Pass it with `--entry` to select one duplicate. |
| `name` | string | Final attribute segment. |
| `version` | string, nullable | Version from the store path name; `null` when the name has no version. |
| `source` | string | The original moving reference installed from. |
| `locked_source` | string | The locked reference Nix resolved. |
| `revision` | string, nullable | Locked revision of the locked source. |

### `tap list`

The envelope `command` is `tap-list`; the envelope is the same versioned
envelope as the other query commands. The result holds one row per saved
tap. Each row carries, at a high level:

- `source` — the saved `owner/tap` identity.
- `origin` — the canonical repository for the source.
- `revision`, `eligible`, `excluded` — the recorded revision and catalog
  counts, when a catalog is recorded.
- `added_unix` — the recorded consent time for the source, in Unix time.

### `doctor`

The result is an array of rows: `component` (string), `status` (string),
and `detail` (string, nullable). Statuses by component: `nix runtime`
`ok`/`missing`; `nix daemon` `ok`/`unreachable`; `pkg profile`
`ok`/`unreadable`/`fresh`; `app launchers`
`healthy`/`unhealthy`/`skipped`/`unknown`/`unreadable`; `config` `present`/`defaults`;
`discovery cache` `present`/`empty`; `background refresh` `on`/`off`;
`shell` `ok`/`not on path`. `doctor` exits nonzero when any status is
`missing`, `unreachable`, `unreadable`, or `unhealthy`. `off` and
`not on path` are listed under `Needs attention:` and do not change the
exit status. It never repairs anything.

## Package IDs

- `nixpkgs:<attribute>` selects a Nixpkgs attribute.
- `cask:<token>` selects a Cask package by bare token. Tokens may use
  lowercase letters, digits, `+`, `.`, `_`, `-`, and `@`. A bare token
  resolves only when it is unique across the official catalog and every
  saved tap catalog. Excluded entries count as matches.
- `cask:owner/tap/token` selects a Cask package from one saved tap. It
  resolves only inside its named source.
- A bare name is valid only when it has one supported exact match. A cask
  bare name must equal one token exactly; a display name never resolves.
  An ambiguous token lists the qualified choices instead of picking a
  source by ordering.
- A saved catalog that is unreadable or malformed fails bare-token
  resolution and names the failed source. It is never silently ignored.
- An unknown `cask:owner/tap/token` source fails with an explicit
  `pkg tap add owner/tap` instruction. It is never converted silently and
  never reinterpreted as Nix code.
- Ambiguous names require a source-qualified ID.
- Explicit public GitHub flake installables with an attribute after `#` are
  accepted.

Default source: `github:NixOS/nixpkgs/nixpkgs-unstable`. Configuration at
`~/.config/pkg/config.toml` can replace the default reference.

## Fixed references

There is no pin state. A fixed package does not advance during normal
upgrade. To change its reference, remove the entry and install the new
reference. Retained generations provide rollback.

## Conflicts

`pkg` reports Nix's conflict and the conflicting entries. It does not
resolve file collisions silently. Remove the conflicting entry first.

## macOS apps

Supported packages expose their app bundles as launchers in
`~/Applications/pkg/`. Launchers refresh after install, remove, upgrade, and
rollback. If a launcher sync fails, the command reports partial completion
and names `pkg apps sync` as the retry. `pkg apps sync` rebuilds launchers
only. It does not change package selection. For public-tap vendor `.app`
bundles, sync requires vendor signatures to assess cleanly, and it
requires Gatekeeper assessments to be enabled (`spctl --status` reports
`assessments enabled`) before codesign/spctl assessment; a rejected app
never changes existing launchers. This gate does not cover CLI binaries
or ordinary Nixpkgs apps, and it does not claim GUI launch or
application runtime isolation.

Some casks store vendor signatures in file metadata that Nix cannot retain.
For these apps, sync restores the original signatures on a private app copy
under `/nix/var/pkg/cask-apps/<uid>/`. Bundled CLI commands use the same
copy. Both paths verify the signatures before use. No file is re-signed.
Other apps continue to use their store bundles.

Run `pkg apps setup` once before using these apps. It creates root-owned
parent directories and a private directory for your account. Normal installs,
syncs, and app use require no administrator access. The copies share file
data with their Nix payload through APFS cloning. Only file metadata,
signature attributes, and later private writes need additional storage.
There is no fallback to a full second copy. The store volume must support
cloning. `du` and Finder can report the full logical size of each view even
when its data blocks are shared.

For a custom location, set `PKG_APP_CACHE_DIR` to an existing absolute,
user-owned directory with mode 0700 on the same volume as the app payload.
Use the same setting for pkg and bundled commands.

Removal removes the native entry and launcher. Cached copies can remain for
running apps. A cache copy alone does not mean that the package is installed.
If Nix later collects the source version, a remaining clone retains its data
blocks. Old views can keep space until removed. There is no automatic cleanup
of old views in this change.
Update apps through `pkg upgrade`. If a copy is missing or damaged,
`pkg apps sync` recreates it from its Nix payload.

Profile mutations and explicit app sync hold one process lock per pkg
profile. The lock covers inventory checks, mutation, verification, and app
sync. Direct Nix commands do not use this lock. Repeating an install skips
an active entry with the same source and attribute. With an explicit output
selection, the output set must also match.

Default rollback selects Nix's previous retained generation. After a
branched history, that generation can contain an older package set.
The command prints the destination generation and the package changes, for
example `Added hello 2.12.1`. Use `pkg history` and `pkg rollback N` to
choose a specific retained generation.
