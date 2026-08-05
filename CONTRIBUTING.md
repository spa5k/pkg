# Contributing to `pkg`

> **The plans are the source of truth.** Design decisions, invariants, threats, risks, and
> the PR DAG all live in [`plans/`](plans/README.md). This document makes the *engineering
> process* operational: how PRs are sized, reviewed, gated, and rolled back. It restates the
> rules from the roadmap ([§1 Principles](plans/11-pr-roadmap.md#1-principles) and
> [§2 Reviewer Model](plans/11-pr-roadmap.md#2-reviewer-model)) in checklist form; if this
> file and the roadmap ever disagree, **the roadmap wins** and a follow-up fixes this file.

## 1. Before you open a PR

- **Find your PR in the [roadmap](plans/11-pr-roadmap.md#4-pr-entries).** Every change maps
  to a numbered PR (PR-0 … PR-38). State the PR number in the PR description and copy its
  `Purpose / Owns / Depends / Tests & gates` fields.
- **Respect `Depends:`.** A PR may not merge until each listed dependency has merged. The
  DAG and parallelism matrix in the roadmap are authoritative; do not invent new edges.
- **One purpose per PR.** A reviewer must be able to hold the whole change in their head.
  Target a few hundred lines of *logic* (fixtures/tests excluded); anything larger is split.
- **Every PR is reversible.** Your PR description must include the rollback strategy from the
  roadmap entry (usually `git revert` plus any state it leaves behind).
- **Plans are owned.** Edit a plan document only if you own that area (see reviewer model
  below) **or** the change is links/typos/cross-references that do not alter a decision.
  Link-only fixes to plan cross-references are always allowed. Never silently revert another
  contributor's work — propose the change and let the owner merge it.

## 2. Reviewer model (areas F / E / A)

Three area owners (people TBD; **roles fixed** — see [roadmap §2](plans/11-pr-roadmap.md#2-reviewer-model)):

| Role | Owns | Plans |
| --- | --- | --- |
| **F** — Foundations & Trust | architecture, channel/TUF, Nixpkgs/index, Nix adapter contract | `00`–`03` |
| **E** — Execution & Platform | resolve/install/build, state/locks/gen/GC, CLI/UX, installers | `04`–`07` |
| **A** — Assurance | security, tests, release/ops, roadmap, risks | `08`–`12` |

**Every PR requires:**

1. A **primary reviewer** — the owner of the plan most touched by the PR.
2. **≥ 1 cross-area reviewer** — a *different* owner than the primary.
3. **Mandatory security review by A** for any PR on the **trust surface**:
   channel/keys, state integrity, privileged helper, substitution, eval purity, uninstall,
   or release signing. The roadmap marks these with **"mandatory security"** on the PR entry.

Spikes (PR-4 … PR-8) additionally require sign-off by the owner whose plan the spike informs,
and must close with an accepted Decision Record in [`plans/12`](plans/12-open-decisions-and-risks.md)
before their dependent PR opens. No irreversible architecture merges before the gating spike's
DR is accepted (roadmap §9 guardrails).

## 3. Required gates

- **All PRs:** the **docs-linkcheck** CI job is green. It validates Markdown cross-references
  across the repo (including fragments), rejects repository-escaping paths, and enforces the
  PR-0 structural invariants (all of `plans/00`–`12` and `plans/README.md` exist;
  `README.md` links the threat model; this file links the reviewer model). Run it locally:
  ```sh
  python3 .github/scripts/check_docs_links.py
  ```
- **Code PRs (PR-1 onward):** the lanes defined in [`plans/09`](plans/09-testing-and-validation.md)
  apply as the roadmap entry specifies — at minimum the Fast-CI **G-LINT** job
  ([`ci-fast.yml`](.github/workflows/ci-fast.yml): `fmt`, `clippy -D warnings`, `doc`,
  `build`, `cargo deny check`, `cargo audit`). Do not disable a gate to make CI green; fix
  the cause or split the PR.
- **Trust-surface PRs:** the relevant security test lane from
  [`plans/08`](plans/08-security-model.md) must be green *and* A's security review recorded
  on the PR before merge.

### 3.1 Toolchain, MSRV, and the local G-LINT gate

- **Repo toolchain.** `rust-toolchain.toml` pins the exact channel **`1.96.1`** (profile
  `minimal`, components `rustfmt` + `clippy`). CI and contributors use this exact toolchain;
  it is deliberately **not** a mutable `stable`. The workspace **MSRV is `1.96`**
  (`[workspace.package] rust-version`), matching the repo toolchain.
- **Local G-LINT gate.** This mirrors `.github/workflows/ci-fast.yml` step-for-step; every
  command must be green locally before a code PR opens:
  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
  cargo build --workspace --all-targets --all-features --locked
  cargo deny --locked check        # cargo-deny 0.20.2: cargo install --locked cargo-deny@0.20.2
  cargo audit                      # cargo-audit 0.22.2: cargo install --locked cargo-audit@0.22.2
  ```
  `cargo` normally selects the pinned toolchain automatically via `rust-toolchain.toml`,
  **but an exported `RUSTUP_TOOLCHAIN` environment variable overrides it** (that is how
  rustup resolves precedence). If your shell has one set — the usual cause of “bare
  `cargo`/`rustc` used the wrong compiler despite the repo pin” — the final gates will run
  against the wrong toolchain silently. Clear it, or pin it to the repo channel, before the
  final gate:
  ```sh
  unset RUSTUP_TOOLCHAIN            # let rust-toolchain.toml decide
  RUSTUP_TOOLCHAIN=1.96.1 cargo --version   # …or pin it to the repo channel explicitly
  ```
  The final G-LINT gate must run on exactly `1.96.1`; an older toolchain (below the MSRV
  of `1.96`) is **not** acceptable for final validation, even temporarily.
- **License deferral (DR-015).** The project license is undecided. Until
  [DR-015](plans/12-open-decisions-and-risks.md) is superseded by an Accepted DR, do **not**
  add a `license` field to any `Cargo.toml` and do **not** add `SPDX-License-Identifier`
  headers to source files. The `deny.toml` license allowlist is a *dependency* policy, not a
  project license.

## 4. Rollback evidence

Every merged PR must leave enough trace to roll back cleanly:

- The PR description restates the roadmap **Rollback** field (e.g. `revert`; what state, if
  any, is left behind, and how it is cleaned up).
- PRs that lay down files or mutate state (installers, provisioning, migrations) record the
  manifest/keys/paths needed to undo them, as the roadmap entry requires.
- For release/channel PRs, rollback is the published higher-`sequence` channel — rehearse key
  revocation before it is needed (roadmap §7, [`plans/10`](plans/10-release-and-operations.md)).

## 5. Spikes and unresolved questions

Open go/no-go questions live in [`plans/12`](plans/12-open-decisions-and-risks.md) as
**DR-***/spikes **S1–S5**/**RISK-***. Raise new ones there (status `Proposed`) rather than
leaving them in a PR thread. Mark a DR `Accepted` only when the spike owner and the affected
area owner(s) both sign off.

---

*This document is process, not product. For design authority, read the plans.*
