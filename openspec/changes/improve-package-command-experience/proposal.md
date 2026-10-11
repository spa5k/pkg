# Improve package command output

## Problem

`pkg uninstall` and `pkg add` are not accepted. Installation can wait without
visible progress. Its result shows a short name without its source or command.
Removal exposes native chatter and an ignored permission error.

## Decision

Accept `add` as a visible alias of `install`. Accept `uninstall` as a visible
alias of `remove`. Both aliases use the same handlers, options, and completions.

Show resolution before installation work. Use qualified package IDs in the
install, remove, and upgrade stages and results. After installation, show up to
ten executable names verified in the selected package outputs and profile.
Do not infer a command name or installed version from a package name.

Show elapsed work in an interactive terminal during native mutations, including
quiet waits. Keep warnings and trust prompts visible. Use the filtered native
reporter for removal. Keep ignored errors and native removal chatter under
`--verbose`. Preserve native failure causes and cancellation behavior.

The native profile remains the source of package state. Command discovery reads
local files. It adds no evaluation or network request after installation.

This extends [package states and discovery](../improve-cli-discovery/proposal.md).

## Verification

Check aliases, help, and completion. Replay ignored errors and fatal removal
errors through the CLI. Check command discovery against real files. Run the
native lifecycle and cancellation checks in disposable profiles. Run the
required repository checks.

Recorded results: [package command verification](../../../docs/verification/2026-10-12-package-command-experience.md).
