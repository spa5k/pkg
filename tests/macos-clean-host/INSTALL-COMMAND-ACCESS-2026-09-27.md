# macOS installer command access

Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`.
Plan: [INSTALL-01](../../plans/determinate-nix-stacked-prs.md#install-01-verify-macos-command-access).

## Evidence and diagnosis

The reported failure said `/opt/pkg/bin` was empty after successful setup.
The supplied archive does not include that directory's contents. It reports
no matching public command **or** denied access without distinguishing them.
Private user logs are not committed.

Two attempts failed because sudo had no terminal for a password prompt. Other
attempts passed the root installation checks. The outer bootstrap log ended
before the final user command and health checks, so a shared success log could
omit the error the user saw afterward. The archive also contains legacy `/pkg`
state; it does not establish that this state caused the failure.

The public CLI belongs at `/usr/local/bin/pkg`. `/opt/pkg/bin` contains private
service binaries and restricts access to root and the broker group. The
installer checks installed signatures as root. That check cannot prove that
the original user can traverse the CLI's parent directories.

A copy of the release VM reproduced a wrapper defect with the exact signed
alpha.56 package and wrapper. Changing `/usr/local/bin` from 0755 to root-owned
0700 made the user command fail with exit 126, `Permission denied`. The original
wrapper still returned zero and printed `pkg setup completed.`

This is a confirmed verification defect. The coworker's actual permissions,
missing-file status, and exact cause remain unconfirmed. The archive does not
establish a SIP or package-extraction failure.

## Correction and proof

The wrapper records path metadata before setup as the user, then as root after
approval, and again after installation. It checks existing command parents
before elevation and rejects inaccessible, symlink, or non-directory paths.
Missing parents reach the existing installer. The wrapper does not create or
change shared directories. Completion requires executable access and a working
`pkg --version` as the invoking user.

If sudo requires authentication, the wrapper checks for a terminal and requests
approval there. Cancellation or no terminal stops installation. The privileged
payload uses noninteractive sudo after approval. Logs contain metadata and
errors, not password input or receipt contents. Generic permission failures no
longer automatically suggest Full Disk Access.

The bootstrap template retains final version and doctor output and errors in
its private log. Health checking requires a readable status of exactly zero;
successful `tee` output alone cannot establish a healthy installation.

- Before the fix, four subprocess cases incorrectly returned success: missing
  CLI, missing executable mode, inaccessible parent, and command launch failure.
- All eight macOS packaging tests pass after the fix. Real `pkgbuild`, `pkgutil`,
  codesign, file modes, and child execution remain in these tests. Only elevation
  and the installed CLI path are controlled in the local harness.
- On the native VM, the final wrapper with the same signed package returned 1
  for a blocked parent before requesting approval or running the root installer.
  It omitted completion and retained the error in a mode-0600 log.
- Real sudo checks passed: no terminal refused before installation, EOF at the
  password prompt refused, and approval through a terminal completed setup.
  A final healthy run with restored directory permissions also passed. The
  installed release remains alpha.56.
- All eight bootstrap process tests and three docs tests pass. Pre-fix controls
  catch omitted final diagnostics and a health-status write failure that could
  otherwise print `pkg is ready`. The fault uses a real conflicting directory.
- The full release-tool suite passes (51 tests), as do the four renderer tests.
  The old `RenderTests.test_doctor_path_is_idempotent` extracted a shell fragment
  by a literal source marker. The new bootstrap test
  `test_doctor_receives_idempotent_path_through_the_complete_bootstrap` preserves
  single PATH entries and repeat stability through a fresh doctor child capture
  on each run, for both platforms. Removing either PATH addition is detected.
  `docs/install.sh` still owns this contract; the installer CI jobs run the keeper.
- E and A source reviews have no open findings. This proof covers repeated setup
  on macOS 15.7.7. It does not prove fresh installation with absent shared command
  directories, Darwin 27 behavior, reboot, SIP, or TCC. No new release is claimed.
- Rust 1.96.1 G-LINT passed: formatting, Clippy, rustdoc, build, deny, and audit.
  Bash syntax and docs links pass. ShellCheck passes with SC2016 excluded for
  the existing shell setup instructions that intentionally print literal code.

VM: `pkg-cli-access-20260927`, a disposable clone of the alpha.56 release VM,
Apple Silicon macOS 15.7.7. The source VM and developer host were not changed.
The guest's sudo policy required credential validation for the prompt fixture.
The guest directory mode was restored after each check.
The temporary sudo policy was removed and the disposable VM was stopped.

Package SHA-256:
`62409a8949d64db2369515f76017a446ccfcd2e37b0ecef7feea4a59b6497a92`.
Original wrapper SHA-256:
`f57aa0a86d5e9b150d899c63e09a5dada3d9e7fdf97ebbf018af3fbacabc7dc5`.
Final candidate wrapper SHA-256:
`e91915d0449177e795b014ba0649aff6721df5f2de5ce6385b488530ab85fed5`.
Raw native and local command output stays outside the source diff.
