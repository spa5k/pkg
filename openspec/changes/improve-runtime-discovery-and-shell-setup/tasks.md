# Tasks: Improve runtime discovery and installer shell setup

## 1. NN-01: Runtime discovery

- [x] 1.1 Add the PATH-then-stable-profile discovery order with the
  executable-file candidate rule and the per-user profile path; verify
  selection of an executable candidate past non-executable files with a
  pure helper test.
- [x] 1.2 Keep the configured `runtime.nix` value alone and fail with a
  configured-value error when it is unusable; verify no fallback runs.
- [x] 1.3 Split the missing-runtime failure into the configured and the
  not-found forms; verify each names its locations or value and one
  useful next action, through isolated subprocess environment tests.
- [x] 1.4 Verify `pkg doctor` passes in a shell whose `PATH` lacks Nix
  while the stable system profile path holds a usable `nix` (fake
  runtime under the pure helper; real machine check recorded in the PR
  test notes).

## 2. NN-02: Installer shell setup

- [x] 2.1 Add managed startup blocks for Bash and Zsh with marker-based
  idempotency, preserved unrelated content and modes, symlink refusal,
  and ZDOTDIR plus XDG_STATE_HOME handling; verify first install, rerun,
  and unrelated-content preservation with temporary homes.
- [x] 2.2 Add Nix detection outside `PATH` and the conditional Nix
  `bin` entry; verify the detected path is reported and entered into the
  block with a fake per-user Nix under a temporary home.
- [x] 2.3 Add the honest missing-Nix ending and the unsupported-shell
  handling; verify no Bash syntax is written for Fish and the missing
  case leaves a working client with the vendor link.
- [x] 2.4 Add `PKG_INSTALL_SHELL_SETUP=0`; verify it changes no startup
  file and prints manual lines, and verify a checksum failure still
  edits no startup file.
- [x] 2.5 Verify realistic new Bash and Zsh shells resolve `pkg`, the
  installed package profile, and the detected Nix from the setup, with
  paths that contain spaces and shell metacharacters.
- [x] 2.6 Round-3 review fixes: unpredictable `mktemp` sibling for
  startup-file updates, block file passed to `awk` as a file argument
  instead of `-v` (literal backslashes in `TMPDIR`/`HOME` stay intact,
  and the escaped-checksum leading backslash marker is stripped),
  truthful concise final report for opt-out, missing Nix, unsupported
  shells, and skips, and the plain directory list for Fish instead of
  unverified Fish-specific quoting; each covered by a focused
  regression test.

## 3. NN-03: Documents and version

- [x] 3.1 Link this change from CONTRIBUTING and update the install
  documents for automatic setup, opt-out, and the new discovery error
  text; keep the active-design links intact.
- [x] 3.2 Move the workspace, lock, downloader, installer test fixture,
  and release links to `0.2.0-alpha.4`.

## 4. NN-04: Verification and release preparation

- [x] 4.1 Run the full local check set: Rust build, test, fmt, clippy,
  docs, installer tests, shellcheck, and the docs link checker; record
  exact commands and outcomes.
- [x] 4.2 Package the real `x86_64-linux` archive and test the packaged
  client outside the checkout, including a `PATH` without Nix; keep
  evidence outside tracked source.
- [x] 4.3 Prepare the alpha.4 release notes, PR body, and the primary's
  verification scripts; make no publication attempt from this sandbox.

## Completion evidence

Evidence: the installer-worker summary plus the PR test notes
  (commands, outcomes, and platform coverage).
