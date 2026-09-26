# Raw GLM review — verify against the main audit report

This is the model's original response. Some conclusions were corrected by the
parent's source checks and mutation experiments. See `review-corrections.md`.

# Test Audit — `pkg` renderer and installer

**Scope.** Base commit `e96a7d06b01e0205e82f51f112a8951ae9324830`. The working tree has plan-cleanup edits only (docs and plans). The five audited files match HEAD. `tools/release/mint_dn1_proof_pair.sh` is dirty, but it is not in this audit. I read `AGENTS.md`, `tools/install/render.py` (40 lines), `tools/install/test_render.py` (75 lines), `docs/install.sh` (140 lines), `tools/release/test_install_script.py` (109 lines) in full. I used git log for the three file groups. Model: GLM 5.3 Flash. Tool calls: 8 of 8. No files changed. No native install ran.

**Git history.** `render.py` and `test_render.py`: `0083dac` (one verified installer), `03c4e3a`, `23a2de2`. `docs/install.sh`: `f1b0c4d` → `0083dac` → `2ef90d2` → `03c4e3a`. `test_install_script.py`: `f1b0c4d` → `0083dac`. The pair render.py/test_render.py grew together with the installer.

**R/F/C/D key.** R = inputs reach the intended guard. F = real failure the test catches. C = coverage width. D = deficit.

---

## The five tests in `tools/install/test_render.py`

### 1. `test_missing_or_linked_asset_refuses`
Guard: `path.is_symlink() or not path.is_file() or path.stat().st_size == 0` in `render()`, per artifact.
- **R:** The missing-file case reaches the guard and raises `ValueError` at the first artifact. The symlink case does not discriminate. The fixture holds only `pkg-install-x86_64-linux`; the macOS package and wrapper are absent. So the symlink check can vanish and a `ValueError` still fires, for the missing second artifact. The assertion sees "a ValueError", not the reason. Evidence confirms this: mutant suite exit 0 (see results section).
- **F:** Catches a fully missing artifact. The mutant "symlink refusal removed" survives.
- **C:** Two calls, one artifact name, no zero-size file, no directory-as-file case.
- **D:** No message assertion. One of three artifact names exercised. Zero-size branch never tested.
- **Owner/keeper:** Sole owner of the artifact-safety guard. Keeper, but fix: complete fixture, symlink one artifact per case, assert `str(cm.exception)` contains the artifact name.
- **Evidence limits:** The `assertRaises` check is reason-blind. That is the whole gap.

### 2. `test_invalid_tag_cannot_inject_shell`
Guard: `re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[1-9][0-9]*)?", tag)` at the top of `render()`.
- **R:** Full. All four bad tags fail the regex at the first statement. No tag text reaches `install.sh` or the macOS file name when invalid.
- **F:** Catches shell metacharacters, a quote, path traversal, a newline. A loosened regex of these classes dies here.
- **C:** Four reject inputs. The accept path is covered by test 3 with `v0.1.0-alpha.49`. Boundaries not tested: `v1.0`, `V1.0.0`, `v01.0.0`, `v1.0.0-alpha.0` (the regex bans a leading zero in the prerelease; no test pins that).
- **D:** No assertion on the message. Boundary tags absent.
- **Owner/keeper:** Sole owner of the tag guard. Keeper. Add boundary rejects.
- **Evidence limits:** A mutant that only relaxes the prerelease zero rule passes today.

### 3. `test_render_contains_exact_bytes_for_both_platforms`
Guards: digest computation per artifact, and the final `@PKG_[A-Z0-9_]+@` unfilled-template check.
- **R:** The template guard is reached fully; any missed key leaves a placeholder and raises. The digest guard is reached but never judged. The test renders three fixture files of content `b"bytes"` and checks only: no leftover placeholder, and the constant strings `Darwin:arm64` and `Linux:x86_64`. It never compares any `PKG_SHA256_*` value to an independent hash of the fixture bytes. The name says "exact bytes"; the test does not check exact bytes.
- **F:** Catches an unfilled template and missing artifacts. The mutant "write 64 zeroes as the digest" survives, and the evidence shows exit 0.
- **C:** The two platform strings are static text in `docs/install.sh`; asserting them only catches an edit to the case labels. It does not assert `PKG_RELEASE`, the base URL, or the HTTPS line.
- **D:** No independent hash check. This is the gap the mutation probe proves.
- **Owner/keeper:** This test should own digest verification. Fix it: compute `hashlib.sha256` of each fixture and assert each `PKG_SHA256_X86_64_LINUX='…'`-style line. Also assert `PKG_RELEASE='v0.1.0-alpha.49'` and the https base URL.
- **Evidence limits:** Fixtures are complete in name and non-empty, so they pass the size guard, but their arbitrary content makes rendered digests unverifiable by eye or by test.

### 4. `test_doctor_path_is_idempotent`
Owner: the `pkg_check_path` dedup block in `docs/install.sh`.
- **R:** Full. The test slices the real block out of `docs/install.sh`, appends a printf, and runs it with real `/bin/sh` under three PATH shapes: missing both entries, managed dir first, `/usr/local/bin` already present. The `pkg_user_bin` value arrives as an environment variable, which matches the sliced block's free variable. No mocks.
- **F:** Catches a double-add mutant, a removed dedup guard, and a non-fixed-point result. Counts must be exactly 1, and a second run must return the same PATH.
- **C:** Good for the sliced block. It does not cover the sliced-out Darwin/Linux `pkg_user_bin` assignment, insertion order, or a PATH with an empty component.
- **D:** Brittle slicing by `source.index(...)` markers. A rename fails loudly with `ValueError`, which is acceptable.
- **Owner/keeper:** Keeper. This is a real-behavior test of the kind the user wants.
- **Evidence limits:** POSIX `/bin/sh` only; that matches the `#!/bin/sh` shebang, so the limit is fine.

### 5. `test_guarded_startup_survives_removed_binary`
Owner: the printed shell-setup line in `docs/install.sh` and its copy in `packaging/macos/install-preview.sh`.
- **R:** Real. The test extracts the single-quoted `[ ! -x /usr/local/bin/pkg ] || eval …` line from the source, repoints it with `shlex.quote`, and runs it under real `/bin/bash` and `/bin/zsh`. With the binary present, the eval must set `PKG_SHELL_TEST=ready`. With the binary removed, the line must exit 0 with empty stderr.
- **F:** Catches an inverted test (`[ -x ]`) and a removed guard. Both break the two-phase check.
- **C:** The wrapper checks are text-in-file assertions only. They prove the line is duplicated in the macOS wrapper, not that the wrapper's echo path runs.
- **D:** Duplication drift risk: install.sh and the wrapper hold two copies of the line; only one copy is executed here. Shells not present are skipped with no record.
- **Owner/keeper:** Keeper for the execution part. The wrapper text checks are weak but cheap.
- **Evidence limits:** Half the shell coverage disappears silently on hosts without `/bin/zsh`.

---

## Independent assessment of the mutation evidence

File: `/tmp/pkg-audit/mutations/results.json`, entries 1 and 3. I checked both against the source myself; I did not trust the JSON.

**`renderer-wrong-sha256` (64 zero digests).** Credible. `render()` computes real digests with `hashlib.file_digest`, but no test in `test_render.py` compares any rendered digest to an independent hash. Test 3 renders `b"bytes"` fixtures and asserts placeholders and platform strings only. So the mutant passes with exit 0, as recorded. The recorded control says the restored renderer emits the correct three digests. So this is a **test weakness, not a current product bug**. Note the downstream effect if the mutant ever shipped: `docs/install.sh`'s `pkg_digest` checks format only, and 64 zeroes are valid hex of length 64. Every user install would then fail closed at `shasum --check` with "Checksum mismatch". An availability failure, not a trust failure.

**`renderer-symlink-accepted`.** Credible, and the recorded exit 0 is correct behavior for this suite. With `path.is_symlink()` removed, `Path.is_file()` follows the symlink and returns true, so render proceeds. In the test's incomplete fixture, the second artifact is missing, so `render()` still raises `ValueError` for that reason and `assertRaises` passes. The recorded complete-fixture control matches Python semantics: original refuses, mutant accepts. Again a **test weakness, not a product bug**; the restored source refuses symlinks.

The other three entries (`skipped-linux-proof`, `private-result-leak`, `doctor-missing-checks`) are outside this audit's scope. Their pattern supports the same conclusion: fake or narrow suites stay green while a real keeper test catches the mutant.

**Fixture completeness summary.** Test 1's fixture is incomplete (one of three artifacts), and that incompleteness is what masks the symlink guard. Test 3's fixture is complete but content-blind, so digests are unchecked. Hashes are checked independently only in `tools/release/test_install_script.py`: it pins real `sha256` values of its payload and wrapper, and its tamper test makes the real `shasum --check` fail with "Checksum mismatch". That suite guards `docs/install.sh`, not `render.py`. The renderer has no independent hash check anywhere.

---

## First small replacement batch

1. **Test 3:** add independent digest assertions. For each key, compute `hashlib.sha256(fixture_bytes).hexdigest()` and assert the rendered `PKG_SHA256_*='<digest>'` line. Add `PKG_RELEASE` and https URL lines.
2. **Test 1:** complete the fixture (all three names, `b"bytes"`, non-empty). Then, per case, symlink exactly one artifact and assert the message contains that artifact's name. Add a zero-size case.
3. **Test 2:** add rejects `v1.0`, `V1.0.0`, `v01.0.0`, `v1.0.0-alpha.0`.

Commands:

```sh
# after each edit
python3 -m unittest discover -s tools/install -p test_render.py -v

# re-run the two renderer probes by hand, expect nonzero now
# (apply mutation, run suite, then restore)
git -C . stash list >/dev/null  # no-op reminder: edit render.py, test, then:
git checkout -- tools/install/render.py

# real-behavior suite stays as the guard for install.sh (no root, no network)
python3 -m unittest discover -s tools/release -p test_install_script.py -v
```

Expected outcome after the batch: the zero-digest mutant fails in test 3, the symlink mutant fails in test 1 with the artifact name in the message. Total new code: about 20 lines in one file.

