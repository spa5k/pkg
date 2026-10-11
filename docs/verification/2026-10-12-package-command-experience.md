# Package command verification — 2026-10-12

Base revision: `4bb554fe7ee40a17ac3c1d23df3b51c30bd6a381`.
Client version: `0.2.0-alpha.15`.
Host: Apple silicon macOS 27.0.1. Toolchain: Rust 1.96.1.
Runtime: Determinate Nix 3.22.1, Nix 2.35.2.

## Behavior

Parser tests pass for both aliases, required arguments, global options,
and `--entry`. Help and Bash, Zsh, and Fish completion include the aliases.

Removal tests replay the ignored `/etc/nix/sentry-endpoint` permission error.
Normal output omits it and the native removal line. Warnings and trust prompts
remain visible. Verbose output includes the raw diagnostic. A fatal removal
error reports its cause and unchanged installed entries, with exit code 1.

Command discovery accepts executable files exposed by the selected active
entry. It excludes directories, text files, missing links, shadowed commands,
inactive entries, and hidden wrappers.

`tools/native-cli-check.py` passed with the development client in disposable
profiles. It covered `add`, `uninstall`, command output, upgrade, rollback,
Formulae, Cask limits, launcher refresh, and cancellation. The terminal showed
elapsed progress during a quiet build before cancellation. Cancellation returned
130 and retained the previous profile in both normal and verbose modes.

A separate terminal check replayed a native trust prompt. Progress stopped
while the prompt waited for input. The reply reached the child. The command
completed and reported the unchanged profile.

A real Nixpkgs `fff` journey passed in a disposable profile:

```text
Resolving: nixpkgs:fff
Installing: nixpkgs:fff
Installed: nixpkgs:fff
Commands: fff
Already installed: nixpkgs:fff
Removing: nixpkgs:fff
Removed: nixpkgs:fff
```

The installation took 21.594 seconds. The duplicate check took 0.846 seconds.
Removal took 0.235 seconds. This change improves progress and results; it does
not remove the cost of native evaluation or package realization. The user's
profile target and manifest checksum were unchanged throughout verification.

## Required checks

- Build, format, Clippy with denied warnings, and documentation with denied warnings passed.
- The full Rust suite passed: 262 tests, with no failures.
- Installer syntax and ShellCheck passed.
- Installer tests passed: 44 tests, with seven platform skips on macOS.
- Documentation links and OpenSpec structure passed.

Local detailed evidence is retained under the ignored `dist/` directory in
the `package-command-*` logs and JSON records. This record precedes release CI.
