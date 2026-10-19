# Tasks

- [x] M1. Profile real archive unpack and artifact staging.
- [x] D1. Select a measured change or record rejection.
- [x] T1. Add a focused regression for a selected behavior, if needed.
- [x] P1. Implement the selected change or record rejection.
- [x] V1. Compare complete outputs and run affected native checks.
- [x] R1. Record timing, failures, and limits.

No production speed change is selected. T1 and P1 need no code.
V1 records full staged/output hashes and entry counts for the direct profile.
It does not claim a comparative output check or a new sandbox builder run.

## Pinned interpreter follow-up

- [x] M2. Repeat the phase profile with the exact derivation Python.
- [x] R2. Record interpreter identity, phase times, and limits.

The first direct driver uses local Python 3.11.15 with pinned archive tools.
The actual derivation uses Python 3.14.7. No copy-speed change is selected.

Pinned Python profile is complete. Staged hashes match. Output hashes include
temporary wrapper paths. No normalized output equivalence or speed change is claimed.
