# Decode standalone gzip payloads

## Why

A real Diversion payload contained a standalone gzip executable stream. The installer copied compressed bytes, so the installed command could not execute.

## What Changes

- Decode a standalone gzip stream before artifact installation.
- Keep declared nested paths, path validation, extraction limits, and prewrite safety checks.
- Leave compressed manpages and other declared compressed artifacts byte-for-byte unchanged.

## Capabilities

### New Capabilities

- `app-payloads`: Defines safe preparation of vendor app and executable payload archives.

### Modified Capabilities

- None.

## Impact

Standalone gzip executables install and run correctly. Existing archive formats keep their bytes and refusal rules.
