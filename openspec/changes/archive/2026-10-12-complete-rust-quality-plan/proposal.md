# Complete the remaining Rust quality plan

The first repair phase ends at `df1af1d`.
The [audit](../../../../docs/verification/rust-quality-audit-2026-10-12.md) records six remaining items.
Complete all six items in this change.

1. Give the child supervisor full lifetime ownership. Prevent signaling a stale PID after reaping.
2. Preserve typed interruption through app helper probing, payload preparation, and launcher refresh.
3. Put shared package-change ordering and lock ownership in one module. Keep command-specific policies explicit.
4. Prevent invalid catalog IDs and saved source bindings at public construction and decode boundaries.
5. Separate unrelated assertions in the longest CLI journeys. Keep the existing behavioral coverage through real fixture owners.
6. Use structured discovery results for absent output roots. Preserve real evaluation errors and cache policy.

Keep Nix as the authority for profiles and generations.
Keep the full Clippy all and pedantic policy with warnings denied.
Use the Rust Style Guide and Rust API Guidelines.
Do not introduce a second transaction journal, an async runtime, or a generic backend framework.

The [design](design.md) defines the workflow and acceptance checks.
The [tasks](tasks.md) record completion.
