# Installer lane summary — authorized test cleanup

Date: 2026-09-27. Configured model (inspected, not substituted): `PI_PROVIDER=zai`, `PI_MODEL=glm-5.3-flash`.
Owned files changed (only these two): `tools/install/test_render.py`, `tools/release/test_install_script.py`.
Originals preserved at `/tmp/pkg-cleanup/installer-before/`. No product source, CI, dependency, or plan edits. Not committed.
Isolated mutation probes lived in `/tmp/pkg-cleanup/probe/` (repo source restored byte-for-byte; `cmp` verified).

## Commands and outcomes

```
python3 tools/install/test_render.py -v
  -> Ran 5 tests ... OK  (RenderedReleaseProof: 3; RealShellBoundaryTests: 2)
python3 tools/release/test_install_script.py -v
  -> Ran 5 tests ... OK  (all 5 bootstrap contracts)
```

Isolated-copy mutation controls (unchanged tests vs mutated `render.py` copy; probes ran under
`/tmp/pkg-cleanup/probe` with its own `docs/` and `packaging/` copies):

| Mutant (copy only) | Test run | Result |
| --- | --- | --- |
| none (byte-identical control) | `RenderedReleaseProof` | OK |
| all SHA-256 replaced with 64 zeroes | `test_rendered_script_carries_independent_digests` | FAILED (failures=1) — caught |
| `path.is_symlink()` removed | `test_each_unsafe_artifact_refuses_at_the_cli` | FAILED (failures=3) — every symlink subcase caught |
| tag guard replaced with `if False:` | `test_invalid_tags_fail_at_the_tag_guard` | FAILED (failures=7) — all 7 unsafe-tag subtests caught |
| restored control | `cmp` + rerun | identical, OK |

Before the fix, the audit had already shown the old tests stayed green under the digest,
symlink, and tag mutants; the new runs above show the replacements catch them.

## Removed weak checks -> keepers

- Removed: in-process `test_missing_or_linked_asset_refuses` (Linux symlink only, could pass at
  the wrong guard, no macOS artifacts). Keeper: `test_each_unsafe_artifact_refuses_at_the_cli` —
  real `render.py` subprocess, complete fixtures, 3 artifacts x {missing, symlink, empty,
  directory} while the other two stay valid; asserts nonzero exit, `missing or unsafe release
  artifact` on stderr, and no output file.
- Removed: in-process `test_invalid_tag_cannot_inject_shell` (ran with `Path(".")`, so mutants
  failed at the missing-artifact guard instead of the tag guard). Keeper:
  `test_invalid_tags_fail_at_the_tag_guard` — real CLI with complete fixtures; unsafe tags
  (`v1.0.0'; echo injected`, `$(id)`, `` v1.0.0`id` ``, `../v1.0.0`, `v1.0.0\nrm -rf /`,
  `1.0.0`, `v1.0.0-alpha.0`) must fail with `invalid release tag` on stderr and no output file.
  Tag-derived artifact names that a disabled guard would need are created safely as plain path
  components for the shell-metachar controls (asserted present), so missing-artifact masking
  cannot pass; names with `/` are skipped because a path component cannot carry them. Unsafe
  tags are never executed; no output script exists to run. `v01.0.0` is not asserted — the
  grammar accepts it and no product requirement rejects it.
- Removed: `test_render_contains_exact_bytes_for_both_platforms` (identical `b"bytes"` for all
  three artifacts; only text-presence checks; audit showed it survived zeroed digests). Keeper:
  `test_rendered_script_carries_independent_digests` — real CLI, three pairwise-distinct
  artifact payloads, digests computed independently with `hashlib.sha256` in the test process
  and required to appear in the rendered output; distinctness asserted; no `@PKG_...@`
  placeholders; tag, base URL, and artifact name present.
- Replaced: `tools/release/test_install_script.py` setUp hand-filled
  `@PKG_SHA256_*@`/`@PKG_RELEASE@` placeholders. Now setUp invokes the real
  `tools/install/render.py` CLI over the three distinct fixture artifacts, verifies the rendered
  digests independently, and substitutes only host boundaries (`[ -d /run/systemd/system ]` ->
  `$PKG_TEST_SYSTEMD`, `pkg_cli=/usr/local/bin/pkg` -> `$PKG_TEST_CLI`). Fake `curl` now serves
  the actual fixture artifacts per URL basename, so the shell harness executes the true
  rendered bootstrap and downloads bytes that the renderer hashed.

## Preserved contracts

- All 5 bootstrap security/status tests in `test_install_script.py`:
  verify-only without elevation on both platforms; real exit status 17 + private 0600
  `pkg-install-log.*` + no `pkg-install.*` leftovers; tampered package/wrapper (and now Linux
  artifact) -> Checksum mismatch before sudo marker; success requires healthy installed
  version; unsupported host and network failure never execute.
- Real PATH/startup-shell tests in `test_render.py`: `test_doctor_path_is_idempotent` and
  `test_guarded_startup_survives_removed_binary` (bash/zsh, removed-binary case) unchanged.

## Limitations

- E2B-less Linux host run: proves the render CLI boundary, digest pinning, POSIX sh download/
  checksum/privilege logic with doubles for curl/sudo/uname/systemd/id/pkg, and real bash/zsh
  startup shells. It does not prove macOS launchd, PackageKit, Gatekeeper, real GitHub
  downloads, or a clean-host privileged install.
- Darwin flow uses a fake wrapper; the real `packaging/macos/install-preview.sh` is only
  exercised by the retained startup-shell text/shell checks.
- The 12 refusal cases and 7 tag cases are the selected adversarial set, not exhaustive fuzzing.
- The macOS package fixture bytes are never executed (by design; the fake wrapper ignores them).
