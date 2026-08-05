//! `pkg-core` — the shared domain vocabulary for `pkg`.
//!
//! This crate is intentionally empty as of **PR-1**. It exists only as the
//! permanent home for the Cargo workspace so that `cargo build`, `check`,
//! `clippy`, `doc`, and `fmt` operate against a real member crate — a
//! memberless virtual workspace makes all of those commands fail.
//!
//! **PR-2** (see `plans/11-pr-roadmap.md`) adds the domain modules — identity,
//! selector, realization, channel, version, and system — that the rest of `pkg`
//! builds on. Until then this crate exposes no types and no public API, by
//! design.
