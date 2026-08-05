# `pkg`

> **Status: implementation is just beginning.** Nothing here ships yet — there is no
> runnable `pkg` binary, no channel, and no installer. This repository currently contains
> **plans only**. Every binding design decision, invariant, and open question lives in
> [`plans/`](plans/), which is the **source of truth**. This README orients you there and
> makes no product claims beyond what those plans commit to.

`pkg` (working codename) is a **planned** single **Rust** binary that aims to provide a
brew-/paru-style imperative package workflow — `search`, `info`, `install`, `remove`,
`list`, `outdated`, `update`, `upgrade`, `pin`/`unpin`, `history`, `rollback`, `gc`,
`repair`, `doctor`, `completion` — on top of a **fully hidden, bundled, product-managed
Nix** that the user never types or configures directly. Whether and how each of these lands
is decided by the plan set and its open go/no-go spikes — not announced here.

## Read the plans first

The navigator and index for the reconciled plan set (`00`–`12`) is
[`plans/README.md`](plans/README.md) — **start there**. It owns no new decisions; it
summarizes and links to the documents that do.

| If you want… | go to |
| --- | --- |
| The navigator + system summary | [`plans/README.md`](plans/README.md) |
| Decisions, invariants, glossary, scope | [`plans/00-overview-and-decisions.md`](plans/00-overview-and-decisions.md) |
| The threat model & trust boundaries | [`plans/08-security-model.md`](plans/08-security-model.md) |
| The PR roadmap & build DAG (PR-0 … PR-38) | [`plans/11-pr-roadmap.md`](plans/11-pr-roadmap.md) |
| Open decisions, spikes, risks | [`plans/12-open-decisions-and-risks.md`](plans/12-open-decisions-and-risks.md) |
| How to contribute & review rules | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| A clickable day-to-day UX prototype (HTML) | [`artifacts/pkg-day-to-day-prototype.html`](artifacts/pkg-day-to-day-prototype.html) |

> Five go/no-go **spikes (S1–S5)** are still open, and several high-severity residuals are
> disclosed rather than hidden. No irreversible architecture is committed until the
> corresponding spike's Decision Record is accepted. See `plans/12` and the roadmap
> guardrails in [`plans/11`](plans/11-pr-roadmap.md).

## Current state

This is **PR-1** of the roadmap: the Cargo workspace and the permanent `pkg-core` crate
scaffold now exist, along with the toolchain (`rust-toolchain.toml`, pinned exactly to
`1.96.1`), the lint/format/deny config (`clippy.toml`, `rustfmt.toml`, `deny.toml`), and the
Fast-CI **G-LINT** job ([`ci-fast.yml`](.github/workflows/ci-fast.yml): `fmt`,
`clippy -D warnings`, `doc`, `build`, `cargo deny check`, `cargo audit`). The `pkg-core`
crate is an **empty scaffold** — its domain types/logic/tests arrive in
[PR-2](plans/11-pr-roadmap.md); the product binary and every other crate arrive later. See
the roadmap's ["Initial implementation starting instructions"](plans/README.md) for the
sequenced entry points.

## License

The project license is **not yet chosen**. Until
[DR-015](plans/12-open-decisions-and-risks.md) is superseded by an Accepted decision, all
source in this repository is **all rights reserved** — no public license is granted, no
`license` field is set in the manifests, and no `SPDX-License-Identifier` headers are present.
(The `deny.toml` permissive license allowlist is a *dependency* policy, not a project
license.) Until then, treat everything here as unreleased material belonging to the project
authors.
