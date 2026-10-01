# Repair faults found through real package use

## Problem

Real use of alpha.10 found lost concurrent profile changes, private cask
file collisions, duplicate installs, unsafe conflict instructions, missing
AppImage fonts, and incomplete macOS daemon reloads. Cold discovery also
used about 1.69 GiB of Nix evaluator memory on a 2 GB Linux host.

See [the reproduction record](../../../docs/verification/2026-10-01-realistic-use.md).

## Decision

Keep the native Nix profile authoritative. Serialize cooperating pkg
mutations. Repair cask runtime layout and setup. Build local discovery
data in bounded metadata batches and store it in SQLite. `pkg update`
prepares the cache before search. Retain native rollback selection and
explain its destination.

This change replaces the full native-map implementation of
[search amortization](../amortize-search-with-revision-snapshots/proposal.md).
It does not add a service, package state engine, hosted index, or Nix bundle.

## Verification

Use real installed clients in 2 GB Linux E2B sandboxes and an Apple silicon
macOS VM. Record exact commands, revisions, resource measurements, and
application workflows. Existing findings remain historical evidence.
