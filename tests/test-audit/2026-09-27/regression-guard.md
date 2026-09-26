# Regression guard follow-up

Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`.
Plan: [VERIFY-01](../../../plans/determinate-nix-stacked-prs.md#verify-01-repeatable-assurance).

This PR contains the reviewed seven-file test cleanup, its audit evidence,
the repository-specific test-audit skill, and a mandatory Fast-CI behavior
guard. Earlier plan and spike deletions are excluded. The historical audit
and cleanup records describe their original checkpoints.

The [guard procedure](../../../tools/verify/TEST-CONTRACTS.md) describes how to
run the eleven known defects. Each defect needs a healthy baseline, a test
failure caused by the defect, and a healthy restored control. The checkout is
never mutated. Source drift requires an explicit mutation update. CI retains
the complete logs and machine-readable results.

Eight subprocess integration tests check the guard itself. They cover real
assertion failure and restoration, weak assertions, missing/skipped tests,
direct and child import errors, child syntax errors, an abnormal exit after a
failing test summary, and stale mutation targets. Python owners are compiled
before mutation tests run. This catches syntax failures even when a child
process error would otherwise become an assertion failure in its parent test.

Independent A and E preservation reviews found two false-green paths in the
first guard draft: timeout status after a failing summary, and child syntax or
import failure hidden inside an assertion. Both paths were fixed and covered
by real subprocess controls. Both reviewers found no remaining issue in their
reviewed scope. These are source reviews, not native lifecycle proof.

The local G-LINT gate passed on Rust 1.96.1: formatting, Clippy, rustdoc, build,
dependency policy, and vulnerability audit. The eight guard integration tests,
workflow validation, documentation links, and static ambient-time/temp-root
audits passed. The static audit did not run network isolation.

All eleven mutations, baselines, and restored controls passed locally. The six
Python-driven cases were repeated after the final child-import classifier fix;
all passed. The quality ratchet passed with 922 existing debt sites. Raw audit
logs, patches, and model responses retain their original whitespace through a
directory-scoped Git attribute; production code and test source stay checked.

The cleanup evidence retains the broader Linux and macOS test runs. Native
installation, reboot, and real-Nix lifecycle validation are still separate
requirements. This bounded guard does not complete VERIFY-01 or prove every
future change safe. Reviewers must preserve or replace each contract when
changing the case registry or its tests.
