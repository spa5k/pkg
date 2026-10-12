# Design: implement the accepted pkg audit repairs

Scope: [proposal](proposal.md). Six accepted audit findings, one
architect decision each. The decisions below record the two structurally
distinct choices weighed for every ownership or type change and the
reason the chosen one wins.

## 1. Fail-closed setup, typed importer errors

**Decision.** The one-time administrator setup runs only when its
preconditions are inspectable from this account. `/etc/nix/nix.conf`
that exists but is not world-readable refuses ALL automatic
administrator steps with manual review instructions: pkg prints what an
administrator must check (the file may hold `access-tokens` or other
secrets) and that making it readable is a manual decision after that
review. The sudo-grep secrets probe and the automatic `chmod 0644` are
deleted: a raw exit status cannot distinguish a sudo failure from a
grep no-match, and grep output can print secret values.

Choices weighed: (A) keep the probe but classify exits robustly and
redact output; (B) refuse automatic setup on uninspectable state. (A)
still chmods automatically after an inspection that can silently fail
and still routes file contents through grep. (B) costs one rare
automated repair and gains: no secret can reach the terminal, no chmod
runs on unverified content, and the failure mode is an instruction
instead of a guess. (B) wins.

Setup inspection returns a typed world. A missing custom config plans
the append step; an unreadable or malformed (non-UTF-8) custom config
fails the setup with the cause preserved — the append decision depends
on the inspection, so an inspection failure must not become "append
anyway". Global config metadata errors, non-files, and failed Nix settings
queries also stop automatic repair and retain the cause. Only a metadata
`NotFound` result is treated as a missing file. A required piped-stdin write
failure in `run_step` stops and reaps the child before returning the cause;
best-effort cleanup and infallible formatting keep ignoring results.

The importer seam returns a small public `ImportError`:
`SandboxRefused(String)` versus `Failed(String)`. The category is
produced by construction at the one refusal source (the local effective
config gate distinguishes a genuine refusal from a settings query that
could not be observed), not by matching text. The client preserves the
category through staging and offers its existing single retry only
after the consent-gated setup verified; an ordinary failure whose text
happens to contain the refusal phrase is formatted at the CLI boundary
and never triggers setup. Choices weighed: a client-side `CommandError`
variant (textual classification inside the seam, pollutes the general
error type) versus the typed seam error (one enum, two variants,
formatted only at the boundary). The typed seam error wins; the client
keeps a private two-variant staging error rather than growing the
command error taxonomy.

## 2. Tap staging and publication ownership

**Decision.** `SourceStore::new_staging` returns one owned
`StagedGeneration` (path plus guard) whose consuming `publish` settles
the guard internally before returning. `publish` returns a `Publication`
that borrows its originating locked store, owns the retained and scratch
state, and exposes exactly `finish`
(best-effort scratch cleanup) and `undo` (registry-failure rollback of
the first or replacing publication). `Drop` performs no fallible
rollback: an unfinished publication conservatively retains the old
generation and its scratch. The publication is refused if the staged
generation was created by another source's store (path containment and
provenance source are checked under the lock). `undo` takes no store
argument; its lifetime keeps the source lock alive through settlement.

Choices weighed: (A) keep the `(PathBuf, StagingGuard)` pair and move
the three command-layer helpers into the store as free functions;
(B) make the values own the protocol. (A) leaves the paired-values
hazard (an unrelated path or guard can be handed to another source) and
keeps the guard-settling rule in caller hands. (B) costs a breaking
interface change for callers that are all in this repository, and gains
one ownership protocol that cannot be half-executed. (B) wins.

Preserved invariants: the global registry lock is taken before the
source lock and held through registry commit and undo (command layer);
publication and registry commit stay two renames with the same crash
gap; the exchanged-out old generation stays at its final immutable path
immediately after the exchange; error messages and cleanup warnings
keep their exact fidelity; the importer's untrusted checkout never
becomes the flake root.

## 3. Typed catalog target decision

**Decision.** One internal `TargetDecision` holds the common version
and homepage plus one outcome: eligible (owning the typed plan) or
excluded (owning a small reason enum and optional detail). The summary
kind is derived from the plan at emission. The emitted JSON keeps the
exact field names, order, null behavior, and deterministic bytes of
`pkg-cask-catalog/4` through a manual `Serialize` implementation, so
the client decoder and every committed fixture stay valid.

Choices weighed: (A) keep the flat `TargetStatus` and add constructor
helpers that maintain the invariants; (B) make the malformed
combinations unrepresentable. (A) leaves `capture`/`emit` comparing
strings and reconstructing invariants; (B) is a structural change with
one serialization seam. (B) wins because every consumer is in this
repository and the byte contract is verifiable against the committed
catalog.

## 4. One isolated Nix environment constructor

**Decision.** One private constructor builds the isolated environment
and process setup shared by the config gate and the raw-export build:
cleared environment, fixed `PATH` (nix bin plus the system default),
`LC_ALL=C`, private `HOME` with `.config` and an empty
`NIX_USER_CONF_FILES`, private `TMPDIR`, null stdin, piped output. The
two callers keep their own roots, arguments, deadlines, log caps,
labels, and gates (the early local gate stays distinct from the fresh
authoritative in-build probe).

Choices weighed: (A) a shared session or home-framework object that
lives across both phases; (B) one pure constructor whose result each
caller applies to its own child. (A) adds a mutable shared abstraction
the audit explicitly rules out. (B) removes the duplicated
environment table with no shared state. (B) wins.

Each sentinel test starts a child test process with a cleared environment
and controlled `NIX_CONFIG`, proxy, and fake credential variables. A fake
Nix dumps its environment for the gate and the export build. The tests
check keys before values and account for shell bookkeeping on each host.
An isolation failure cannot dump host credentials.

## 5. Snapshot lane loading

Decided during implementation: the two lanes
(`native_lane`, `index_lane`) share identity resolution, snapshot
freshness, persistence, and the stale fallback; they differ in the
fetch operation, the payload type, and what they do with a payload
(rows versus platform skip plus filtered rows). One private generic
loader parameterized by the single fetch closure removes the
duplication without callbacks, traits, codecs, or registries. If the
resulting helper turns out larger than the duplication it removes, the
rejection is recorded here and the code stays.

**Decision (stage 5, evaluated read-only on 2026-10-01): REJECTED,
the code stays.** A closer read shows the shareable part is only the
head (identity → freshness predicate → eval_ref → live fetch →
conditional persistence); the tails are structurally different, not
just payload-different: the native stale fallback builds the lane
outcome directly, while the index stale and fresh paths both pass
through `index_apply`, whose platform rule distinguishes fresh
(skipped_platform, clean skip) from stale (Stale report with the
original failure) and whose `filter_catalog` has its own error path.
Sharing the head requires injecting at least three operations
(typed `read_native_snapshot`/`read_index_snapshot`, the
`nix.search`/`nix.catalog_index` fetch, and the distinct
`NativeSnapshot`/`IndexSnapshot` constructors) because the snapshot
structs are concrete types with distinct kind tags — i.e. the
callback-heavy shape the assignment rules out, for a saving of only
~40–50 lines per lane. It would also hide the freshness, persistence,
and provenance invariants (revision AND locked reference must match,
`None == None` is not proof, unversioned local sources never cached,
stale reuse labeled by the snapshot's own provenance) that each lane
currently states visibly and that tests pin, with a nonzero risk of
drifting the pinned report semantics. `cache.rs` already is the
intentional generic reuse point (`read_snapshot<P>`,
`write_snapshot<P: Serialize>`); the lanes are policy and only about
half identical. Cost exceeds benefit: rejected.

## 6. Quality and CI

Only obsolete comments that reference deleted gates are corrected — in
`clippy.toml`, `rust-toolchain.toml`, and the lint-policy header of
`Cargo.toml`; ADR 0005 stays superseded and unrestored. `unwrap_used`,
`expect_used`, and `panic` are added as individually selected
restriction lints at deny level in the inherited workspace policy; test
builds opt out once per crate root with a reasoned
`cfg_attr(test, allow(...))` (tests may fail fast on broken fixtures;
production denies explicit unwrap/expect/panic), and the three
integration-test crates, which compile without `cfg(test)`, declare the same reasoned header allow
once each. The real production sites are replaced on domain evidence:
the `checked_source` split becomes an explicit fallible path (no
validated source can lack a separator — now the code shows it), the
boundless arm of the range refusal returns explicitly instead of
`unreachable!` (a record with no bound declares no range), and the
`expect` on the lazily loaded tap state disappears by restructuring the
lazy load as a match that uses and caches the loaded state in the same
arm, so no impossible state is left to assert. The macOS smoke job
installs the Clippy component and lints all targets so `cfg(macos)`
code is checked; the installer smoke check gains the ShellCheck run
CONTRIBUTING already requires, on both matrix legs, with the runner's
own ShellCheck and an on-demand install fallback. `let _` for
infallible formatting, best-effort cleanup, and optional cache writes
stays allowed; no advisory fetching is added.
