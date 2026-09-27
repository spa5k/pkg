# Behavior test regression guard

Run from the repository root:

```sh
python3 -m unittest discover -s tools/verify -p 'test_check_test_contracts.py' -v
python3 tools/verify/check_test_contracts.py --output /tmp/pkg-test-contract-results
```

Use a new output directory. The guard copies the tracked tree and non-ignored
new files into a temporary directory. It never mutates the checkout. Cargo
output remains in the checkout's ignored `target` directory for reuse.

Each case in `contract_cases.py` names one real source defect and its existing
behavior test. The guard requires a passing baseline, an assertion failure
with the defect, and a passing restored control. The mutation target must match
exactly once. A changed target fails closed. Build/import failures, skipped or
missing tests, and timeouts do not count as caught defects.

The eleven cases cover renderer digests, symlink and tag refusal, doctor rows,
public-result privacy, trusted HOME boundaries, workflow manual gating, harness
entry, GC dry-run policy, and exactly-once dispatch. The
[audit and cleanup evidence](../../tests/test-audit/2026-09-27/cleanup.md)
explains the original gaps. New false-green findings need a meaningful case
here or equivalent mutation proof in their owning platform lane.

The guard's integration tests use actual Python source files and child test
processes. They verify that weak assertions, missing tests, skipped tests,
import failures, and stale mutation targets cannot produce a passing guard.

Fast CI runs this job on every pull request and main push. The aggregate result
requires its success. Logs and machine-readable results are retained as a CI
artifact. A reviewer must check any removal or weakening of cases against the
product contract; no local test can make its own deletion impossible.

This guard covers known regressions. It is not proof of full native installation,
network isolation, or all future defects. Keep native lifecycle and security
tests until a demonstrated replacement owns their contracts.
