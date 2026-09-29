# Design: Improve runtime discovery and installer shell setup

Decision record for the alpha.4 change. Numbered D1 to D6.

## D1: Discovery order and candidate rule

Default discovery checks, in order:

1. Every `PATH` entry, left to right, for a file named `nix`.
2. `/nix/var/nix/profiles/default/bin/nix` — the stable system profile
   used by the Determinate Nix installer and the multi-user Nix
   installer on both supported systems.
3. `$XDG_STATE_HOME/nix/profile/bin/nix`, default
   `~/.local/state/nix/profile/bin/nix` — the modern per-user Nix
   profile. It is checked before the legacy location, so a machine with
   both uses the current one.
4. `~/.nix-profile/bin/nix` — the legacy per-user profile created by
   older single-user Nix installers. It stays because the vendor
   documents it as stable, and such an install never appears in `PATH`
   for other users' shells.

A candidate counts only when it is a regular file with an executable
bit. Symlinks to executables resolve through `metadata`, which follows
links. Nothing else is scanned; pkg never walks the disk.

The first usable candidate wins. pkg then probes `nix --version` as
before and keeps the full version text. `std::fs::canonicalize` still
resolves the final absolute path, so a symlinked profile reports the
real store path.

## D2: Configured runtime keeps priority and fails alone

When `runtime.nix` is set in `~/.config/pkg/config.toml`, discovery uses
only that value: an absolute or relative path is used directly, a bare
name is searched in `PATH`. If that one value is unusable — absent, not
an executable file, or its version probe fails — discovery fails with an
error about the configured value. Default discovery never runs as a
fallback, because a silent fallback would use a different runtime than
the one the user pinned, against a different daemon and store.

## D3: Two distinct failure modes

- `MissingExecutable` (configured): names the configured value, states
  that pkg will not fall back, and points at the config file.
- `NotFound` (default): names what was searched — the `PATH` entries
  plus the stable profile paths — and gives one useful next action
  for each common cause: add the Nix `bin` directory to `PATH` (the
  common case on a working machine), set `runtime.nix`, or install
  Determinate Nix from the vendor if it is truly absent. Reinstall
  advice appears only in the truly-absent case, as one option among the
  three.

## D4: Managed shell block, not a framework

The downloader appends one managed block between two marker comments to
one startup file per supported shell. The block contains plain
`case`/`export PATH` lines with each directory single-quoted. It needs
no eval, no subcommand, and no external tool at shell startup; this is
simpler and more robust than `eval "$(pkg shellenv)"`. `pkg shellenv`
keeps its documented print-only behavior for users who prefer manual
setup.

Directories added, in order: the install `bin` directory (the real
install destination, so `PKG_INSTALL_BIN` custom locations work), the
detected Nix `bin` directory when Nix was found (always, also for a PATH
hit, because that entry can be a one-time export), then the `pkg`
profile `bin` directory (`$XDG_STATE_HOME/nix/profiles/pkg/bin`,
default `~/.local/state/nix/profiles/pkg/bin`).

File selection:

- Bash: two files. Interactive Bash reads `~/.bashrc`, login Bash reads
  the first existing login file. The block goes into `~/.bashrc` and
  into the first existing of `~/.bash_profile`, `~/.bash_login`,
  `~/.profile`; when none exists, a new `~/.profile` is created. pkg
  never creates `~/.bash_profile` or `~/.bash_login`, because a new
  login file would hide an existing `~/.profile`.
- Zsh: `$ZDOTDIR` (when set to an absolute path) or `$HOME`; the
  existing `~/.zshrc`, otherwise a new `~/.zshrc`.
- Other shells, including Fish: no file is written. The downloader
  prints the plain directory list, so the user adds the paths in the
  shell's own syntax. No unverified per-shell command line is printed.

Rerun rule: when both markers exist with identical content, the
downloader changes nothing. When both markers exist with different
content, the downloader replaces only the text between its own markers —
that text is pkg's own block, never user content. The new content is
written through a `mktemp` sibling file in the same directory plus an
atomic rename, so the pending name is never predictable and unrelated
same-named files cannot be hit. The block file path and the escaped
checksum filename rule (a leading backslash marker when a path holds a
literal backslash) are passed or stripped as raw strings, never through
`awk -v`, which would process backslash escapes. A startup file that is
a symlink is reported and skipped, so the downloader never writes
through a link to an unknown target. Unrelated lines, file modes, and
user settings are never touched.

## D5: Nix detection without a correct PATH

The downloader detects Nix the same way default discovery does:
`command -v nix`, then the stable profile paths, first usable
executable wins. It reports the path it found. The detected Nix `bin`
directory always enters the managed block, also for a `PATH` hit,
because a `PATH` entry can come from a one-time export; this keeps
`pkg doctor` and `nix` itself working in new shells. When no Nix is
found, the downloader finishes the client install, prints the vendor
link, and states plainly that Nix is missing. The final report states
only the actual setup result: it never claims new terminals get `nix`
when none was found, and never claims a completed setup on opt-out,
symlink skip, unsupported shell, or failure. It never starts an
installer.

## D6: Opt-out and the parent shell

`PKG_INSTALL_SHELL_SETUP=0` skips every startup file change and prints
the manual lines instead; this serves CI and custom install
destinations. The downloader cannot change the shell that ran it. After
a setup it prints one exact `export PATH=...` command for the current
terminal for Bash and Zsh, the plain directory list for other shells,
and tells the user that a new terminal picks the setup up automatically
only when the setup completed. Tests exercise this through temporary
`HOME`, `ZDOTDIR`, and install destinations only; no test writes a real
user file.
