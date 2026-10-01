# Tasks

- [x] RF-01: Implement bounded metadata evaluation and complete SQLite snapshots.
- [x] RF-02: Stream search output, prepare data in update, and target ordinary name resolution.
- [x] RF-03: Serialize profile mutations and standalone app sync. Prevent duplicate installs.
- [x] RF-04: Scope conflict instructions and explain native rollback destinations.
- [x] RF-05: Namespace private cask files and preserve older installed layouts.
- [x] RF-06: Supply AppImage runtime fonts and verify Actual without user fonts.
- [x] RF-07: Reload macOS daemon settings and verify effective sandbox readiness.
- [x] RF-08: Run required checks and real Linux/macOS workflows. Record evidence and limits.

RF-08: All required checks and command workflows passed. Linux E2B cold
discovery produced the complete native catalog below the memory budget.
Both platforms passed 20 concurrent install rounds. Actual, Obsidian, and
Calibre passed removal and explicit restore checks. The final Mac visual
checks used Tart's built-in display. The real tap import passed without a
reboot. See [the verification report](../../../docs/verification/2026-10-01-search-and-real-use-fixes.md).
