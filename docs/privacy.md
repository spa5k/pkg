---
title: Privacy and security
---

# Privacy and security

- Telemetry is disabled by default. The current implementation has no telemetry transmitter.
- Local logs use an allowlisted schema, private permissions, bounded rotation, and secret redaction.
- Crash records exclude panic payloads, arguments, environment values, backtraces, and memory dumps.
- `pkg doctor --support` is explicit and preview-only. Nothing is uploaded.
- Package management goes through the product broker and closed helper protocol. Users do not get
  raw access to the managed Nix CLI, daemon, store controls, or trust configuration.

New support-bundle fields require privacy review and redaction tests before release.
