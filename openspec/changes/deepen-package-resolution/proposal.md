# Deepen Package Resolution

Tap import cancellation currently loses the signal number. The command exits with status 1. Package inspection and installation also assemble the same resolution rules in separate modules.

Preserve cancellation through the import and command error types. Put Package Resolution under one owner. Remove duplicate production implementations and unnecessary intermediate data. Reduce production Rust code by at least 20% from revision `1c1164b`. Count neither tests nor fixtures. Use code lines and Rust tokens to detect formatting changes.

## Non-goals

Do not remove supported package operations. Do not restore the removed package state engine. Do not weaken tap validation, source isolation, or process cleanup.

## Scope decision: 2026-10-13

The user accepted the current result and withdrew the 20% reduction target.
The measured production Rust reduction is 9.93%. Current product capabilities
remain. No further reduction work is planned. The Rust compiler update is complete.

## Dependency update: 2026-10-13

Update direct Rust dependencies to their latest stable releases and refresh the
lockfile. Update CI action pins and the shared rolling Nixpkgs pin. Retain exact
versions and commit hashes. Regenerate Formula metadata for the new Nixpkgs
revision. Run the required checks on Linux and macOS, then create a PR with the
completed cancellation fix, Package Resolution refactor, and tool updates.
