# macOS installer command access

Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`.
Plan: [INSTALL-01](../../plans/determinate-nix-stacked-prs.md#install-01-verify-macos-command-access).

## Evidence and diagnosis

The reported failure said `/opt/pkg/bin` was empty after successful setup.
The supplied archive does not include that directory's contents. It reports
no matching public command **or** denied access without distinguishing them.
Private user logs are not committed.

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

The wrapper now checks executable access and runs `pkg --version` as the
invoking user before completion. Failure returns nonzero and appends useful
diagnostics to the private installation log. It does not change host permissions.

- Before the fix, four subprocess cases incorrectly returned success: missing
  CLI, missing executable mode, inaccessible parent, and command launch failure.
- All six macOS packaging tests pass after the fix. Real `pkgbuild`, `pkgutil`,
  codesign, file modes, and child execution remain in these tests. Only elevation
  and the installed CLI path are controlled in the local harness.
- On the native VM, the fixed wrapper with the same signed package returned 1
  for the blocked parent. It omitted completion and retained the error in a
  mode-0600 log. Restoring the original directory mode made setup and user CLI
  execution pass. The installed release remains alpha.56.
- E and A source reviews found no actionable issue. No new release, fresh-install
  campaign, reboot, or SIP test is claimed by this bounded proof.
- Rust 1.96.1 G-LINT passed: formatting, Clippy, rustdoc, build, deny, and audit.
  Bash syntax and docs links pass. ShellCheck passes with SC2016 excluded for
  the existing shell setup instructions that intentionally print literal code.

VM: `pkg-cli-access-20260927`, a disposable clone of the alpha.56 release VM,
Apple Silicon macOS 15.7.7. The source VM and developer host were not changed.
The guest's noninteractive sudo validation policy was set for the test fixture.
The guest directory mode was restored after each check.
The temporary sudo policy was removed and the disposable VM was stopped.

Package SHA-256:
`62409a8949d64db2369515f76017a446ccfcd2e37b0ecef7feea4a59b6497a92`.
Original wrapper SHA-256:
`f57aa0a86d5e9b150d899c63e09a5dada3d9e7fdf97ebbf018af3fbacabc7dc5`.
Raw native and local command output stays outside the source diff.
