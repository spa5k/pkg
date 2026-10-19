# Release artifact experiments

The first trial combines thin LTO, one code generation unit, and debug-info removal.
Its complete mixed results select no production profile change.

## Isolated symbol removal

This follow-up tests only release symbol removal.
Use the accepted prepared-insert source and an unchanged production manifest.
Require a fresh default build to reproduce the accepted binary.
Compare that binary with a separate stripped build from the same source.
Keep fresh target directories and every build log.
Record complete archive size, installer modes, search, and CLI results.
Do not infer general download speed from local HTTP transfer times.

The [Cargo profile reference](https://doc.rust-lang.org/cargo/reference/profiles.html#strip)
and [rustc options](https://doc.rust-lang.org/rustc/codegen-options/index.html#strip)
define symbol and debug-info removal. Context7 returns its monthly quota error.
Symbol removal reduces diagnostic symbol information.
Record that tradeoff before selection.
Keep all default artifacts for comparison and recovery.
