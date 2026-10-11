# Design

## States and names

Profile reads have three outcomes: installed, absent, and unknown. A failed
read produces unknown state and a failed command status. Cask availability
uses the same catalog status and host macOS range in info, search, and install.

Installed entries expose a package ID and the native entry ID. Remove and
upgrade resolve package IDs and unique names from the native manifest.
Ambiguity requires a choice. `--entry` selects exact native entry IDs.
No persistent installed-package mapping is added.

## Search

Search treats text literally unless `--regex` is set. Query words all match.
Exact IDs precede display names, name prefixes, other name words, and description words.
Short substrings inside unrelated words do not match. Within a match class,
identity matches precede display-only matches. General CLI/client families
precede helper tools; newer major versions within that family precede older
ones. Library descriptions lower relevance unless the query requests them.
Normal search hides known language, plugin, and build dependency namespaces,
Nixpkgs library descriptions, and explicit `@` or development variants. An exact attribute,
namespace, or variant query can reach them; `--broad` exposes them all.
An explicit library term such as `sdk` includes matching libraries. These
are conservative metadata rules, not a classification of built executables.
Regex search includes development packages and variants. Availability
still uses the normal cask rules.

If no literal result exists, name-only edit distance supplies conservative
typo alternatives. Three-character terms such as `aws` never use fuzzy
matching. Longer terms permit one edit, or two for eight or more characters,
including an adjacent transposition. `--no-fuzzy` disables the fallback.
Results identify approximate matching. Install keeps exact resolution and
can show a short list of discovery suggestions after a failed bare name.
Bare-name exact attribute probes use the current immutable source revision.
Alias lookup may read only complete prepared data from that revision. A
missing snapshot does not start a full scan; qualified IDs remain available.
A short alias that only resolves to a hidden development package or variant
requires an exact package ID. Hidden aliases still count for ambiguity with
other sources. Install suggestions precede the broad inspection instruction;
they never select a package automatically.

Search combines official Nixpkgs, Homebrew Casks, and all registered cask taps.
Source-qualified queries select their source. `--source homebrew/cask` selects
the official casks; `--source cask` includes all cask sources.
Human output shows 20 results by default. `--limit` and `--all` control the result count.
JSON returns all results unless a limit is supplied. A temporary SQLite
spool orders results with bounded memory. Exact unavailable casks remain
visible. `--unavailable` includes other unavailable matches.

Search reads complete local snapshots without Nix or network access.
Snapshots retain their source, system, revision, and preparation time.
Catalogs older than one day are marked stale. A missing catalog produces
a short preparation instruction. Search never starts a full scan implicitly.
Preparation stages complete files and atomically replaces each source.
Failure retains the previous complete file. Sources can succeed independently.

## Preparation and refresh

`pkg setup` prepares catalogs and enables daily background refresh.
`pkg setup --no-background` prepares only the catalogs.
The installer runs setup when Nix is available. Explicit environment options
can disable preparation or background refresh. Preparation failure does not
remove the installed client and is reported as incomplete setup. Background
setup is still attempted so a later job can retry. Cancellation stops setup.

`pkg background enable`, `disable`, and `status` control a per-user job.
On macOS the job is a LaunchAgent with StartInterval=86400 and no KeepAlive.
On Linux it is a systemd user oneshot service and timer. The job calls
`pkg update --catalogs-only`, has no interactive input, and never imports
tap Ruby or changes the profile. Logs record refresh failures. User commands
can still refresh metadata without a scheduler. No root service is added.

References: [Apple launchd guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
[systemd timer documentation](https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html).
