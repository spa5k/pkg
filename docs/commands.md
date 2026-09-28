# Commands

`pkg` uses one native Nix profile per user. The profile lives at
`~/.local/state/nix/profiles/pkg`. Nix owns package state, generations,
rollback, and roots.

Global options: `--help`, `--version`, `--no-color`, `--verbose`.

| Command | Behavior |
| --- | --- |
| `pkg search QUERY` | Search supported sources. Shows source-qualified names and support status. |
| `pkg info ID` | Show source, attribute, metadata revision, and known limits. |
| `pkg install ID…` | Resolve all names, then run one native profile add. |
| `pkg list` | Read the dedicated profile. Shows native entry IDs and source identity. |
| `pkg remove ENTRY…` | Remove exact installed entries in one native operation. |
| `pkg update` | Refresh discovery data. Does not change installed packages. |
| `pkg upgrade ENTRY…` / `--all` | Follow each entry's original reference. Reports fixed references. |
| `pkg history` | Show native profile generations. |
| `pkg rollback [N]` | Use Nix's native previous or selected generation. Refreshes launchers. |
| `pkg prune --older-than Nd` | Remove only eligible non-current generations. Requires an explicit positive age. |
| `pkg doctor` | Read-only runtime, profile, and launcher status. Repairs nothing. |
| `pkg shellenv` | Print idempotent Bash/Zsh path settings. Does not edit shell files. |
| `pkg completion SHELL` | Generate completions for the current grammar. |
| `pkg apps sync` | Rebuild pkg-owned macOS launchers from the active profile. |

Removed commands: `pin`, `unpin`, `outdated`, `repair`, `gc`, and
`system uninstall`. Removed options: global dry-run, build approval,
`--keep-going`, and JSONL progress. Use normal CLI usage errors for them.

## Package IDs

- `nixpkgs:<attribute>` selects a Nixpkgs attribute.
- `cask:<token>` selects a supported Cask package.
- A bare name is valid only when it has one supported exact match.
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
only. It does not change package selection.
