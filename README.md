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

This is **PR-0** of the roadmap: the repository, the plans-as-source-of-truth convention,
the contributor/reviewer model, and a CI linkcheck that guards plan cross-references and
the threat-model baseline. There is **no Cargo workspace and no Rust code yet** — that
arrives in [PR-1](plans/11-pr-roadmap.md) and later. See the roadmap's
["Initial implementation starting instructions"](plans/README.md) for the sequenced entry
points.

## License

To be finalized (see `plans/10`). Until then, treat all content here as unreleased planning
material belonging to the project authors.
