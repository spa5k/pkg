# Completed alpha work

Historical delivery record, reconciled on 26 September 2026.
This file is evidence, not an implementation backlog.
The [active plan](determinate-nix-stacked-prs.md) owns remaining work.

## Determinate migration

| Work | Result and evidence |
| --- | --- |
| DN-00 through DN-07 | Plan, cancellation and lease fixes, vendor contract, authenticated assets, invocation, and handoff foundation completed |
| DN-08 through DN-14 | Inactive platform integration, ownership decisions, detection, PATH, and terminal uninstall completed |
| DN-15 | Native x86-64 Linux proof passed at `9bd17b716503d7be3bcf5bd310ceddd9aecede50`; [run 33198985687](https://github.com/spa5k/pkg/actions/runs/33198985687) |
| DN-16 | Two Apple Silicon slots passed at product `cbd3494`, sealed pair `baae3f8b`; [run 33329100389](https://github.com/spa5k/pkg/actions/runs/33329100389); merged at `1a3905d` |
| DN-17 through DN-19 | Cutover cleanup merged with the delivery. Remaining managed modules must be assessed by live use, not deleted by their names |
| Release proof | Later public alpha releases added exact-asset and native lifecycle evidence; this does not activate production trust |

The old statements that macOS code was unmerged, runners did not exist, or
DN-16 had never passed were stale. Later repeat-proof failures do not erase the
historical proof. They remain a separate assurance task.

## Public alpha improvements

| Work | Delivered result |
| --- | --- |
| PUBLIC-02 | Automatic verified product service updates; explicit offline mode retained |
| PUBLIC-03 and PUBLIC-04 | Setup language, shellenv, native package/build/repair/reboot/removal evidence; [report](../tests/macos-clean-host/PUBLIC-04.md) |
| PUBLIC-05 | Broad pinned Nixpkgs catalog and name-only packages; [report](../tests/macos-clean-host/PUBLIC-05.md) |
| PUBLIC-06 and PUBLIC-07 | Unprivileged source evaluation and macOS public flakes; [PR #38](https://github.com/spa5k/pkg/pull/38), [PR #39](https://github.com/spa5k/pkg/pull/39) |
| PUBLIC-08 | Alpha.49 installation and daily-command improvements; [PR #42](https://github.com/spa5k/pkg/pull/42), [PR #43](https://github.com/spa5k/pkg/pull/43) |
| PUBLIC-09 | Terminal output and progress; [PR #44](https://github.com/spa5k/pkg/pull/44) |
| PUBLIC-10 | One verified public installer; [PR #45](https://github.com/spa5k/pkg/pull/45) |
| PUBLIC-11 | CI summaries, exact-release checks, production-signing helpers, initial runbook; [PR #46](https://github.com/spa5k/pkg/pull/46). Production activation remains open |
| PUBLIC-12 and PUBLIC-13 | Command help, layout, useful results and previews; [PR #50](https://github.com/spa5k/pkg/pull/50), [PR #51](https://github.com/spa5k/pkg/pull/51) |
| PUBLIC-14 and PUBLIC-15 | Alpha.52 publication and installation guide; superseded by alpha.54 |
| PUBLIC-16 | Installer authentication diagnostics; [PR #54](https://github.com/spa5k/pkg/pull/54) |
| PUBLIC-17 | Installed-name resolution, package/system uninstall separation, history and rollback details; [PR #55](https://github.com/spa5k/pkg/pull/55) |
| PUBLIC-18 | Alpha.53 candidate prepared but withheld after native removal failed; never a public release |
| PUBLIC-20 | System-removal cleanup and partial-removal recovery; [PR #58](https://github.com/spa5k/pkg/pull/58) |
| PUBLIC-21 and PUBLIC-22 | Alpha.54 published and current installation instructions; [release](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.54), [PR #60](https://github.com/spa5k/pkg/pull/60) |
| PUBLIC-23 | Bounded doctor startup checks and reliable lock regression; [PR #61](https://github.com/spa5k/pkg/pull/61), merged at `4ab2121`. Not released |

The alpha.54 [fresh public checks](https://github.com/spa5k/pkg/actions/runs/36135260608)
passed on Linux and macOS. Its release notes identify exact native upgrade,
reboot, removal, and reinstall evidence and their limits.
Delivered features can still have defects. The later
[daily-use audit](../tests/macos-clean-host/BUG-HUNT-2026-09-26.md) records open
concurrency, recovery, repair, and CLI failures. Those remain in QUALITY-01
and issue #4; they do not reopen the superseded feature proposals below.

## Superseded proposals

The `improve-cli-ux` and `improve-install-ux` OpenSpec proposals were superseded
by PUBLIC-09 through PUBLIC-17 and PUBLIC-20. Their unchecked task lists were
not a reliable backlog and have been removed. Git history retains the originals.
Do not revive their proposed rename, replacement exit codes, unrestricted
channel flags, separate uninstall script, or duplicate installer library as
unfinished requirements. Current public contracts are in
[commands](../docs/commands.md), [installation](../docs/install.md), and
[CLI experience](../docs/cli-experience.md).

The original custom private-Nix design remains in the
[legacy archive](archive/2026-08-22-custom-managed-nix-v1/README.md).
Optional simplification proposals are not automatically complete; future work
needs current evidence and a scoped entry in the active plan.
