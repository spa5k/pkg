# Design

## Name choice

One rule picks the package for a bare name:

1. Collect exact matches from Nixpkgs, Homebrew Casks, Homebrew
   Formulae, and saved taps. A Nixpkgs match is a top-level attribute or
   an attribute whose name is exposed without a package set prefix.
2. Remove matches that cannot install on this computer: excluded or
   unsupported Casks and Formulae, and hidden package sets (language
   packages, plugins, tests, bootstrap sets).
3. Pick the first source in this order: Nixpkgs, Casks, Formulae, saved
   taps. Two matches in the same source stay ambiguous.
4. When no exact name matches, match the main command of a package
   (`meta.mainProgram` for Nixpkgs, `executables` for Formulae), with
   the same order.

The install result names the choice. The next line names the other
choices, for example `Also available: cask:firefox`.

The local search data answers the rule when it is ready. Nix answers it
when the data is missing.

## Search output

Search ranks all sources in one list. Rows with the same name from
several sources become one row: the row that the name rule picks. A row
shows the shortest ID that installs it: the bare name when the rule picks
that row, otherwise the qualified ID. Columns are name, version, and
description. A source column shows only for non-Nixpkgs rows. The
description column is cut to the terminal width. A mark shows installed
packages. `--source` keeps its meaning. JSON output is unchanged and
keeps every row.

Ranking puts top-level Nixpkgs attributes before nested package sets
with the same name.

## Profile change summaries

A mutation reads the profile manifest before and after the change. The
difference gives added, removed, and changed packages with versions. The
version comes from the store path name. Install, remove, upgrade, and
rollback print this summary. `pkg history` uses the same difference
between neighboring generations.

## Client update

When `pkg update` replaced the client, it runs the new client with a
hidden option to refresh the search data and skip the client check. The
old process waits and returns its exit status.

## Color

`output::paint` adds color only when the stream is a terminal, color is
not turned off, `NO_COLOR` is empty or unset, and `TERM` is not `dumb`.
`output::Table` pads columns and cuts the last column to the terminal
width.
