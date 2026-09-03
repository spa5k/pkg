# Proposal: CLI user experience

## Why

The CLI works, but it was built for proofs, not for people. Help output is sparse, error
messages assume the reader wrote the code, progress is silent during long operations, and the
machine-readable mode is inconsistent across commands. The upcoming rename (`pkg` → `kelv`)
makes this the right moment to fix the UX once: every user-facing string will be centralized,
so the rename becomes a one-place change instead of a repo-wide hunt.

Concrete pain points observed today:

- Long operations (install, upgrade, repair) print nothing while the Determinate installer or
  the channel fetch runs; users cannot tell progress from a hang.
- Errors surface as deep enum variants with no suggested next action (for example a
  channel-verification failure does not say what to pin, refresh, or remove).
- There is no single "check my setup" command; diagnosis means reading logs under
  `/opt/pkg/var/` (path subject to rename) or re-running with env vars.
- Exit codes are undocumented; scripts cannot distinguish "already installed" from "failed".

## What Changes

1. A UX text registry: all user-visible strings (help, errors, progress, confirmations) live in
   one module with stable identifiers; nothing user-facing is inline.
2. Consistent help: every command gets usage, examples, and a link to docs; `--help` output is
   golden-tested so it cannot drift silently.
3. Actionable errors: every error printed to a human includes what happened, why (when known),
   and the next action; machine mode (`--json`) includes a stable error code per failure class.
4. Progress reporting: named phases with elapsed time for long operations; quiet in
   non-interactive contexts (no TTY, `CI` env, or `--quiet`); plain lines, no spinners.
5. Safe defaults with explicit confirmation for destructive operations (uninstall, repair);
   `--yes` for scripts; `--dry-run` where a plan already exists.
6. Documented exit codes: 0 success, 1 usage, 2 state conflict (already installed, absent), 3
   network/channel, 4 verification failure, 5 internal; documented in help and docs.
7. `doctor` command: one command that checks prerequisites, channel reachability, TUF root
   health, install state consistency, and prints a pass/fail table with the same exit codes.

## Non-goals

- No interactive TUI framework; plain lines only.
- No new product capabilities beyond `doctor` (which reuses existing checks from preflight).
- No rename execution in this change; this change only makes the rename cheap.

## Impact

- `crates/pkg-cli`: help text, error rendering, progress, exit codes, `doctor`.
- `crates/pkg-nix`, `crates/pkg-installer`: stable error codes for failure classes.
- Docs: new `docs/cli.md` exit-code and JSON schema reference.
