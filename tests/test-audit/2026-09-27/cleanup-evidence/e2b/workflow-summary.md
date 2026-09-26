# Workflow lane cleanup: tools/release/test_workflow.py

Owner file only: `tools/release/test_workflow.py`. No production, shell, or
workflow changes. Original saved at `/tmp/pkg-cleanup/workflow-before/test_workflow.py`.
No commit made.

## Deletion -> keeper mapping

| Removed declaration | Why removed | Keeper that owns the contract |
| --- | --- | --- |
| `test_workflow_never_loads_a_production_key_or_publishes`: README wording assert `"in-memory Ed25519 test keys"` | Docs wording; protects no behavior | None needed. `secrets.` / `contents: write` / `gh release` / `aws-actions` negative pins kept in the same test |
| `test_linux_uninstall_uses_plain_terminal_exec_status`: `assertNotIn("pkg-after-uninstall", ...)` | Obsolete name-only negative; protects no behavior | Exact uninstall lifecycle asserts in the same test (exit status 78, empty stderr, `cmp before after`) |
| `test_linux_uninstall_uses_plain_terminal_exec_status`: `assertNotIn("idempotent uninstall", ...)` | Same | Same keeper |
| `test_production_linux_input_is_manual_fixed_and_not_published`: global `assertIn("production-linux:")` and `assertIn("inputs.production-linux")` | Not deleted; replaced. Global string presence passed with an unconditional job (audited mutant) | New scoped condition check on the exact `production-linux` job block |

## Repairs (F) and additions

1. Repaired manual-only production guard
   (`test_production_linux_input_is_manual_fixed_and_not_published`):
   - New stdlib helpers `_single_block` and `_job_block`: scoped indentation
     extraction. They fail closed on missing or duplicate headers. PyYAML is
     not in this environment or CI, so no parser dependency was added.
   - Exact job condition: `github.event_name == 'workflow_dispatch' &&
     inputs.production-linux` must be the only `if:` on the `production-linux`
     job, inside its `jobs:` block.
   - Trigger input scoped inside `on: > workflow_dispatch: > inputs: >
     production-linux:`: exactly one `required: false`, `type: boolean`,
     `default: false`.
   - Permissions scoped: top-level `permissions:` block contains only
     `contents: read`; the production job adds no own `permissions:` block.
   - Restriction scoped: `dry-run-sign` and `linux-alpha-proof` jobs each have
     exactly `github.event_name != 'workflow_dispatch' || !inputs.production-linux`.
   - Kept inventory pins: pinned Rust URL, SHA-256 pin, artifact name,
     `pkg-release-index`, `runs-on: ubuntu-22.04` (now job-scoped).

2. New real-execution refusal class `LinuxCleanHostRefusalTests` (runs the real
   `tests/linux-clean-host/run.sh`; no `--self-check` mode added):
   - `test_invalid_arguments_refuse_with_exact_usage_and_no_side_effects`:
     `--keep-artifacts` (no value), `--keep-artifacts out extra`, `--bogus`.
     Asserts exact exit 2, exact `usage:` line on stderr, empty stdout, no
     sentinel call, no files created. 30 s subprocess timeout; a timeout fails
     the test instead of hanging.
   - `test_valid_arguments_stop_at_first_docker_attempt_without_git_or_artifacts`:
     valid `--keep-artifacts` form reaches only a fail-fast sentinel `docker`
     wrapper (exit 97 propagates), logs exactly one
     `docker version --format {{.Server.Arch}}` line, never calls `git`, and
     creates no artifact directory. PATH holds only the sentinels, so a mutant
     that removes validation can never start real Docker or system install.

## Retained (R)

All security pins, trust and permission guards, the macOS authentication
block, the uninstall lifecycle contract, the isolated-proof and ordering
checks, and the full 46-name blocker filter inventory. No mass deletion of
source checks.

## Tests and results

- `python3 -m unittest discover -s tools/release -p test_workflow.py -v`:
  10 tests, OK (was 8; net +2).
- Mutation controls (sources restored byte for byte, checked with `cmp`):
  - Early `exit 0` in `run.sh`: new refusal test FAILED (caught).
  - Production job `if: ${{ true }}`: repaired guard FAILED (caught).
  - Trigger input `default: true`: repaired guard FAILED (caught).
- Final suite after restore: 10 tests, OK.

## Limitations

- The refusal tests prove argument handling and fail-fast order only. They do
  not run the Docker lifecycle; native clean-host proof still needs the
  documented disposable hosts.
- The indentation extraction is scoped to the current fixed shapes of
  `release.yml` and fail closed on missing or duplicate headers. It is not a
  general YAML parser.
- The 46-blocker inventory still proves name presence, not execution. A
  replacement owner is still needed before that can shrink.
