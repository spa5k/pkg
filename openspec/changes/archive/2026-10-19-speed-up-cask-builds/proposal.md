# Speed up Cask archive builds

## Why

Cask installation time was dominated by archive extraction and bundle staging, but the exact phase costs were unknown.

## What Changes

- Profile the pinned Calibre archive build by phase.
- Preserve prewrite validation, member and symlink rules, signatures, and complete output audits.
- Select no unpacking change because extraction dominates and measured alternatives did not justify a change.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

No production archive behavior changes. The measurements and rejection remain in the consolidated speed record.
