# Tasks

## 1. Base

- [x] U0. Add color, terminal width, and a table helper to `output`.

## 2. Defects

- [ ] U1. Show one short cause for native evaluation errors.
- [ ] U2. Refresh search data with the new client after an update.
- [ ] U3. Hide Nix chatter in install, remove, upgrade, and rollback.
- [ ] U4. Fix empty and misaligned `pkg history` output.
- [ ] U5. Find and fix duplicate entries for one package.
- [ ] U6. Use color, honor `--no-color`, and fix `doctor` grammar.

## 3. Names and search

- [ ] U7. Resolve bare names in the fixed source order.
- [ ] U8. Match main command names.
- [ ] U9. Show one search list that fits the terminal.
- [ ] U10. Read `info` and the name check from local search data.

## 4. Profile output

- [ ] U11. Make `pkg upgrade` upgrade all packages by default.
- [ ] U12. Report names and versions in results, `list`, and `history`.

## 5. Help and documents

- [ ] U13. Use package words in help.
- [ ] U14. Update `docs/commands.md` and `README.md`.

## 6. Verify

- [ ] V1. Run the Rust checks and the tests.
- [ ] V2. Run the daily commands in a test home on macOS.
- [ ] V3. Run the daily commands on Linux in an E2B sandbox.
