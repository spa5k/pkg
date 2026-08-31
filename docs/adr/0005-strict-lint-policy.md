---
Status: Proposed
---

# Strict lint policy: make the repository hostile to mediocre code

## Context

Rust prevents bad code. It does not prevent mediocre code. AI makes mediocre
code cheap to produce. This repository already has a Linux-only G-LINT gate
(`cargo fmt`, `clippy -D warnings`, `cargo doc -D warnings`, `cargo-deny`,
`cargo-audit`). That gate is not enough. Proof on the `dn/16-determinate-cutover`
branch:

- `cargo clippy --locked --workspace --all-targets --all-features` fails on
  macOS with 7 errors and reports 86 dead-code warnings. Linux CI never
  compiles the macOS-gated code, so the gate stayed green.
- `cargo test --locked --workspace` fails on macOS: one stale test still
  encodes the old Linux-only live-uninstall output contract.
- 1029 pedantic + nursery warnings exist across the workspace. Blanket
  enablement would drown a branch in noise, but the high-value subset is
  tractable.
- 36 `#[allow(...)]` sites exist. Most carry no reason. Suppression is cheap.
- 1249 `.clone()` lines, 533 `.to_owned()`, 178 `Box<dyn Error>`, and
  2479 `.map_err(` lines exist across source and tests. These are not bugs.
  They are trends the repository cannot currently see.

This ADR decides the lint policy. It does not delete code or change behavior.

## Decision

The policy has three enforcement tiers. All numbers below were measured on
commit `854f288` with toolchain `1.96.1` on Apple Silicon macOS.

1. **Deny.** The lint fails the local build and CI. Denied lints must be clean
   before merge. New suppressions use `#[expect(lint, reason = "...")]` so the
   suppression dies when the lint stops firing.
2. **Ratchet.** The lint is a warning. A checked-in baseline
   (`tools/quality/baseline.json`) records the current count. CI fails when
   any count grows. Debt may only shrink.
3. **Touched files.** Every file changed in a pull request must be clean at
   the strict level (`clippy --lib --bins --all-features -- -D warnings` plus
   the strict thresholds below). Existing debt in untouched files stays
   tolerated. Touched files must improve.

### 1. Compiler lints — workspace `[lints.rust]`

| Lint | Level | Why |
|---|---|---|
| `unsafe_op_in_unsafe_fn` | deny | Every unsafe operation must sit in an explicit `unsafe` block with a `SAFETY` comment. 14 unsafe blocks exist; 13 lack the comment. |
| `missing_docs` | deny | Public API must be documented. Most crates already deny this; the policy makes it uniform. |
| `unused_must_use` | deny | Ignored `Result`s and guards are the classic silent-failure shape. |
| `unused_imports`, `unused_variables`, `unused_assignments` | deny | Compiler-detected leftovers. |
| `dead_code` | deny | The 86 dead-code warnings include real DN-17/DN-18 deletion residue. The planned DN-19 deletion modules (`pkg-nix/src/managed/*`) get scoped `#[expect(dead_code, reason = "DN-19 ...")]`. |
| `unused_crate_dependencies` | deny | Dependency hygiene. Platform-gated dependencies get per-crate `expect` with reason. |
| `unsafe_code` | per-crate | Keep existing `forbid`/`deny` in the crates that have it. Do not forbid workspace-wide: `pkg-macos-security` needs scoped unsafe for FFI. |

### 2. Clippy core — workspace `[lints.clippy]`

| Lint | Level | Why | Measured |
|---|---|---|---|
| `all` | deny | Correctness, suspicious, style, complexity, perf. Currently CI-only (`-D warnings`); make it local so the editor and CI agree. | — |
| `redundant_pub_crate` | deny | Visibility discipline. Already caught real macOS-only debt. Auto-fixable. | 69 |
| `missing_const_for_fn` | deny | Const-eval hygiene. Auto-fixable. | 231 |
| `needless_lifetimes` | deny | The exact lint that broke the macOS build. Auto-fixable. | 8 |
| `option_if_let_else` | deny | Simpler control flow. Auto-fixable. | 14 |
| `or_fun_call` | deny | Eager allocation on fallible paths. Auto-fixable. | 7 |
| `useless_let_if_seq` | deny | Auto-fixable. | 4 |
| `derive_partial_eq_without_eq` | deny | `Eq`/`Hash` consistency. Auto-fixable. | 2 |
| `needless_collect` | deny | Auto-fixable. | 1 |
| `redundant_clone` | deny | Clone budget: a clone is a performance decision, not a borrow-checker escape hatch. Auto-fixable. | 13 |
| `future_not_send` | deny | Multi-threaded runtime safety. | 4 |
| `too_long_first_doc_paragraph` | deny | Docs are part of the API. | 10 |
| `suspicious_operation_groupings` | deny | Operator precedence traps. | 2 |
| `use_self` | ratchet | Style preference with 233 hits. Blanket fix is noise. | 233 |
| `significant_drop_tightening` | ratchet | Behavior-adjacent. Needs review per site, not mechanical application. | 133 |

### 3. Panic policy — workspace `[lints.clippy]`

| Lint | Level | Why | Measured (lib+bins) |
|---|---|---|---|
| `unwrap_used` | deny | If failure is possible, model the failure. Test modules carry `#![expect(...)]` with reason. | 2 |
| `expect_used` | deny | `expect` is the sanctioned invariant mechanism: if you intentionally panic, name the invariant. Every production `expect` carries an invariant message. Tests may expect. | 10 |
| `panic` | deny | Production code returns errors; it does not panic. Tests may panic. | 0 |
| `todo`, `unimplemented` | deny | No shipped placeholder. | 0 |
| `dbg_macro` | deny | No debugging output in production. | 0 |
| `print_stdout`, `print_stderr` | deny | Product output goes through the output abstractions. The 2 hits are audited and either fixed or expected with reason. | 2 |
| `allow_attributes_without_reason` | deny | Suppressions are expensive: every `allow` must carry `reason =`. This makes the "no bare suppression" rule a compiler check, not a review promise. | 22 |

### 4. Pedantic — curated

The blanket `pedantic = "deny"` policy already exists in `pkg-installer` and
`pkg-macos-security` and stays. Blanket workspace-wide pedantic is rejected:
`missing_errors_doc` alone has 704 hits and would drown the branch.

Curated denies:

| Lint | Why | Measured |
|---|---|---|
| `needless_pass_by_value` | Cheap-to-pass types by value. Auto-fixable. | 35 |
| `redundant_closure_for_method_calls` | Auto-fixable. | 61 |
| `match_same_arms` | Collapsed arms reveal real structure. | 44 |
| `doc_markdown` | Backticks in docs. | 30 |
| `duration_suboptimal_units` | Readability of time units. | 20 |
| `unnecessary_wraps` | API smell: `Result` that never errors misleads callers. | 12 |
| `large_stack_arrays` | Stack overflow risk. | 12 |
| `cast_possible_wrap`, `cast_possible_truncation`, `cast_precision_loss` | Silent numeric corruption. | 21 |
| `single_match_else` | Simpler control flow. | 10 |
| `unnecessary_literal_bound` | Auto-fixable. | 8 |
| `items_after_statements` | Locals before items. | 7 |
| `struct_excessive_bools` | Boolean fields hide state machines. | 6 |
| `missing_fields_in_debug` | Complete debug output. | 6 |
| `must_use_candidate` | APIs whose result must not be dropped. | 6 |
| `similar_names` | Confusable identifiers. | 5 |
| `manual_let_else`, `if_not_else`, `single_char_pattern`, `semicolon_if_nothing_returned`, `used_underscore_binding`, `no_effect_underscore_binding`, `zero_sized_map_values`, `float_cmp`, `unnecessary_literal_bound`, `fn_params_excessive_bools`, `trivially_copy_pass_by_ref`, `missing_panics_doc` | Small, high-signal polish lints. Auto-fixable where noted. | 2–5 each |
| `missing_errors_doc` | ratchet only. 704 hits; the debt may not grow. | 704 |

### 5. Complexity budgets

Configured in `clippy.toml`. The strict thresholds are the policy. The deny
levels reflect measured production debt (lib + bins, not tests).

| Threshold | Value | Level | Measured production debt | Test debt |
|---|---|---|---|---|
| `cognitive-complexity-threshold` | 10 | deny in production | 10 — fixed on this branch | 81 — ratchet |
| `type-complexity-threshold` | 150 | deny in production | 20 — fixed on this branch | 51 — ratchet |
| `too-many-lines-threshold` | 50 | ratchet + touched-files | 132 | 380 |
| `too-many-arguments-threshold` | 5 | ratchet + touched-files | 73 | 153 |
| `excessive-nesting-threshold` | 3 | deny in production | measured during application | — |
| `struct-field-count-threshold` | — | not set; enum/struct budgets stay review policy for now | — | — |

Tests get a separate, looser treatment: the same thresholds, but violations
are ratcheted instead of denied. Long integration tests are legitimate.
New test code must still be clean via the touched-files rule.

### 6. Slop ratchet — `tools/quality`

A dependency-free script counts repository-level patterns and compares them
against `tools/quality/baseline.json`. CI fails on growth.

| Pattern | Why |
|---|---|
| `.clone()`, `.to_owned()`, `.to_string()`, `.into_owned()` | Ownership-copy budget. AI clones to satisfy the borrow checker. |
| `Arc<Mutex<`, `Arc<RwLock<` | Concurrency smell: often the Rust equivalent of a global. 5 hits today. |
| `Box<dyn Error` | Lossy error type. 178 hits today. Structured errors are the library contract. |
| `std::process::exit` | Bypasses cleanup and error types. 2 hits today. |
| `unsafe {` blocks without a `SAFETY` comment | Unsafe hygiene. 13 hits today, all in `pkg-macos-security`. |

### 7. CI changes — `ci-fast.yml`

- Add a `g-lint-macos` job on `macos-14` (Apple Silicon): format, clippy
  (all targets), and `cargo test --workspace`. This closes the exact hole
  that produced the 7 macOS clippy errors and the stale test.
- Add a `g-quality` job that runs the strict gate, the touched-files rule,
  and the slop ratchet.

### 8. Local workflows — `Justfile`

- `just lint` — format check, clippy deny set, docs.
- `just lint-strict` — adds the threshold lints at `-D warnings` for
  lib + bins (the CI touched-files level).
- `just fix` — `cargo clippy --fix` for the auto-fixable deny set.
- `just ratchet-rebase` — deliberate debt paydown records a new baseline.
  Manual step; never automatic.

## Rejected alternatives

- **Blanket `pedantic = "deny"` workspace-wide.** 1029 hits, led by
  `missing_errors_doc` (704). It drowns the branch and produces
  `#[expect]` noise instead of design pressure.
- **Blanket `restriction = "deny"`.** Restriction lints are designed to be
  cherry-picked; many contradict each other or this codebase's explicit
  invariants (e.g. `let_underscore_drop` against the 180 deliberate
  `let _ =` guards).
- **AST-based architecture rules** (`no-single-implementation-trait`,
  `no-trivial-wrapper`, layer boundaries). This codebase uses traits as
  test seams by design. These rules are false-positive generators today.
  Revisit after DN-21 through DN-32 when the seams are re-proved.
- **`forbid(unsafe_code)` workspace-wide.** `pkg-macos-security` needs
  scoped unsafe for FFI. `forbid` cannot be scoped. Per-crate `deny` plus
  the `SAFETY`-comment ratchet gives the same guarantee without blocking
  legitimate code.
- **Complexity lints denied everywhere immediately.** 132 production
  functions exceed 50 lines. Refactoring all of them in one branch risks
  the proof-verified cutover code. The ratchet + touched-files rule gives
  the same guarantee — no new debt, touched files must improve — without
  one giant destabilizing refactor.

## Consequences

- Local `cargo clippy` becomes as strict as CI on every platform. macOS
  developers cannot merge platform-gated lint debt silently.
- A suppression is a first-class artifact: it must name a reason, and it
  dies when the lint stops firing.
- New code must be clean. Existing debt cannot grow. Debt shrinks only
  through deliberate, reviewable work.
- The slop baseline records today's counts. It is a floor, not a ceiling.
- This branch fixes the deny-set debt (the 7 macOS errors, 86 dead-code
  sites, panic-policy hits, 10 cognitive, 20 type-complexity, the curated
  auto-fixable batch, 13 `SAFETY` comments, 22 allow-reasons, and the
  stale uninstall test) and leaves the ratcheted debt untouched.
