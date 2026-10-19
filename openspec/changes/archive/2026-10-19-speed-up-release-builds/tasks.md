# Tasks

- [x] M1. Build default and thin-LTO releases from identical sources.
- [x] M2. Compare large searches and broad CLI workloads.
- [x] M3. Compare all six client installation modes with the same installer.
- [x] V1. Verify exact output, real profiles, and native workflows on both hosts.
- [x] D1. Select settings or keep the current release profile.
- [x] R1. Record build cost, size, every timing sample, and limits.

- [x] V2. Run the complete optimized Rust suite on both hosts before selection.

Keep the default profile. Search, CLI, and installer speeds are mixed.
Thin LTO makes smaller artifacts but raises the Mac build cost.
All native and optimized Rust checks pass on both hosts.


## Isolated symbol removal follow-up

- [x] M4. Compare fresh default and symbol-stripped releases from the accepted search source.
- [x] M5. Measure all command groups, search, and all installer modes with complete notice payloads.
- [x] V3. Check exact output, native profiles, client identity, and compiler source equality.
- [x] D2. Select only after complete timings and check the diagnostic tradeoff.
- [x] R2. Keep every sample, build cost, artifact size, failure, and decision.

Change only the strip setting in temporary build environments.
Keep optimization, code generation units, LTO, panic behavior, and Cargo.toml unchanged.
Build into separate fresh target directories. Do not compile beside a timed workload.

Reject isolated symbol removal. Sizes improve but runtime samples stay mixed.
Keep default diagnostic symbols and every experimental result.
