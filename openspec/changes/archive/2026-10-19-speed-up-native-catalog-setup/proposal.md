# Speed up native catalog setup

## Why

Native catalog setup issued many small metadata requests and took several minutes on both supported hosts.

## What Changes

- Increase the bounded native metadata request group to 512 paths.
- Keep the encoded-argument limit, timeout, retry, splitting, and complete-result validation.
- Preserve atomic publication and exact native catalog rows.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

Setup completes faster without changing catalog contents or failure behavior.
