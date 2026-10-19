# Speed up app workflows

## Why

Real app installs and launcher refreshes spent measurable time in repeated profile reads, helper checks, and staged publication.

## What Changes

- Reuse postflight profile entries under the profile lock.
- Check helper startup during staged sync instead of running a separate help probe.
- Repair only the exact known helper startup fault before promotion.
- Preserve signature checks, cancellation, atomic publication, and launcher ownership.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

App sync and helper repair paths improve while externally visible app behavior remains unchanged.
