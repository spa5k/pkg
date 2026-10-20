# Tasks

## 1. Measure

- [x] S1. Measure the current first install: time, output lines, and user steps.

## 2. Installer

- [x] S2. Remove the installer options and fix the install layout.
- [x] S3. Run the new client before it replaces the old client.
- [x] S4. Offer the Nix install once when no Nix exists.
- [x] S5. Add Fish setup and working Bash and Zsh completion.
- [x] S6. Enable the daily job instead of preparing search data.
- [x] S7. Rewrite the installer tests, including the Linux isolation tests.

## 3. Client

- [x] S8. Remove `pkg setup`. Prepare missing search data in the first search.
- [x] S9. Update the client in `pkg update` and the daily job.
- [x] S10. Hide `background`, `shellenv`, and `completion` from help.
- [x] S11. Report the daily job and `PATH` state in `pkg doctor`.
- [x] S12. Add `pkg self uninstall`.
- [x] S13. Show one transient status line during long operations.

## 4. Release and documents

- [x] S14. Publish `install.sh` as a release asset.
- [x] S15. Update the install, command, and privacy documents.
- [x] S16. Update the installer benchmark for the new options.

## 5. Verify

- [x] V1. Run the Rust checks, the shell checks, and the Python tests.
- [x] V2. Install from a local release archive in a test home on macOS.
