# Measure release compiler settings

## Why

Search CPU cost could plausibly fall with a different release compiler profile, but runtime and build cost had to be measured together.

## What Changes

- Compare default release settings with identical-source Thin LTO builds.
- Measure build time, executable size, commands, search, and native checks on both hosts.
- Keep the default profile because runtime results are mixed and build cost rises.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

No compiler configuration changes. The rejected trial and its limits remain in the consolidated speed record.
