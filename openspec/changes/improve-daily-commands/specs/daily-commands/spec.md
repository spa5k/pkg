## ADDED Requirements

### Requirement: Short native error causes
The client SHALL report a failed native evaluation with one short cause and SHALL NOT print the evaluated expression in normal output.

#### Scenario: A removed Nixpkgs alias
- **WHEN** the user runs `pkg install python`
- **THEN** the client prints the Nixpkgs message for `python`
- **AND** the output has no Nix expression and no trace

### Requirement: The new client refreshes search data
After `pkg update` replaces the client, the new client SHALL refresh the search data.

#### Scenario: A client update changes the data format
- **WHEN** `pkg update` installs a newer client
- **THEN** the newer client writes the search data
- **AND** the next search does not prepare the data again

### Requirement: One choice for a bare name
The client SHALL resolve a bare name in the order Nixpkgs, Homebrew Casks, Homebrew Formulae, and saved taps, and SHALL ignore entries that cannot install on this computer and hidden package sets.

#### Scenario: Nixpkgs and a Cask have the same name
- **WHEN** the user runs `pkg install slack`
- **THEN** the client installs `nixpkgs:slack`
- **AND** the client names `cask:slack` as another choice

#### Scenario: Only an excluded Cask shares the name
- **WHEN** the user runs `pkg install firefox` and the Cask is excluded
- **THEN** the client installs `nixpkgs:firefox` without an ambiguity error

#### Scenario: A command name
- **WHEN** the user runs `pkg install rg` and no package is named `rg`
- **THEN** the client installs the package whose main command is `rg`

### Requirement: One search list
Human search output SHALL show one ranked list with one row for each package and SHALL fit the terminal width.

#### Scenario: The same package in two sources
- **WHEN** Nixpkgs and Homebrew Formulae both have `ripgrep`
- **THEN** search shows one `ripgrep` row
- **AND** JSON output keeps both rows

### Requirement: Upgrade all by default
`pkg upgrade` without arguments SHALL upgrade all installed packages.

#### Scenario: Nothing to upgrade
- **WHEN** the user runs `pkg upgrade` and no package changes
- **THEN** the client prints `Everything is up to date.`

### Requirement: Results name packages and versions
Install, remove, upgrade, rollback, list, and history SHALL name packages with their versions.

#### Scenario: An upgrade changes a version
- **WHEN** an upgrade changes ripgrep from 15.1.0 to 15.2.0
- **THEN** the client prints `Upgraded ripgrep 15.1.0 → 15.2.0`

#### Scenario: A new profile has no history
- **WHEN** the user runs `pkg history` before any change
- **THEN** the client prints that no changes exist yet

### Requirement: Color only on terminals
The client SHALL use color only on terminals and SHALL print plain text for pipes, `--no-color`, and a nonempty `NO_COLOR`.

#### Scenario: Output to a pipe
- **WHEN** the output goes to a pipe
- **THEN** the output has no ANSI escape sequences
