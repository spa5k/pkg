---
title: Privacy and security
---

# Privacy and security

- Telemetry is disabled by default. The client has no telemetry transmitter.
- `pkg doctor` reads local state and prints a summary. It uploads nothing.
- `pkg` keeps disposable discovery data under `~/.cache/pkg/`. You can delete
  it at any time. Deleting it does not affect installed packages.
- `pkg` accepts package names and validated public flake references. It does
  not run arbitrary Nix expressions from packages and does not accept
  source-provided Nix settings, substituters, or signing keys.
- Package payloads come from their vendor sources through Nix, with native
  store checks and download hashes. A hash is not publisher identity or
  approval.
