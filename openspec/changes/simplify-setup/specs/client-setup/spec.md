## ADDED Requirements

### Requirement: One install command without options
The installer SHALL take no arguments and SHALL install the client to one fixed per-user location.

#### Scenario: An argument is given
- **WHEN** the user runs the installer with an argument
- **THEN** the installer prints its usage and exits with status 2
- **AND** the installer changes no file

#### Scenario: A first install with Nix present
- **WHEN** the user runs the installer and a working Nix exists
- **THEN** the installer installs the client and its completions
- **AND** the installer sets up the user's shell
- **AND** the installer enables the daily job

### Requirement: Nix is offered once
The installer SHALL offer to install Nix only when no Nix candidate exists and a terminal is available.

#### Scenario: The user approves the Nix install
- **WHEN** no Nix exists and the user answers yes
- **THEN** the installer runs the Determinate Nix installer
- **AND** the installer adds the new Nix to the managed shell setup

#### Scenario: No terminal is available
- **WHEN** no Nix exists and no terminal is available
- **THEN** the installer installs only the client
- **AND** the installer prints the Nix install command

### Requirement: Only a working client is installed
The installer and the client update SHALL run a new client before it replaces the old client.

#### Scenario: The new client does not run
- **WHEN** the new client fails its version check
- **THEN** the old client stays in place
- **AND** no startup file changes

### Requirement: The client updates itself
`pkg update` and the daily job SHALL update the installed client to the newest published release after the checksum is verified.

#### Scenario: A development build runs
- **WHEN** the running process is not the installed client
- **THEN** no client update occurs

#### Scenario: The update fails
- **WHEN** a download or the checksum fails
- **THEN** the old client stays in place
- **AND** the search data refresh continues

### Requirement: Search prepares its own data
A search SHALL prepare missing search data before it reports results.

#### Scenario: The first search
- **WHEN** the user searches and no search data exists
- **THEN** pkg waits for a running refresh or prepares the data
- **AND** pkg shows the results

### Requirement: The client can be removed
`pkg self uninstall` SHALL remove the client and the files that the installer owns, and SHALL keep Nix and installed packages.

#### Scenario: Removal without a terminal
- **WHEN** the user runs the command without a terminal and without `--yes`
- **THEN** pkg removes nothing and exits with status 2
