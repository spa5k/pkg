# Improve daily commands

## Why

A test of the daily commands on macOS found defects and friction:

- `pkg install python` printed a 36-line Nix expression and a trace.
- After a client update, the old client wrote the search data. A format
  change then made the first search wait 280 seconds.
- Raw Nix lines appeared in remove, rollback, install, and upgrade output.
- `pkg history` printed nothing for a new profile.
- Common names failed: `firefox`, `slack`, and `google-chrome` were
  ambiguous, `rust` offered a test package and a Vim plugin, `rg`
  suggested a Haskell library, and `node` found nothing.
- Search showed three sections, long IDs, the same package twice, and
  lines wider than the terminal. Internal package sets ranked first.
- `pkg upgrade` without arguments was a usage error.
- Results named Nix entries and generations instead of packages and
  versions. The output had no color.
- `pkg info` and the name check before install took 1.3 to 2.1 seconds.

## What Changes

- Native evaluation errors show one short cause, for example
  "`python` previously pointed to Python 2; use `python3` instead".
- After a client update, the new client refreshes the search data.
- Normal output hides Nix progress chatter and nonfatal Nix messages.
- **BREAKING** A bare name that more than one source has resolves in a
  fixed order: Nixpkgs, then Homebrew Casks, then Homebrew Formulae.
  Entries that cannot install on this computer and hidden internal
  package sets do not count. The result names the chosen package and
  the other choices.
- A bare name also matches the main command of a package, so `rg`
  finds `ripgrep` and `node` finds `nodejs`.
- **BREAKING** Search shows one ranked list with one row for each
  package. A row shows the shortest name that installs it. Lines fit
  the terminal. Installed packages are marked. JSON output keeps all
  rows.
- **BREAKING** `pkg upgrade` without arguments upgrades all packages.
- Install, remove, upgrade, and rollback report package names and
  versions, for example `Upgraded ripgrep 15.1.0 → 15.2.0`.
- **BREAKING** `pkg list` shows name, version, and source. Entry IDs
  show only for duplicates or with `--verbose`.
- `pkg history` shows what each generation added, removed, and changed.
- Terminals get color. `--no-color`, `NO_COLOR`, and pipes get plain
  text.
- `pkg info` and the install name check read the local search data
  when it is ready. They fall back to Nix when it is not.
- Help text uses package words instead of Nix words.
- `remove` and `upgrade` accept the command name of an installed package.
- `pkg rollback` without a number skips generations in which only the
  source revision moved.
- Search data preparation leaves out an entry that alone exceeds the
  memory budget, instead of failing the whole source.

## Capabilities

- New: `daily-commands`.

## Impact

- `crates/pkg-cli`: discovery, profile, output, client, CLI help.
- The nixpkgs search data gains the main command of each package. The
  first search after this change prepares the data again.
- Documents: `docs/commands.md`, `README.md`.

## Non-goals

- A command that lists available upgrades.
- Popularity data for ranking.
- Changes to JSON envelopes, other than added fields.
