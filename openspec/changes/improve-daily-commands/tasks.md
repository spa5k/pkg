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

## 7. Verify

- [x] V1. Run the Rust checks and the tests.
- [x] V2. Run the daily commands in a test home on macOS.
- [ ] V3. Run the daily commands on Linux in an E2B sandbox.
