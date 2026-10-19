# Speed up tap imports

## Why

Tap import cost was concentrated in sandbox export startup, but changing it risked omitting upstream Ruby runtime state.

## What Changes

- Profile current-branch and pinned public tap workflows end to end.
- Compare standalone and direct export entries with complete upstream runtime state.
- Select no startup change after captures, runtime controls, and public package behavior fail to justify one.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

Tap import behavior remains unchanged. Public tap timings remain mixed and retain the slower samples.
