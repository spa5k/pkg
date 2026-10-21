# Tasks

## 1. Base

- [x] U0. Add color, terminal width, and a table helper to `output`.

## 2. Defects

- [x] U1. Show one short cause for native evaluation errors.
- [x] U2. Refresh search data with the new client after an update.
- [x] U3. Hide Nix chatter in install, remove, upgrade, and rollback.
- [x] U4. Fix empty and misaligned `pkg history` output.
- [x] U5. Find and fix duplicate entries for one package.
- [x] U6. Use color, honor `--no-color`, and fix `doctor` grammar.

## 3. Names and search

- [x] U7. Resolve bare names in the fixed source order.
- [x] U8. Match main command names.
- [x] U9. Show one search list that fits the terminal.
- [x] U10. Read `info` and the name check from local search data.

## 4. Profile output

- [x] U11. Make `pkg upgrade` upgrade all packages by default.
- [x] U12. Report names and versions in results, `list`, and `history`.

## 5. Help and documents

- [x] U13. Use package words in help.
- [x] U14. Update `docs/commands.md` and `README.md`.

## 6. Found during verification

- [x] U15. Accept command names in `remove` and `upgrade`.
- [x] U16. Skip generations with the same packages in `pkg rollback`.
- [x] U17. Keep search data when one entry exceeds the memory budget.

## 7. Remaining commands

- [x] U18. Handle Linux without a systemd user session in the installer, `background`, `doctor`, and `self uninstall`.
- [x] U19. Sort equal-rank JSON search rows in source order.
- [x] U20. Check the rollback number before the rollback starts, and print how to go back.
- [x] U21. Give one plain cause and one action for taps, Casks, doctor, help, and bad input.

## 8. Edge cases

- [x] U22. Fit piped and narrow search output, and write hints to stderr.
- [x] U23. Give every search JSON row `install_command` and `command`.
- [x] U24. Hide Nixpkgs packages that cannot install, and refuse them before Nix runs.
- [x] U25. Count an entry with the same build as installed.
- [x] U26. Report a missing HOME, XDG folders that are files, and a broken lock file with their paths.

## 9. Search data

- [x] U27. Read the Nixpkgs search data from the NixOS channel list.
- [x] U28. Run the local evaluation in parallel, and continue it after Ctrl-C.
- [x] U29. Show an exact-name match while the local evaluation runs.
- [x] U30. Show the origin of the search data in `doctor`.

## 10. Verify

- [x] V1. Run the Rust checks and the tests.
- [x] V2. Run the daily commands in a test home on macOS.
- [x] V3. Run the daily commands on Linux in an E2B sandbox.
- [x] V4. Repeat V1 to V3 for sections 7 to 9.
