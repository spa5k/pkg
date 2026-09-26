# Proposal: Complete deterministic verification

Status: partially delivered. Reconciled on 26 September 2026.
Scope: VERIFY-01 in the [active plan](../../../plans/determinate-nix-stacked-prs.md).

## Why

Hermetic tests, static clock/root audits, Linux clean-host CI, CLI process tests,
and a repeat-proof workflow already exist. Main CI passes. The repeat-proof
workflow still lacks a successful complete run in the inspected history.
Recent nightly cancellations also skipped fault and real-Nix jobs.

## What Changes

Complete the remaining repeat proof and scheduled assurance. Add reviewed
stable-state fixtures, a mutation-boundary inventory, parser fuzzing, and
lifecycle operation-sequence tests. Combine their evidence in a report only
after the individual checks work.

## Non-goals

No product rename, new exit-code contract, clock freezing, duplicate workflow,
or blanket backward-compatibility promise. No release is published by this work.

## Impact

Existing verification workflows, test fixtures, and `tools/verify`.
The [tasks](tasks.md) list the remaining acceptance work.
