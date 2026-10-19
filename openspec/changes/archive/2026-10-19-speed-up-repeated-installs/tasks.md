# Tasks

- [x] M1. Measure repeated 1/10/100-package installs on both hosts.
- [x] M2. Test native no-op generation behavior and same-name source collisions.
- [x] D1. Validate a shared output-check prototype against 100 real installed outputs.
- [x] T1. Add a regression that rejects per-request process growth.
- [x] P1. Add the bounded Nix output-check operation with strict decoding.
- [x] P2. Integrate batch checks with the existing exact-check fallback.
- [x] V1. Verify ambiguity, collisions, output selections, errors, and cancellation.
- [x] V2. Run required checks and repeat real command benchmarks on both hosts.
- [x] R1. Record product results and remaining limits.

## Single-package live check follow-up

- [x] M1. Screen the existing bounded evaluator for one matching installed request.
- [x] V3. Compare real native outputs, collisions, cancellation, and generation links.
- [x] D1. Keep the single-request route only if complete timings justify it.
- [x] R3. Record separate compiler settings and all trial samples.

Wait until the compiler-profile builds and measurements end on each host.
Keep an identical-source default-profile baseline for this separate trial.

Reject the single-request route. Real Nixpkgs is slower on both hosts.
The production source and tests are restored exactly.
