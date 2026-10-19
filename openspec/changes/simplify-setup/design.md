# Design

## Principle

Give the user one path. Make the right choice for the user. Report only
what the user must know or do.

## Installer

The installer has one fixed layout under `HOME`:

- client: `~/.local/bin/pkg`;
- completions: `~/.local/share/pkg/completions/` as `pkg.bash`, `_pkg`,
  and `pkg.fish`;
- Fish setup: `${XDG_CONFIG_HOME:-~/.config}/fish/conf.d/pkg.fish`.

The internal value `PKG_INSTALL_MODE=test` skips the daily job, so test
homes do not replace the real user job. It is not documented for users.

Archive checks stay as before: a verified checksum, exact regular
members, and reads through standard output only.

The new client is written to a random sibling file. The installer runs
it with `--version`. Only a working client replaces the old client, with
one rename.

Nix detection stays as before. When no candidate exists and `/dev/tty`
opens, the installer asks `pkg needs Nix. Install Nix now? [Y/n]`. On
approval it downloads the Determinate installer to a file and runs it
with `install --no-confirm`. A candidate that does not run is not
replaced; the user must repair it.

The `$SHELL` value selects the shell that gets new setup. Existing pkg
setup in the startup files of other shells is refreshed, not added. The
`PATH` order is the client, Nix, then the package profile.

## Client update

`pkg update` and the daily job compare the running client with the
`SHA256SUMS` file of the latest published release. The update applies
only when the running process is the installed client, so a development
build never replaces it. Product code never starts a shell, so the
update runs in Rust:

1. Download the checksum list and the archive with `curl`.
2. Verify the archive checksum.
3. Read the client member with `tar -xO`.
4. Write a random sibling file and run it with `--version`.
5. Write completions with the new client.
6. Replace the old client with one rename.

A failed update reports one line and does not stop the search data
refresh.

## Search data

The installer does not prepare search data. The daily job starts at
once after it is enabled. A search that finds no data waits for a
running refresh under the refresh lock. If data is still missing, the
search prepares it in the foreground with one status line.

## Removal

`pkg self uninstall` asks for approval on a terminal, or requires
`--yes`. It removes the daily job, the managed shell setup, completions,
settings, the discovery cache, the job log, and the client. Nix and the
package profile stay.

## Rejected options

- Run `install.sh` from `pkg update`. Product code must not start a
  shell.
- A choice of install directory. It adds a decision that most users do
  not need.
- Prepare search data during install. It made the first install take
  minutes.
