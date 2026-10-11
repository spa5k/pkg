# Explain package file conflicts

## Problem

Installing a Homebrew Formula can fail because an installed Nixpkgs package
provides the same file. Normal output exposes store paths and native priority
commands. The final cause does not identify the installed package or explain
how to select the requested package with `pkg`.

## Decision

Keep native file conflict checks. Show the profile-relative file and the
installed and requested package IDs with their source labels.
State that this installed package prevents the new
installation. Omit replacement and priority commands from normal output.
Keep native diagnostics under `--verbose`.

This extends [package states and discovery](../improve-cli-discovery/proposal.md).

## Verification

Replay the native conflict diagnostic through the CLI. Check owner selection,
multiple owners, unknown profile state, filtered output, and verbose output.
Repeat the reported `bat` conflict with real Nix in a separate profile.
Run the required checks.

Recorded results: [package conflict verification](../../../docs/verification/2026-10-12-package-conflicts.md).
