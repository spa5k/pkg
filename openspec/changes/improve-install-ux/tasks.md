# Tasks: Installation experience

## 1. Shared script library

- [ ] 1.1 `tools/install/lib.sh`: platform/arch detection, TTY detection, digest helpers,
      plain-message helpers (what happened + next action), channel fetch of verified manifest
- [ ] 1.2 ShellCheck clean, `set -euo pipefail` discipline, POSIX sh portability (dash, bash,
      zsh sh-mode)
- [ ] 1.3 Golden transcript tests for each refusal path (unsupported OS, missing disk, foreign
      nix, already installed)

## 2. install.sh

- [ ] 2.1 Entry flow: preflight → fetch verified manifest → verify artifacts → invoke platform
      installer → run doctor → exit by verdict
- [ ] 2.2 Preflight wiring through `doctor --preflight` where available, local checks for
      TTY/disk
- [ ] 2.3 Checksum-gated download with delete-on-mismatch and digest printing
- [ ] 2.4 `--channel <url>`, `--yes`, `--dry-run` flags
- [ ] 2.5 End-to-end test in the staged Linux host: clean install via script, doctor verdict
      pass, terminal uninstall via script

## 3. uninstall.sh

- [ ] 3.2 Install-state detection, ownership-receipt-scoped removal via existing product
      uninstall path, confirmation discipline (interactive ask, `--yes`, non-interactive
      refusal)
- [ ] 3.3 Post-uninstall absence check (the same contract the proof harness verifies) and
      residue report

## 4. Installer messaging

- [ ] 4.1 Phase reporting hooks in macOS `.pkg` and Linux installers writing the phase line to
      the invoking console
- [ ] 4.2 Failure message composer: journal phase + on-disk state + recovery-matrix next
      action (reuse the fault-injection matrix as the source of truth)
- [ ] 4.3 Completion panel: verify / doctor / uninstall commands with the installed version

## 5. Channel packaging

- [ ] 5.1 Add both scripts + `lib.sh` as TUF targets in the release workflow
- [ ] 5.2 Stable redirect `https://kelv.dev/install.sh` → current channel copy (hosting lands
      with the trust ceremony; wire the path now, document the placeholder)
- [ ] 5.3 Docs: install guide page with the supported matrix, flags, and verification
      explanation

## 6. Wrap-up

- [ ] 6.1 Full workspace tests, clippy, fmt, quality gate; rebase baseline if lint sites moved
- [ ] 6.2 One staged-host proof run exercising the full script lifecycle
