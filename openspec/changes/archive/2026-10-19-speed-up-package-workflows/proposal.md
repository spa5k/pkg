# Speed up package workflows

## Why

Ordinary package commands paid repeated Nix startup, profile decoding, installer decoding, and repeated-install checking costs.

## What Changes

- Remove ordinary version probes; keep explicit doctor probes.
- Reuse one checked profile observation under the profile lock.
- Decode the verified installer archive once and prefer the measured faster Mac checksum command.
- Share bounded live-output checks for repeated plain default-output installs.
- Use larger bounded Formula metadata groups.
- Preserve source identity, locking, cancellation, postflight reads, and exact result text.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

Setup, updates, installs, upgrades, rollback, removal, listing, and installer preparation become faster without changing package behavior.
