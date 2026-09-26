# Alpha.56 retained evidence

See the [release report](../../ALPHA-56-2026-09-26.md) for scope, source, hashes,
platforms, results, and remaining limits. Files here retain observed output.
An earlier failure is not rewritten after a later successful check.

## Native VM

- [68-check lifecycle table](native-proof/summary.md), [checks](native-proof/checks.json),
  [final checkpoint](native-proof/checkpoint.json), and
  [checkpoint before reboot](native-proof/checkpoint-before-reboot.json).
- [Intermediate alpha.55 baseline](native-proof/intermediate-baseline.json) and
  [saved alpha.55 checkpoint](native-proof/alpha55-checkpoint-before56.json).
- [Upgrade](upgrade.log), [real reboot](reboot.log), and [resume](resume.log).
- [UX commands, exits, and timings](native-ux/commands.json).
  Each case has separate `.stdout` and `.stderr` files in that directory.
- [Uninstall](uninstall.log), [exit status](uninstall-exit.txt),
  [post-removal checks](native-proof/removal-checks.json).
- [Fresh shells before removal](native-proof/shell-before.json) and
  [fresh shells after removal](native-proof/shell-after.json).
- [Public reinstall result](public-vm/result.json),
  [public reinstall checks](public-vm/checks.json), and [transcript](public-vm.log).
- [Public just build](public-vm-just.log), [exit](public-vm-just-exit.txt), and
  [execution/verification/health](public-vm-extra/summary.md). The initial
  [noninteractive SSH health command](public-vm-just-health.log) had no configured
  package PATH and correctly reported it. The subsequent checks use the documented
  shell environment and pass.

The retained scripts describe the specific disposable-VM proof. They are not
additional public commands. The [public reinstall script](public_vm.py) removes
GitHub tokens from its child environment and uses the saved public release
installer. Product writes occur only in the selected VirtualMac guest.

## Signed and public files

- [Signed payload inventory](verified-assets.json) and [verification](verify-assets.log).
- [Sealed file inventory](channel-files.json) and [channel verification](channel-verification.json).
- [Downloaded public file comparison](public-verification.json) and
  [independent public signature verification](public-channel-crypto.json).
- [Original draft inventory](draft-inventory.json), before the proof-document upload.
- [Independent artifact review](artifact-review/review.log) and
  [macOS package inspection](artifact-review/package-inspection.json).

Release binaries and private signing or SSH keys are excluded. The release
retains the signed payloads, bundles, sealed channel, and release card separately.

## Source and public CI

- [Source gates](source-checks.json), [source fast CI](source-fast-ci.json),
  [native Linux source CI](source-native-ci.json), and [signed build CI](signed-build-ci.json).
- [First hosted attempt](public-ci-first/release-check-macos-15/summary.md),
  [second attempt](public-ci-second/release-check-macos-15/summary.md), and
  [captured refusal stage](public-ci-diagnostic/release-check-macos-15/broker-diagnostic.log).
- [Public platform CI](public-check-ci.json). Hosted checkpoints stop before
  reboot; only the VM resume record proves a reboot.
- [Later metadata/resource diagnostic run](public-ci-evaluator/release-check-macos-15/summary.md),
  [sandboxed metadata result](public-ci-evaluator/release-check-macos-15/source-metadata-probe.json),
  and [anonymous GitHub status](public-ci-evaluator/release-check-macos-15/public-source-service.json).
  The fixed metadata probe source is retained alongside these records.
- [Completed public VM package lifecycle](public-vm-finish/summary.md).
- [Source checks](checks.log), [macOS quality](quality-macos-ci.log),
  and [Linux quality](quality-linux-ci.log).
- [First source macOS hermetic failure](hermetic-job.log) and
  [documentation-branch failure](docs-hermetic-job.log).
- [Native lock-inheritance probe](review-spec-lock-probes/README.md),
  [probe results](review-spec-lock-probes/results.json), and
  [30 CLI suite repetitions](cli-repeat.log).

The lock probe is diagnosis, not a change to production lock behavior.
VERIFY-01 retains the test-isolation follow-up. Raw CI logs include runner
setup and diagnostic output; those paths refer to the original disposable host.
