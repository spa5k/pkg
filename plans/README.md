# Implementation plans

The active future implementation plan is
[Simplify pkg to native Nix](../openspec/changes/simplify-to-native-nix/proposal.md).
Read its [design](../openspec/changes/simplify-to-native-nix/design.md),
[ordered tasks](../openspec/changes/simplify-to-native-nix/tasks.md), or
[HTML view](../artifacts/pkg-simplification-plan.html).
It is based on origin/main at 5256cfa for alpha.58.
Planning is complete. Implementation is pending.

Breaking changes and removal of obsolete tests are allowed. There is no
backward-compatibility or data-migration requirement.
The [previous plan](determinate-nix-stacked-prs.md) remains a record of the
alpha design. Its verification and production trust backlog is superseded.

The [onboarding audit](../tests/macos-clean-host/ONBOARDING-2026-09-26.md)
and [daily-use audit](../tests/macos-clean-host/BUG-HUNT-2026-09-26.md)
retain the original findings. The nine bug fixes and six UX fixes are merged
and released in alpha.56, including the final macOS wrapper and Linux profile
corrections. [Fix evidence](../tests/macos-clean-host/BUG-FIXES-2026-09-26.md)
and [release proof](../tests/release-lifecycle/ALPHA-56-2026-09-26.md) record the
source, signed assets, native upgrade/reboot/removal, and fresh public checks.
Alpha.55 remains unpublished. Completed fixes are not additional open tasks.

[Completed alpha work](completed-alpha-work.md) records the finished migration
and public improvements. It is not an active backlog.

The [architecture report](../architecture-report.html) is historical evidence.
The [custom private-Nix archive](archive/2026-08-22-custom-managed-nix-v1/README.md)
is historical and is not normative. The native Nix OpenSpec change wins over
older proposals for future implementation.
