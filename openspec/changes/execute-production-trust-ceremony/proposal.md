# Proposal: Activate production trust

Status: external inputs and final proof pending. Reconciled 26 September 2026.
Scope: TRUST-01 in the [active plan](../../../plans/determinate-nix-stacked-prs.md).

## Why

Alpha.54 is published with the alpha test root and ad-hoc macOS signatures.
Production signing helpers, an initial runbook, and exact-release checks exist.
Production key custody, provider identities, domain access, Apple identities,
and notarization access are not established by that tooling.

## What Changes

Supply and review the operator inputs. Activate offline root custody,
provider-backed online signing, an immutable HTTPS channel, Developer ID
Application and Installer signing, and notarization. Prove the final artifacts
on independent native hosts before a production announcement.

## Non-goals

No alpha-root promotion, product rename, arbitrary channel override, or new
package feature. This proposal does not authorize a release during the current
audit. The next alpha release is deferred separately.

## Impact

Follow [the production runbook](../../../docs/ops/production-release.md) and
[remaining tasks](tasks.md). Verify actual provider and domain access before
changing constants or publication workflows.
