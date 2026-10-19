# Speed up repeated installs

## Why

Repeating a large group of already installed packages re-evaluated each request independently and took tens of seconds.

## What Changes

- Share bounded source-resolution and output checks across plain native attribute groups.
- Keep the per-request path for complex attributes, uncertain comparisons, and native batch limits.
- Require exact source, active entry, attribute, output, and selected-path equality.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

Large repeated installs complete in under one second while profile rows and messages remain unchanged.
