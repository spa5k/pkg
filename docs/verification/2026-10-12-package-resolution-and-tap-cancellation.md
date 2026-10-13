# Package Resolution and tap cancellation verification

Date: 2026-10-12.
Follow-up: 2026-10-13.
Baseline: `1c1164b42052e09dd1dfa544987d5f38d100d81f`.
Scope: the working tree for [Deepen Package Resolution](../../openspec/changes/deepen-package-resolution/proposal.md).

Tap cancellation and Package Resolution are implemented.
The whole-repository 20% production Rust reduction is **not complete**.
The user withdrew this target on 2026-10-13 and accepted the current result.
Reduction work is stopped. The sections below retain the measured history.
Current product capabilities and safety checks remain required.
The [Rust 1.99.0 follow-up](2026-10-13-rust-1.99-upgrade.md) records the final compiler update.

## Behavior

- Tap imports retain SIGINT, SIGTERM, and SIGHUP through staging and command errors.
- Cancellation exits with 130, 143, or 129.
- Cancellation does not publish a tap generation or registry entry.
- A supervision failure retains its cancellation signal and original I/O cause.
- A cancelled profile check after a failed mutation supplies its signal to the exit status.
- Mutation cancellation stops further conflict discovery and retains supervision causes.
- Sandbox setup, staging validation, and Doctor probes retain cancellation.
- A cancelled host-version probe returns an error before an availability decision.
- Install suggestions retain cancellation while ordinary diagnostic failures stay optional.
- Supplied stdin is sent without blocking cancellation checks.
- Supplied stdin is borrowed and advanced without an extra input buffer.
- Incomplete stdin retains child cancellation, including a handled exit status of 130.
- Cleanup after an ordinary input failure does not count as cancellation.
- Only a typed sandbox refusal can offer setup.
- Package Resolution owns source selection, aliases, metadata, and install gates.
- Checked Catalog IDs use one representation with a borrowed kind.
- Validated Catalog Indexes own target, entry, and token lookups.
- Availability and information use the same recorded entry status.
- The full install batch passes its gates before profile mutation.
- Qualified requests retain source isolation.
- Bare cask requests count excluded entries when they check ambiguity.
- Upgrade views retain each full original Source Reference.
- Different Formula branches use separate Catalog Index views.
- Excluded cask information retains its homepage in verbose text.
- Offline search retains complete match counts and bounded result storage.

Native Nix commands and tap imports now use one process supervision module.
It retains signal forwarding, child ownership, group cleanup, wall limits,
output limits, and native discovery memory limits.
Native snapshot preparation requires immutable provenance.
Its unused transient publication path was removed.
Snapshot validation, locking, sync, and atomic publication remain.
Catalog snapshot and search-system cache writes share one atomic JSON writer.
Tap display metadata uses borrowed records without cloning the full record.
Sandbox policy checks and native log filtering use shared rule tables.
Upgrade preparation has one policy owner and retains its two-pass source checks.
Removed taps are checked before fixed-reference skip output.
Completion reports share their package output construction.
Native history is parsed once before rollback and prune use it.
Mutation failures share their heading and checked-state report.
The unused whole-table search renderer is removed.
The bounded search owner retains row rendering and the same empty-result text.
Staging uses the existing `TempDir` cleanup owner.
First publication removes scratch. Replacement retains the old generation.
Directory flush uses a file owner instead of a raw descriptor.
Source parsing and native metadata use borrowed values.
The generator reads JSON records directly instead of using a forwarding wrapper.
The official snapshot reads borrowed records and builds its final entry map directly.
Coverage counts now read the final entries.
The two cross-target count maps and their mutation logic are removed.
Target order, sorted reason counts, empty reason groups, and zero eligible counts remain.
All six focused emission tests passed.
Strict saved-tap loading uses the existing checked cache after every registered
source loads in order. Its extra keyed collection is removed.
Numeric version parsing relies on the integer parser to reject empty components.
Its ASCII digit and overflow checks remain.
The strict index decoder constructs the validated index directly.
Typed provenance retains its nonempty-field checks.
Tap list rows construct common fields once.
Native commands share argument, tracing, executable, and color setup.
Discovery retains its separate GC settings, bounds, and original failure arguments.

Tap capture constructs entries in its one conversion loop.
Origins share path syntax, SHA-256 syntax, and lowercase normalization.
Tap origins still require an exact tree path, matching hash, and token filename.
A focused regression keeps origin failure before duplicate-token failure.
All 19 focused capture and snapshot tests passed.

Effective Nix configuration uses typed settings and grants.
Object-only input, ignored metadata, missing/null rules, mandatory grants,
mapping override order, host restrictions, and the runtime probe remain.
Malformed shape errors use sanitized setup advice without config values.
They can precede semantic policy errors.
A temporary comparison matched 8,073 input and host decisions.
All 21 focused configuration tests passed.

One typed Catalog Index document owns decoded and encoded protocol fields.
The decoder validates typed entries in place and compares the declared target set once.
The stored target vector and serialized field order remain unchanged.
Duplicate-key handling and accepted record arrays remain unchanged.
The pinned `serde_path_to_error` 0.1.20 adapter keeps field paths in type errors.
Semantic refusal messages can change while retaining the source or entry identity.
A temporary comparison matched 156 acceptance and byte-serialization cases.
All 11 focused Catalog Index tests passed.

Search output and completion retain typed I/O causes.
Search JSON, database, and checked integer errors also retain their causes.
Repeated error-to-text conversions are removed.
Error text, ordinary exit status, output order, flush order, and storage bounds remain.
An early catalog error derive changed access to the shared host error cause.
It was first reverted while the source interface was held fixed.
The later reviewed derive uses the user's breaking-change permission.
It retains the same Arc, host object, and deepest I/O cause, as recorded below.

macOS constraints and artifact options use borrowed typed decoders.
Manual shape handling and intermediate string copies are removed.
Object-only constraints, one-string arrays, missing/null rules, unknown fields,
installer choices priority, pending-source pairing, and artifact order remain.
Numeric versions, coherent bounds, baseline checks, and path checks remain.
Malformed-input detail text and shape-error order can change.
A temporary comparison matched 254,282 old and new decisions and normalizations.
Accepted bounds and pairs, refusal reason classes, and minimum-OS details matched.
All 42 focused classifier and planner tests passed.

The unused public saved-catalog loaders are removed.
Package Resolution owns strict whole-set loading through its checked cache.
All four saved-catalog tests now use that command owner.
All 20 assertions remain, including missing and corrupt captures,
source isolation, excluded matches, and provenance binding.
Unused metadata getters, a config-query wrapper, a staging getter,
and the unused catalog attribute constant are removed.
The serialized metadata fields remain. Staging source ownership remains enforced.
Metadata tests use the serialized document.
The focused saved-catalog tests passed.
Both independent reviews found no semantic defect.
The moved test file's unused import was corrected before the successful checks.

Catalog checks reuse the existing SHA-256 rule.
IPv4 and IPv6 literal refusals share one diagnostic.
Domain suffix checks borrow the last label without collecting every label.
Summary kinds read artifact kinds without an intermediate vector.
Token validation uses the same ASCII grammar over borrowed bytes.
A temporary comparison matched 1,118,481 token decisions.
Another comparison matched 32,404 URL decisions and exact error details.
All 128 catalog library tests passed.

Process observation and RSS measurement read the retained child ID directly.
The redundant reverse conversions are removed.
The child ID already has the required unsigned type on macOS and Linux.
Construction still checks the signed PID used for signal operations.
Unreaped identity, signal forwarding, wait ownership, and resource limits remain.
All 40 process tests passed.
The independent Standards and Spec reviews found no semantic defect.
The full checks passed for this follow-up on macOS and Linux.

App errors share their context and cause construction.
The cause uses derived display and source handling.
The outer error retains the original native or process object through its private cause.
The final derived source interface is recorded below.
Cancellation, I/O causes, context text, and the Dock-work flag remain.
Two focused regressions check the original cause types and signal priority.
Saved-catalog loading constructs the checked result at its one loading site.
The repeated source check and private construction wrapper are removed.
The first source check still precedes registry origin and index reads.
Binding checks, reference checks, error context, and the read lock remain.
All four saved-catalog tests pass with their 20 assertions.
Both independent reviews found no semantic defect.
The full checks pass on macOS and Linux.

The maintainer CLI and tap importer share one import request.
The second set of request fields and five value copies are removed.
Flags, defaults, help text, and raw path bytes remain.
An actual-parser comparison matched 43 cases.
The standalone maintainer reporter now retains the typed import error.
It returns `128 + signal` for interruptions and cancelled supervision failures.
An isolated check injected only the importer leaf in temporary source.
It exercised the actual main, run, and import reporting functions.
All six cancellation cases returned status 1 with the previous reporter.
The new reporter returned 130, 143, or 129.
Ordinary status 1 and diagnostic text matched in all seven cases.
No test-only production interface was added.

Formula metadata reads its JSON fields by reference.
The two intermediate value clones are removed.
The repeated entry-source scan is also removed.
The validated index already binds each entry key and source to an input.
The one required `homebrew/core` input proves the same source rule.
A temporary comparison matched 1,568 decoding cases and exact error strings.
Aliases still fail before executables when both are invalid.
Cache bytes, identity validation, search ranking, and resolution remain.
The catalog tests passed.
The full workspace tests cover Formula search, aliases, and upgrade branches.
Both independent reviews found no defect.
The full checks for these latest changes pass on macOS and Linux.

Direct commands accept borrowed string arguments.
Their callers no longer construct intermediate owned argument vectors.
Path conversion, argument order, literal delivery, and NUL refusal remain.
The second app assessment still runs only after the signature check succeeds.
A temporary comparison matched 154 actual caller and supervisor cases.
Both independent reviews and the full checks passed on macOS and Linux.

Nix argument construction also borrows its prefix and caller arguments.
Profile paths retain their borrowed lossy text until command construction.
One shared classifier now constructs interrupted Nix outcomes.
Parent cancellation still wins over a resource limit.
A resource limit still wins over a child outcome when no parent signal exists.
The setup plan uses one Step constructor and adds stdin through a builder.
The second argument copy and comparison-only vector are removed.
Step summaries, argument values, stdin, consent, readiness, and retries remain.
Temporary comparisons matched 374 Nix cases and 1,440 complete setup plans.

Four static enum vocabularies use `strum_macros` 0.28.0.
The derived literal maps run without allocation or runtime case conversion.
All current reason, source kind, source status, and mutation action strings remain.
The actual old and new maps matched 32 literal checks and five JSON statuses.
Three permanent tests protect the 30 strings changed in this batch.
Future Rust variant renames must retain the public string or change it deliberately.
The lock adds two compile-time packages: `strum_macros` and `syn` 2.0.119.
Existing derives retain `syn` 3.0.6; no existing dependency version changed.
The prototype's assumption that every transitive dependency was already locked was incorrect.
Both independent reviews passed.
Full host checks for the Nix, Step, and enum changes passed on macOS and Linux.

Native discovery supplies its resource limits directly to the process module.
The unused metadata-limit factory and bounded-child forwarding function are removed.
Discovery retains Capture mode, no sink, 512 MiB RSS, two minutes of wall time,
and a 16 MiB limit in each captured stream.
The same supervisor retains enforcement, signal forwarding, group cleanup,
original I/O causes, and the native error classification order.
Profile replacement also retains borrowed lossy path text until command construction.
All process tests and both independent reviews passed.
The full checks passed on macOS and Linux.

The process monitor derives observed exit from its existing drain deadline.
The duplicate Boolean, unused restore result, and group-cleanup forwarding
method are removed. Signal recording still becomes inactive before restoration.
The caller still reads the retained signal after restoration.
Clock reads, first-error choice, group cleanup, and reap order remain.
A repeated private Boolean guard is also removed without changing stop calls.

Launcher status stores borrowed string slices at its existing collection sites.
The second conversion vector and list closure are removed.
Name order, diagnostic text, drift checks, filesystem reads, and assessments remain.
The existing Doctor regression checks the exact drift messages and absence of writes.

Catalog token lookup checks sorted validated source keys through exact record lookup.
It removes the full entry scan and temporary source set.
Excluded packages still count as choices, and source isolation remains.
A temporary comparison matched 512 decoded documents and serialized bytes,
and 12,288 source-choice results.
Native cache matching relies on the nonempty revision already checked by its read owner.
The repeated iterator limit is removed; the SQL query still limits jobs to 256.
A temporary comparison matched 4,608 header and match cases.
Source reference classification reads the scheme once and removes an ineffective
empty query-component filter. All 4,231 temporary classifications matched.

Target decisions write their shared protocol fields once after outcome selection.
The unused public detail getter is removed.
Its one test now checks the published JSON detail.
A temporary comparison matched 1,728 exact JSON byte strings,
including every exclusion reason, nulls, escaped strings, and eligible plan kinds.
Both independent reviews passed for this batch.
Full macOS and Linux checks passed for this batch.

Atomic exchange and exclusive rename use `rustix` 1.1.5, already in the lock file.
The directory, ownership, raw path byte, and NUL checks remain before the calls.
The checked flags select the same syscall operation on each host.
A missing operation fails with an OS error.
The calls cannot use an ordinary rename fallback with these flags.
Launcher backup, restore, and blocked recovery behavior remain.
Nonblocking pipe flags also use borrowed descriptors through `rustix`.
The Linux comparison returned the same pipe flags for the old and new calls.
Input budgets, poll order, delivery, cancellation, and reap behavior remain.

Path references now use `percent-encoding` 2.3.2, already present in the lock file.
The strict malformed-escape check remains before decoding.
An independent comparison checked 163,850 paths and escape combinations.
The old and new code returned the same result in every case.
Raw Unix bytes, UTF-8, uppercase escapes, and literal `+` characters remain.
Bounded GitHub diagnostic text uses the standard UTF-8 boundary operation.

The planner and classifier share one typed Catalog Exclusion.
The intermediate planner error vocabulary and conversion are removed.
Refusal order, reason codes, detail text, field order, and nulls remain.
Catalog bytes and coverage match the previous output for six pinned fixture records.

Typo matching now uses `strsim` 0.11.1, which was already in the lock file.
The 50-character candidate limit and maximum distance remain.
An independent comparison checked 529,984 ASCII and Unicode cases.
The old and new algorithms returned the same result in every case.
Focused tests cover Unicode, adjacent transpositions, and character limits.

## Standards

The independent review found lost I/O causes and lost cancellation on failed
supervision, setup, host-version, and install suggestion paths.
The findings were corrected.
Native and catalog errors now expose their cancellation signal to callers.
History reads, catalog preparation, installed-state reads, mutation reports,
setup checks, Doctor probes, and install suggestions use that signal before
they continue or select an exit status.

A follow-up review confirmed the input-error and suggestion fixes.
Tool-enforced lint findings were corrected through the normal checks.
The latest Standards review found no hard violation.
Its suggested upgrade collection name is corrected.
The next review found a lost signal during a failed mutation's profile check.
The report owner now uses that signal when the mutation has none.
The follow-up review found no remaining Standards issue in these changes.
The next Standards reviews found no defect in capture, typed settings,
command causes, or the typed Catalog Index decoder.

## Spec

The independent review found two behavior regressions in the refactor.
Formula upgrade views used a reference key that removed branch distinctions.
Excluded cask information lost its text homepage.
Both findings were corrected.

The new branch test fails with the old key.
The old key permits profile upgrade against the first branch's index.
The full reference key reads the excluded second branch and refuses mutation.
The mixed explicit upgrade test includes a fixed reference before a removed tap.
It checks that refusal precedes skip output and native mutation.
The latest Spec review found no new semantic defect.
It confirmed that the production reduction requirement remains incomplete.
The next review found that cancelled supervision could hide its I/O cause.
The report owner now prints that cause before returning its reported exit status.
The follow-up review found no remaining semantic defect in the reporting fixes.
The next Spec reviews found no defect in capture, command causes,
or the typed Catalog Index decoder.
The production reduction requirement remains incomplete.

The failure-state cancellation regression fails with the previous report owner.
It sends each signal only to pkg during the read after an ordinary mutation failure.
It checks the signal exit, unknown state, original cause, held lock, and final native call.
An isolated reporting test supplies a typed cancelled supervision error.
It checks the printed I/O cause and matching exit status for each signal.
The ordinary failure regression checks changed, unchanged, unreadable, and absent prior state.
A second CLI regression checks that the mutation signal wins over a later profile-check signal.
It checks that pure interruptions keep concise output.
The provenance test checks refusal for each of the seven required nonempty fields.
The typed decoder can change malformed-input diagnostic text and priority.

The isolated cleanup test changes SIGCHLD disposition after child readiness.
Supervision then fails with ECHILD after cancellation.
The test checks the retained signal and I/O cause for all three signals.
Command error tests check signal retention through native, catalog, app, and tap wrappers.

Setup step tests use no input and 5 MiB of input.
They check child SIGINT, SIGTERM, SIGHUP, and a handled exit status of 130.
The closed-stdin test checks a failed delivery and a reaped child without cancellation.
The process tests also send signals to the parent during a blocked input transfer.
They check complete input delivery and EOF for empty and 5 MiB payloads.

Tap integration tests send each signal only to pkg.
They check the exit status, child cleanup, and absence of a published registry.
Linux also tests cancellation in terminal sandbox setup.
Doctor cancellation is tested on both hosts.
Host availability tests distinguish cancelled probes from records that do not need a host probe.

Task S1 remains incomplete.
The current code reduction does not meet the requested 20% target.

## Production size

The baseline was measured before production edits.
The current measurement includes every workspace production Rust source tree:
`crates/pkg-cli/src`, `tools/cask-catalog/src`, and the new `crates/pkg-process/src`.
The baseline includes the former native process files.

The counter parses Rust token trees with `proc-macro2` 1.0.107 and source spans.
It excludes test files, fixtures, inline test items, comments, documentation
attributes, test-only attributes, and lint exception attributes.
It counts physical lines that contain retained Rust tokens.
It also counts tokens and group delimiters.
File moves and formatting receive no deletion credit.

| Measure | Baseline | Current | Removed | Reduction |
|---|---:|---:|---:|---:|
| Production code lines | 16,224 | 14,613 | 1,611 | 9.93% |
| Rust tokens | 119,149 | 108,585 | 10,564 | 8.87% |

The raw current line count is 14,551.
Two lines disappeared when an unused re-export's group collapsed.
Two more lines disappeared when the Formula input condition needed less wrapping.
Five more lines disappeared when borrowed codesign arguments needed less wrapping.
Two reference expression wrapping lines also receive no reduction credit.
Six archive argument wrapping lines and three artifact error wrapping lines
also receive no reduction credit. Three identity and prepared-lookup wrapping
lines and eight error-adapter wrapping lines also receive no reduction credit.
Six classifier error-wrapping lines also receive no reduction credit.
Eight index-visitor wrapping lines and two signal-array wrapping lines also
receive no reduction credit. One tap publication argument-wrapping line also
receives no credit. Two typed-probe match-arm wrapping lines also receive no
credit. Two earlier archive error-wrapping lines and one reporter condition
brace-wrapping line also receive no credit. Four native execution declaration
wrapping lines and eighteen host-policy wrapping lines also receive no credit.
Three requested-name iterator wrapping lines also receive no credit.
These seventy-eight lines are added back.
Five new setup wrapping lines, three profile-call wrapping lines, and four
buffered-drain wrapping lines are subtracted. Four new host-policy wrapping
lines are also subtracted. They are formatting growth, not code growth.
The net formatter adjustment adds sixty-two lines.
The reported line count adds them back to `catalog/mod.rs`, `catalog/brew.rs`,
`apps/assessment.rs`, `nix/reference.rs`, `tap/archive.rs`, `plan/mod.rs`,
`installed.rs`, `catalog/native.rs`, `commands/tap/staging.rs`,
`tap/config.rs`, `classify/mod.rs`, `catalog/local.rs`, `pkg-process/src/lib.rs`,
`commands/tap/mod.rs`, `nix/error.rs`, `nix/report.rs`, `nix/mod.rs`,
`commands/change.rs`,
and the total.
The reverse adjustment applies to `tap/setup.rs` and the total.
Test moves receive no reduction credit.

The line target requires at most 12,979 production code lines.
The current gap is 1,634 lines.
The token target requires at most 95,319 tokens.
The current gap is 13,266 tokens.

The [per-file measurements](data/2026-10-12-production-rust-size.tsv) include
removed files and new files. They show the process move without counting it as deletion.

Three broader Catalog designs identified entry lookup and cache duplication.
The direct lookup and shared cache writer changes are now applied.
The measured total includes those changes.
Native traversal, matching, source validation, bounded storage,
and atomic publication remain.
A separate launcher review found that the second assessment checks mutable
copies and helper results before publication.
Deleting those checks would remove required safety behavior.
Neither review established a capability-preserving path to the remaining 20% target.
The archive and network review found that their remaining checks enforce distinct policy.
Replacing them with generic unpacking or fetching would change those checks.
The output review removed one unused renderer and repeated reporting logic.
It found no material deletion from a generic serialization module.
A later process review found no material deletion from standard wait helpers
or borrowed poll descriptors. The required signal, child identity, pipe budget,
group cleanup, and resource rules remain.
A complete ProfileAction prototype deleted seven old profile methods and
updated eight production callers. It added 19 production lines and 15 tokens.
It was rejected.
The latest generation review removed duplicate coverage state.
A new wire schema had no material deletion after required adapters.
A later SearchSpool prototype moved fair section limits into SQLite.
All 6,875 allocation comparisons matched.
It added two production lines and removed 41 tokens, so it was rejected.
A shared capture-provenance prototype removed only three lines and 27 tokens.
Its extra optional-result state added indirection, so it was rejected.
Combining process phases would retain the required signal, drain, wait,
and group-cleanup rules in new transitions. It had no material deletion.
A checked Tap Source prototype retained each caller's separate input rules.
It added ten production lines and 21 tokens, so it was rejected.
A shared cancellation dispatcher retained the original cause contracts.
It added 15 production lines and 137 tokens, so it was rejected.
A shared publication review found only 46 common lines before adapters.
The remaining writers enforce different sync, mode, staging, and recovery rules.
The existing temporary-file module already owns the common file mechanics.
A later eager catalog decoder would change early architecture acceptance and
Formula dependency refusal priority. Staged adapters retain the same policy code.
A deferred catalog rules prototype retained early architecture acceptance and
refusal priority by storing separate shape results. It added 37 production
lines and 324 tokens, so it was rejected.
A pinned low-level Nix bindings prototype retained raw signal installation and
Darwin observation where the bindings cannot meet the existing contracts.
The valid subset removed only 16 lines and 36 tokens. It added a dependency
and left the process policy intact, so it was rejected.
A Darwin process-inspection library does not check full PID buffers or full
structure reads. It also loses I/O causes. Restoring those checks removes its
deletion benefit. Signal recording, pipe fairness, and administrator actions
remain separate required rules.
The proposed removal of a second ANSI normalization pass was rejected.
The actual reporter functions retain a nested escape in one accepted input
when the second pass is removed. Ordinary colored output alone did not expose it.
The Catalog System object conversion remains. A straight derived decoder would
accept positional section arrays. A flatten adapter needs more code.
The first TraceDecay status check reports a current project index. Its graph includes
995 files across production, tests, fixtures, and other languages.
A scoped zero-caller query includes live Rust methods, trait methods, derive
callbacks, and qualified calls. Direct source checks found live callers for
the remaining production function candidates. Those graph results do not
establish unreachable production code and receive no deletion credit.
Later complexity queries report the prior complete graph generation while the
index rebuilds after this batch. The size and host checks use the actual files.
The reduction task remains open.

The tap export runtime now exposes its actual bounded export function directly.
The unused forwarding function and output type alias are removed. The importer
passes the same named 45-minute limit. The four runtime tests call that same
production function with their existing limits and assertions. The runtime body,
asset writes, arguments, environment, canary cleanup, causes, and cancellation
order remain unchanged. This removes 18 production lines and 57 Rust tokens.
Both review axes passed. Full macOS and Linux checks passed. All 177 source
hashes matched on both hosts. Linux logs were downloaded before sandbox cleanup.

Repository verification has one commit-response path for default and pinned
revisions. Repository identity and safe default-branch checks still run first.
Slash-containing branches still use HEAD. Exact pin matching, lowercase output,
diagnostic text, and the 400-byte API error bound remain. A temporary comparison
ran the actual before and after repository functions against a queued HTTP client.
All 64,278 results and ordered requests matched.

Setup inspection still reads the entire custom config as UTF-8 and retains its
read failures. It removes the unused substring result. The sole production caller
always replaces that result with effective Nix settings before an executable plan.
An unreadable global config still refuses all steps first. The private readiness
field now records a Boolean. Existing tests use that field's current meaning.
Daemon environment inspection records three valid states: no environment
dictionary, a dictionary without TMPDIR, or a dictionary with TMPDIR.
This replaces two Boolean fields that could also describe an impossible state.
A temporary comparison matched all 864 complete setup plans.
Source refusal has one branch for its two identical conditions. Source labels
truncate their existing normalized buffer at the second slash. A temporary check
matched 112,839 actual label, parsed-ID, and exact diagnostic results.
This batch removes 27 production lines and 143 Rust tokens. Both independent
review axes passed. The first Clippy run found four Boolean fields in the setup
observation. The three-state environment field fixes that issue. Full macOS and
Linux checks passed. All 177 source hashes matched on both hosts. Linux logs were
downloaded before sandbox cleanup.

Atomic rename calls pass borrowed paths to the existing Rustix implementation.
The custom C-string converter and repeated C-string construction are removed.
Raw Unix bytes, NUL refusal before mutation, directory checks, rename flags,
recovery, and flush order remain. Invalid-NUL diagnostic wording can change.
Temporary comparisons matched 256 filesystem cases on macOS and 512 on Linux.
Archive extraction retains its checked entry limit before every unpack call.
Each regular file is one counted entry, so the second file limit and overflow
check cannot trigger. The success count and empty-archive refusal remain.
All four actual archive tests passed before and after the change on both hosts.
All byte, path, type, PAX, and collision checks remain.
This batch removes 30 substantive lines and 295 tokens. Both review axes and
full macOS and Linux checks passed. All 177 source hashes matched. Linux logs
were downloaded before the next source change.

Artifact source validation uses one accepted-path predicate and one malformed
source detail. The accepted path set and refusal class remain. Temporary checks
matched 7,174,964 path cases and 16,344 actual classifier decisions. Eligible
catalog bytes and non-malformed refusal details remain exact. Only malformed
path detail wording changes. This removes eight substantive lines and 50 tokens.
Both review axes and full macOS and Linux checks passed. All 177 final source
hashes matched on both hosts. Linux logs were downloaded before sandbox cleanup.

Further architecture prototypes did not meet the deletion test. Shared producer
and client provenance types removed four lines but added 54 tokens across all
callers. Their 148 client cases and four producer byte cases matched, but their
generic interface added knowledge for little benefit. The source and macOS
bound handoff removed no net lines. These prototypes are not applied.
Native discovery, exact lookup, launcher recovery, and tap publication retain
different state and ordering rules. Sharing those rules did not produce a
material reduction without an extra adapter. The 20% task remains open.
The final documentation check passed for 202 Markdown files and 490 local links.
Two stale links in archived OpenSpec documents were corrected. The active
proposal, design, and task files retain S1 as an open task.

The next follow-up removes two private error adapters. The config gate returns
its existing ImportError. Staging returns its existing CommandError and matches
the nested typed sandbox refusal. Only an observed config refusal offers setup.
Text lookalikes, query failures, cancellation, and validation failures do not.
Exact diagnostics, original causes, and stage/setup order matched in 43 gate
cases and 3,888 retry cases. Existing tests retain their refusal, detail, and
cause assertions. Three changed config test paths were corrected before checks.
This removes 32 substantive lines and 297 tokens.

Completion returns launcher names and final text from its existing owner.
The parallel Completion state, second dispatch, and name-printing forwarding
function are removed. Success text still prints only after app refresh succeeds.
Changed-entry selection and install-only command discovery retain their order.
All 1,232 temporary actual-method cases matched output, names, and discovery
calls. The text buffer remains in memory during app refresh. This removes
35 production lines and 103 tokens. The first Clippy check found an import after
statements. Moving that import to the file header corrected the finding.

Installed identity lookup shares its canonical source and decoded attribute.
All 53,460 comparisons matched, including overlapping source settings and
Nixpkgs, Formula, and Cask priority. Current snapshot lookup uses its existing
prepared interface at both read positions. All 18 actual calls matched, including
a child blocked on the real lock that then reused the replacement snapshot.
Immutable identity, lock, reread, staging, and publication order remain.
This removes three substantive lines and 45 tokens after wrapping discounts.
Both independent review axes and the full macOS and Linux checks passed for
these three changes. All 177 final source hashes matched on both hosts. Linux
logs were downloaded before sandbox cleanup. The 20% target remains open.

A later process I/O array prototype removed eight lines and 14 tokens across
the complete source. It replaced named stdout/stderr state with positional rules
and added conversion adapters. The alternative record design added seven tokens.
Both were rejected. A complete production function inventory found only required
Serde callbacks among names without a direct source call. It established no dead
helper to delete. These read-only checks receive no reduction credit.

The bare-name resolver now exposes its existing implementation directly.
Package Resolution supplies the native Source Reference and cache path for a
bare name. It supplies no native lane for a bare cask. The two forwarding
functions are removed. Saved Catalog loading still runs first. The exact lookup
uses its resolved Source Revision directly. Its one-caller forwarding function
is removed. The resolver body is byte-identical. The exact lookup body differs
only in the required identity borrow. All production callers and the existing
failure test use the current interface. This removes 44 lines and 233 tokens.

Catalog classification now has one entry point with the existing PlatformGate.
Official records supply Tags. Tap capture supplies the checked upstream Boolean.
The two forwarding functions are removed. One borrowed string-array validator
serves platform and dependency checks when their existing stages are reached.
It returns the original array slice and adds no successful-path allocation.
Platform tags use the existing target mapping. Unknown dependency keys still
precede Formula and Cask checks. Early architecture acceptance remains.
All 146,268 actual-function comparisons matched refusal classes, eligible JSON,
and non-malformed details. The combined temporary run passed 45 tests, including
the existing classifier and fixture tests. All 50 existing test calls retain
their assertions. No permanent test seam was added. Only malformed detail text
changes. The initial borrowed-string vector design was rejected because it
added two unnecessary allocations. The first full Clippy check found the
obsolete private malformed-result helper. Both reviews confirmed it had no
callers. It is removed. The final classifier change removes 27 substantive
lines and 151 tokens after six wrapping discounts.

Both independent review axes and the full macOS and Linux checks passed for
these changes. This batch removes 71 production lines and 384 tokens.
All 177 final source hashes matched on both hosts. Linux logs were downloaded
before sandbox cleanup. The 20% target remains open.

The shared process module now exposes its actual run owner directly.
The three forwarding functions are removed. Runtime adapters supply their same
stdio mode, sink, limits, and optional borrowed input. Setup passes its optional
input directly and removes its input dispatch. Product command callers keep
their existing runtime interfaces. The private Nix execution owner also replaces
its no-sink forwarding method. The supervision body is byte-identical. Both
actual source versions passed all 40 existing process tests in temporary builds.
The complete actual process, importer, and client libraries passed temporary
type checks. Existing test functions and assertions remain. No permanent test
seam was added. The wider process interface adds absent arguments inside the
runtime adapters. Its public documentation now states that output limits apply
to captured output and memory checks require a configured `resident_bytes`.
Both independent reviews passed after this documentation correction.
Full macOS and Linux checks passed. All 177 final source hashes matched on both
hosts. Linux logs were downloaded before sandbox cleanup. This removes 18
production lines and 181 tokens, including all production caller costs. It has
no formatter credit. The 20% target remains open.

Package Resolution no longer re-encodes a cask identity after its strict
inverse and exact three-segment check. The final segment cannot contain a
literal dot. Every accepted underscore escape re-encodes to its original bytes.
The existing source and token checks remain. Both installed-entry guards remain:
their `splitn(3)` tails can contain literal dots. The temporary comparison ran
7,775,644 actual before/after cases and 304,111 strict inverse cases.
Both independent reviews passed. This removes three production lines and
20 tokens without formatter credit. The full macOS and Linux checks passed.
All 177 source hashes matched on both hosts. The first Linux process-entrypoint
run failed when a fixture executable returned `Text file busy`. The repeat
passed with no source change. Its first-run and passing logs were downloaded.
The final cask identity Linux logs were also downloaded before sandbox cleanup.
The 20% target remains open.

The retained child owner now uses one wait and state-update implementation
for Exited and protected Reaping states. Cleanup reaches one reap completion
loop. Group signals still precede the first wait for an observed exit. Running
cleanup still retains child identity until force-stop and enters Reaping before
wait. Reaping, Reaped, and Lost states cannot send signals. ECHILD, other wait
errors, retry delays, and terminal states retain their rules. The remaining
supervisor body is byte-identical. Both actual temporary source versions passed
all 40 existing macOS process tests. Both independent reviews passed. This
removes 16 production lines and 68 tokens with no formatter credit.

ANSI removal now uses one private, fixed compiled CSI pattern from the existing
locked regex dependency. Each call consumes ESC followed by `[` through the
first ASCII letter, or through the end of an incomplete sequence. Other escape
bytes and Unicode text retain their bytes. Repeated normalization calls remain
in place because normalization is not idempotent. All 4,560,367 actual before
and after comparisons passed, including every Unicode scalar in four contexts
and malformed sequence combinations. The validated fixed pattern uses one
reasoned impossible-failure exception. No dependency is added. Both independent
reviews passed. This removes 11 production lines and 40 tokens. It has no
formatter credit and makes no measured performance claim.

Signal disposition restoration now calls one private deactivation sequence.
Its three callers keep the exact SeqCst operations, handler wait, restoration
order, error priority, and Drop retry. Expanding the helper reproduces the prior
source byte for byte. Two one-caller pipe-reader forwarding methods are removed.
Their exact read calls remain inside the OutputPipes owner. Stdout still precedes
stderr, and stdout overflow still returns before stderr and stdin. The combined
actual source passed all 40 unchanged macOS process tests. Both independent
reviews passed. This removes nine production lines and 110 tokens, including
all helper and caller costs. It has no formatter credit.

Both batches passed the full macOS and Linux checks. All 177 final source hashes
matched on both hosts. The first final hash check for the nine-line Linux batch
used the prior batch's manifest and failed. Its behavior checks passed. The
manifest path was corrected, and the complete script passed again without a
source change. The passing logs and first manifest failure log were downloaded
before sandbox cleanup. The 20% target remains open.

Setup records whether its fixed Determinate plist is present. It no longer
stores an owned copy of that same constant path or converts it back to text.
Plan and probe arguments retain the exact constant bytes. All 432 actual plan
comparisons matched commands, input, summaries, and manual refusals. Full
library and unit-test type checks passed. This removes two substantive lines
and 54 tokens. The raw line count adds three lines because formatting adds
five net wrapping lines; this growth receives no substantive credit.

Launcher destination derivation retains the absolute, non-root HOME check.
The later comparison of its parent with `/Applications` is removed because
Unix lexical component equality makes it unreachable after that check. Dot
and repeated-separator forms of root are already refused. ParentDir components
do not cancel lexical components. This check performed no filesystem resolution.
Ownership, symlink, parent UID, staging, exclusive rename, and recovery checks
remain. All 2,700,941 actual path comparisons matched exact values and errors,
including Unicode, non-UTF-8 bytes, and seven-component path combinations.
This removes four production lines and 35 tokens, with no formatter credit.

The Package ID parser has one full cask identity branch for prefixed and
unprefixed inputs. Prefixed bare tokens still return through their existing
validator first. Brew, Nixpkgs, and GitHub precedence remains. Prefixed cask
inputs containing `#` or `://` remain cask refusals. Only the malformed full
cask diagnostic is normalized. All 1,374,906 actual comparisons matched accepted
values and refusal classes. Other error text is exact. All six existing tests
passed before and after. This removes five production lines and 25 tokens.
It has no formatter credit.

Both independent reviews and the full macOS and Linux checks passed for all
three client changes. All 177 final source hashes matched on both hosts.
The Linux logs were downloaded. This removes 11 substantive production lines
and 114 tokens. The 20% target remains open.

Availability and cask visiting now accept the validated Catalog Index directly.
Saved Catalog visits do not construct or clone a full Catalog View. Snapshot
visits use their same locked-reference fallback and payload. Full views remain
where Source Reference, Source Revision, or upgrade metadata is required. All
15,804 actual comparisons matched rows, order, rank, age, diagnostics, partial
sink failures, cancellation, and original causes. This removes two substantive
production lines and 30 tokens. Eight collapsed wrapping lines receive no credit.

The three signal counters use one const array initializer. They remain three
independent atomic objects with the same zero values. All other process source
and tests are unchanged. All 40 existing process tests passed. The independent
storage probe also passed. This removes two duplicate initializer lines and
12 tokens. Two collapsed array wrapping lines receive no credit.

Both independent reviews and the full macOS and Linux checks passed for these
changes. All 177 final source hashes matched on both hosts. The four Linux logs
were downloaded before sandbox cleanup. This removes four substantive
production lines and 42 tokens. The 20% target remains open.

The full cumulative review found three more gaps. Cancelled supervision could
hide the original I/O cause in rollback, normal postflight, and prune postflight
reports. Their existing report owners now use the same cause decision. Only a
pure Interrupted error stays concise. Prune postflight retains its history
heading and next instruction. A regression exercises the actual report functions
in 24 cases: three signals, four report paths, and pure interruption or failed
supervision. The new assertion fails with the prior postflight report.

The typed tap export decoder had changed object and duplicate-key acceptance.
Export validation now normalizes the bounded bytes through the JSON Value owner
and requires objects at both the root and files field before typed decoding.
The original last-key-wins rule remains. Two new tests fail with the prior typed
decoder. Ten duplicate-field cases check the consumed casks and exact byte hash.
Root and files positional arrays stay refused. All four envelope tests pass.

Background job commands now retain typed I/O, publication, process, interruption,
and schedule-context errors through CommandError. Schedule context retains its
original source. Optional launchd controls tolerate ordinary failure but propagate
cancellation before later work. A real-child unit checks all three signals and an
ordinary failure. Linux tests enable, disable, and status for all three signals.
All nine cases check the signal exit, diagnostic, one control call, retained job
files, and absence of completion output. The same Linux test fails with the old
background and command-error owners: SIGINT during enable exits 1 rather than 130.
Command-wrapper tests also retain each signal and the deepest original ECHILD cause.

An earlier ConfiguredProbe source narrowing to Box<Self> was first rejected.
The unchanged native version-probe unit could no longer downcast the immediate
source directly to NixError::Failed. That pass kept the old Rust source interface
and restored erased storage and signal lookup. Its temporary evidence and
150-unit macOS check remain historical results. The later reviewed change below
uses the user's permission for breaking Rust interfaces. It retains the original
native object and adapts the source-interface assertion explicitly.

The correctness repairs add 47 production lines and 358 tokens. Tests receive no
size credit. Current capabilities and safety rules remain required. The line and
token targets remain open. The new architecture review of the remaining generator
modules found no candidate with a net deletion of 100 production lines. Its actual
function checks confirmed distinct artifact refusal phases and priority. The
search and command review also found separate selection, output, and loading rules
that a shared traversal must retain.
A later review checked eight remaining policy owners with 878 production lines
and 7,042 tokens. No candidate removes 100 net lines while preserving their
approval, publication, lock, health, conflict, strict JSON, and network rules.
Actual-source tests confirm that ordinary JSON decoding would accept nested
duplicates refused by the shared strict decoder. No change from that pass is applied.

The full macOS checks pass after these repairs. Linux first stopped at an old hash
in its verification manifest after an upload could not overwrite the remote file.
A fresh manifest corrected that setup error. Linux Clippy then found unnecessary
raw-string markers in the Linux-only CLI test. Removing the markers preserves the
fixture bytes and changes no production code. The complete Linux script passed
with the final test source. All 177 final file hashes match before and after the
Linux checks and in the local tree. All four real native discovery tests pass on
both hosts. Native CLI checks pass four scenarios on macOS and two on Linux. The
passing Linux logs, initial manifest and Clippy failure logs, and expected old-code
regression failure log were downloaded before the sandbox stopped.

The one-caller platform signal helper is removed. Native signal observation
and the exit-code-130 fallback now live in the same classifier. The actual old
and new functions agree in 1,048,576 raw-status comparisons. Unix signal priority
and the non-Unix fallback stay the same. Both independent reviews pass. This
removes 11 substantive production lines and 41 tokens. No wrapping adjustment
is needed. All 40 macOS and 39 Linux process tests pass. Workspace Clippy with
all targets, all features, and denied warnings passes on both hosts. Format,
diff, and process Rustdoc checks pass on macOS. All 177 source and check-file
hashes match on both hosts. The Linux logs were downloaded before sandbox cleanup.
The prior full workspace and native checks remain valid for the unchanged callers.

Three additional proposals were rejected. Common text availability rendering
matched the actual output in 8,224 cases, but removed only two lines and added
four tokens. A waitid replacement through the already locked Rustix library
passed all 40 Darwin process tests and 1,152 paired native wait observations.
It removed seven raw lines and 15 tokens. Eight lines came from syscall wrapping,
so conservative accounting gives one added substantive line. Neither proposal
is applied. A catalog matching review measured 966 production lines and 7,505
tokens across matching, search, and native discovery. It found no 100-line net
reduction. Ranking already has one owner. Exact selection, alias visibility,
bounded discovery, cancellation, and publication retain distinct rules.

CatalogError now uses the existing error derive for formatting and sources.
Its Rust source interface exposes the existing Arc<HostVersionError>. The user's
breaking-change permission permits this interface change. The same host object
remains inside the Arc. No production caller depends on the old immediate type.
All 862 message and signal comparisons match. The three existing command-cause
tests pass before and after their interface update. Thirty-nine actual command
cases retain the same Arc and host object. All 28 Process cases retain the exact
same deepest I/O object, kind, errno, and text. The eight-choice bound, ordering,
and footer condition remain. No wrapper or dependency is added.

AppError also uses the existing derive. Its source exposes the existing
AppErrorCause, then the original native or process error. The constructors,
Display, cancellation, and Dock flag remain. Actual before and after code passes
75,264 message, signal, and Dock comparisons, 21,504 original-object identity
comparisons, and 39,424 deepest-I/O comparisons. The two existing cause tests
follow the new source step. No production caller depends on the old immediate
type. No production test seam is added.

Full Clippy found two temporary strings in the first choice renderer. Direct
writes to its String remove those allocations. The initial macOS and Linux
failure logs are retained. The repaired derives remove 27 raw production lines
and 264 tokens together. Test changes receive no credit. Their repeated full
macOS and Linux verification passes. Both hosts pass workspace tests, strict
workspace Clippy, four real Nix discovery checks, and their native CLI checks.
macOS also passes the full build, private Rustdoc, format, diff, and dependency
checks. All 177 source and check-file hashes match before and after verification.
The Linux logs were downloaded before the sandbox stopped.

The process monitor shares its four force-stop and first-error sequences in one
private method. The I/O-error branch retains its pre-close flag assignment.
Branch, pipe-close, signal, and error order remain. Both full copied modules
compile with denied warnings and pass all 40 unchanged macOS process tests.
Both reviews pass. This removes five substantive lines and 38 tokens.

Tap staging now owns its SourceStore and exclusive lock. Publication consumes
the stage and retains that same store. A caller can no longer supply a different
store for the stage. The repeated source copy and foreign-stage runtime check
are removed. Provenance, binding, real-directory, sync, exchange, and undo checks
remain. The staging guard drops before the store on failure. Publication keeps
the lock through finish and undo. CLI rollback still holds the registry lock
through reporting and return. Both source reviews pass. All 10 existing store
tests and four saved-catalog tests pass before and after. The final store has one
additional lock-retention regression, and all 11 tests pass. A temporary lock
probe passes three checks. Reversing field order fails its cleanup check as
expected. The Rust interface changes; product commands and capabilities remain.
This removes 11 substantive lines and 88 tokens after one wrapping exclusion.
The combined batch passes the full macOS and Linux checks. This includes strict
workspace Clippy, locked workspace tests, four real Nix discovery checks, and
each host's native CLI checks. The client has 151 passing unit tests on macOS
and 152 on Linux, with the four discovery tests run separately on both hosts.
macOS also passes the full build, private Rustdoc, format, diff, and dependency
checks. All 177 source and check-file hashes match before and after the checks.
All five Linux logs were downloaded before sandbox cleanup. The repository's
documentation checker passes: 243 Markdown files and 506 local links. S1 remains
open; the result does not meet the 20% target.

A command orchestration review found no further substantial deletion across
1,467 production lines and 11,458 tokens. The shared mutation sequence is
already 20 lines. Its remaining stages retain distinct source, generation,
cancellation, and failure rules. A shared app collector adds one production
line and 23 tokens, so it is rejected without a behavior comparison or source
change. Neither review receives reduction credit.

A blind cancellation source walker remains rejected. It would bypass current
CacheWrite and ConfiguredProbe signal decisions. Retaining those decisions would
remove the projected simplification. Its cycle checks are abstract model tests,
not Rust integration verification. No source walker is applied.


The guarded source client borrows its fixed host allowlist directly. The owned
set and the per-request vector are removed. Its existing constant-only caller
is unchanged. URL admission and refusal classes match in 157,200 temporary
cases; all six existing network tests pass before and after. DNS resolution
order, pinned addresses, proxy refusal, redirects, body limits, and time limits
remain. Outside-host diagnostics now retain input order and duplicates rather
than a sorted, unique list. The Rust constructor requires a static outer slice.
This removes three production lines and 54 tokens.

Configuration source and display values now have one owner in their existing
Default implementations. Four scalar forwarding functions are removed.
The three top-level Config field defaults remain. Runtime uses its existing
Default for a missing nix value. All 9,866 decoding comparisons and 559 exact
serialization comparisons match, including malformed and unknown fields.
All 256 XDG cases and the three unchanged config tests pass. Complete Config
decoding adds four allocations and 114 requested bytes; a partially supplied
Sources section adds one allocation and 37 bytes. Missing files, empty config,
empty tables, runtime-only config, and unknown-field refusal add no allocation.
This is a bounded startup cost, not an allocation reduction. The public scalar
helper functions are removed under the breaking-change permission. Product
settings, defaults, paths, loading behavior, and color policy remain.
This removes 20 substantive lines and 100 tokens without formatting credit.

Three macOS constraint failures use the existing malformed-record constructor.
It forwards the same reason and detail to the same existing implementation.
Conditions, evaluation order, and return points remain. This removes three
production lines and 18 tokens. Independent reviews pass for all three changes.
The combined 26-line, 172-token batch passes the full macOS and Linux checks.
Both hosts pass locked workspace tests, strict Clippy, four real Nix discovery
tests, and their native CLI lifecycle checks. macOS also passes the full build,
private Rustdoc, format, diff, and dependency checks. All 177 source and check
hashes match before and after both runs. All six Linux logs were downloaded
before the sandbox stopped. The documentation checker passes: 243 Markdown
files and 506 local links. S1 remained open at 9.34% line reduction and 8.36%
token reduction. The complete product capabilities remain required.

A whole-cluster filesystem diagnostic result helper would remove 52 tokens,
but its 17 raw line savings consist of 30 shorter wrapping lines minus 13 new
trait and import lines. After formatting exclusions, it adds 13 substantive
lines. A smaller formatter adds 74 tokens. Both variants are rejected. Neither
receives production reduction credit.

Two homogeneous output-pipe designs are rejected. Named slots add 52 Rust tokens;
tuple slots add 22. File-descriptor conversions and the distinct channel rules
consume the projected savings. Neither variant was executed or applied.

The process crate now declares its Unix boundary once. One crate guard replaces
21 repeated item and expression guards. All implementation statements, item
visibility, OS-specific branches, and test lint allowances remain. The active
macOS and Linux source is byte-identical after removing true guard metadata.
All 40 unchanged macOS process tests pass before and after. Unsupported non-Unix
targets now produce an empty crate instead of the existing unresolved run_child
compilation failure. No non-Unix binary was compiled. This removes 20 production
lines and 139 tokens through compile-time policy consolidation. No runtime
algorithm deletion is claimed.

The catalog generator has one private envelope and one final entries map.
The flattened private Provenance wrapper and temporary token map are removed.
Moved fields receive no credit. Field order, JSON bytes, coverage, nulls, newline,
sorted entry order, and all integrity refusal text remain. All 1,733 actual
generate and assemble comparisons match. Each frozen variant passes all 131
unchanged generator tests. Valid prefix records now undergo pure classification
before a later invalid or duplicate token is refused. A duplicate also allocates
its qualified ID before refusal. This changes rejected-input work, with no I/O
or publication effect. This removes 10 production lines and 70 tokens.

ConfiguredProbe stores Box<NixError> instead of an erased error box. Its sole
production constructor already supplies that native type. Signal lookup calls
the typed source directly. The original Nix error, RunError, and I/O objects
remain. All 27,040 message, signal, source-text, and cause comparisons match;
pointer checks retain the original objects, I/O kinds, errno, and text. The
immediate Error::source now exposes Box<NixError>; Box::as_ref gives the original
native error. Non-Nix probe sources are no longer constructible. These are
authorized Rust interface changes. The real version-probe assertion checks the
new box explicitly. The downcast removal saves 25 tokens, with zero line credit
after excluding two match-arm wrapping lines.

The full-commit validator is a direct alias to the existing catalog predicate.
The forwarding function is removed. Its name, signature, sole production caller,
local documentation, and target's must_use behavior remain. The unused-return
Rust warning gains the target's existing explanatory note. Product diagnostics
remain. Existing reference, tap revision, real version, and command-cause tests
pass with the combined client sources. This removes three production lines and
16 tokens. Both independent review axes pass for the full batch.

The combined batch removes 33 credited production lines and 250 tokens.
Full macOS and Linux verification passes for the newly applied tree.
Both hosts pass strict locked Clippy, workspace tests, four real Nix discovery
tests, and their native CLI lifecycle checks. macOS also passes format, diff,
workspace build, private Rustdoc, and dependency checks. All 177 source and check
hashes match before and after both runs. All six Linux logs were downloaded
before the sandbox stopped. The documentation checker passes: 243 Markdown
files and 506 local links.
S1 remained open at 9.55% line reduction and 8.57% token reduction.

Filtered native mutations pass their existing profile text to the reporter.
The four callers already construct the same first `--profile` argument.
The private runner no longer scans that argument vector to recover the profile.
The reporter has one constructor that requires the profile and verbose flag.
Its factory, configuration builder, and unreachable missing-profile state are
removed. The terminal query, timer and work initialization, profile allocation,
field drop order, both ANSI normalizations, and all stream handling remain.
Both full copied client sources compile with warnings denied. All 13 unchanged
runtime, reporter, and command-cause tests pass before and after. All 52 actual
child-process comparisons match arguments, environment, reports, signals, and
original error objects. Both independent review axes pass for each step.
This removes 13 substantive lines and 85 tokens. The reporter still owns one
profile String; no allocation reduction is claimed.

The private bounded archive reader owns its actual memory gzip decoder.
Its final drain uses the standard copy operation through a 64 KiB buffer.
The buffer keeps the original read size and first-error priority. The generic
private reader interface is removed. Standard copy retries Interrupted errors;
the concrete memory decoder cannot produce that error. All archive limits,
path, collision, PAX, permissions, damage, and 512-byte alignment checks remain.
Pinned standard-library and decoder sources establish the same drain path on
macOS and Linux. All 324 actual extraction comparisons match exact results,
errors, trees, modes, and inventories, including nine production-limit cases.
Both full copied generator sources compile with warnings denied and pass all
131 unchanged tests. Both independent review axes pass. This removes six
substantive lines and 13 tokens. One 64 KiB heap buffer remains.
Plain copying remains rejected: its smaller reads change the first error for
a corrupt stream that also exceeds the decoded limit.

The combined follow-up removes 19 substantive lines and 98 tokens.
Full macOS and Linux verification passes for the applied tree.
Both hosts pass strict locked workspace Clippy, workspace tests, four real Nix
discovery tests, and their native CLI lifecycle checks. macOS also passes format,
diff, workspace build, private Rustdoc, and dependency checks. All 177 source and
check-file hashes match before and after both runs. All six Linux logs were
downloaded before the sandbox stopped. The documentation checker passes:
243 Markdown files and 506 local links.
At this batch, S1 remained open at 9.66% line reduction and 8.65% token reduction.

## Source and stdio ownership

Catalog routing owns the existing Sources record instead of repeating its three
fields. The source clones, allocation order, owned independence, and tap-map move
remain. Full copied client sources pass all 17 existing affected tests before
and after. All 580 decision comparisons and 54 allocation and drop comparisons
match. Both independent review axes pass. The Rust field paths and derived Debug
layout change. This removes four real production lines and 24 tokens.

Filtered IoMode owns its required mutable sink borrow. The separate optional
sink parameter and stored optional sink are removed. Finishing retires the borrow
before the same callback. Valid Capture, Stream, and Filtered call paths retain
their stdio handling, byte and time limits, cancellation, child identity, group
cleanup, and first-error order. All actual callers compile before and after.
All 40 existing process tests pass in both copied versions. Both independent
review axes pass. The Rust enum loses its value and thread-safety traits; no
production caller requires them. Its stored enum is eight bytes larger than the
former mode and optional-sink fields. No memory reduction is claimed. This
removes nine real production lines and 29 tokens after excluding four declaration
wrapping lines.

Host path validation selects its policy from the checked operating system.
One ordered path loop and one impure-dependency loop replace the repeated host
loops. The private correlated boolean argument is removed. Grant merging,
settings and flag checks, unsupported-host priority, exact refusal details, and
the mandatory runtime sandbox probe remain. All 21 existing configuration tests
pass before and after. All 83 public-function comparisons match. Both independent
review axes pass. This removes nine real production lines and 25 tokens after
excluding a net fourteen wrapping lines.

The three changes remove 22 real production lines and 78 tokens.
Full macOS and Linux verification passes for this applied batch.
Both hosts pass strict locked workspace Clippy, workspace tests, four real
Nix discovery tests, and native CLI lifecycle and cancellation checks. macOS
also passes format, diff, workspace build, private Rustdoc, and dependency
checks. All 177 source and check-file hashes match before and after both runs.
All six Linux logs were downloaded before the sandbox stopped. Documentation
checks pass: 243 Markdown files and 506 local links.
At this batch, S1 remained open at 9.80% line reduction and 8.71% token reduction.

The checked child-ID conversion remains. Pinned standard-library sources show
that a live Unix child returns a native positive process ID. Replacing the
checked OS conversion with a signed cast would conflict with the repository's
checked-conversion rule. No such change or reduction credit is included.

## Ordered install targets

The resolved install batch owns one ordered target-to-display-name map.
The map keeps the first native target position and the last qualified Package
ID for that target. Every original request still runs its resolution and
availability checks before insertion. Preparation checks each distinct target
once in that order. Already-installed and failure reports keep the last label.
The existing Prepared values and native mutation methods remain.

This removes the parallel name map, its separately movable request field,
the second deduplication set and branch, name lookups, and label and key clones.
The direct indexmap dependency uses the already locked 2.14.2 package.
The lock graph gains only that direct edge; no package or version changes.
Two noncomment Cargo bookkeeping lines are added and receive no Rust credit.
Container allocation and String drop timing change. No allocation or timing
reduction is claimed.

Both full copied client sources compile with warnings denied. All 18 unchanged
focused tests pass in each version. All 18 actual CLI comparisons match output,
status, and native command arguments. They cover both orders of duplicate labels
and mixed target orders. The independent Spec review also reruns those comparisons.
Both review axes pass. This removes eight real production lines and 79 tokens
after excluding three iterator wrapping lines.

Full macOS and Linux verification passes for this applied follow-up.
Both hosts pass strict locked workspace Clippy, workspace tests, four real
Nix discovery tests, and native CLI lifecycle and cancellation checks. macOS
also passes format, diff, workspace build, private Rustdoc, and dependency
checks. All 177 source and check-file hashes match before and after both runs.
All six Linux logs were downloaded before the sandbox stopped.
At this batch, S1 remained open at 9.85% line reduction and 8.78% token reduction.

## Unix platform scope

The client and generator libraries declare their existing Unix platform scope
once. The client removes two repeated production guards and two inactive
non-Unix declarations. The generator removes five repeated production guards
and its inactive non-Unix canary fallback. All active brace and let-block scopes
remain. The complete active source is byte-identical for both documented target
configurations. Main files, tests, test attributes, safety checks, permissions,
original causes, cancellation, and macOS/Linux policy branches remain.

The client removes five real production lines and 55 tokens. The generator
removes eight real production lines and 47 tokens. Both added crate attributes
are charged. The combined reduction is 13 lines and 102 tokens. No wrapping,
test, fixture, move, or dependency source receives credit.

Both complete copied source trees compile with warnings denied on macOS.
All 17 affected client tests pass before and after. The complete 155-test lists
and CLI version bytes match. All 131 generator library tests and its one CLI
test pass before and after. Independent Standards and Spec reviews pass for
each candidate. They check the exact source, counts, scopes, and frozen artifacts.

The unsupported non-Unix Rust library interfaces now expose no items. The old
complete graphs already required unavailable Unix-only pkg-process exports.
The unchanged binaries still require those exports. This is a source conclusion.
No Windows or non-Unix compilation or execution is claimed. Every Unix target
retains its existing OS and architecture conditions.

Full macOS and Linux checks pass for the applied tree. Both hosts pass strict
locked workspace Clippy, workspace tests, four real Nix discovery tests, and
native CLI lifecycle and cancellation checks. macOS also passes format, diff,
workspace build, private Rustdoc, and dependency checks. All 177 source and
check-file hashes match before and after both runs. All six Linux logs were
downloaded before the sandbox stopped. Installer syntax and ShellCheck pass.
The installer suite passes 37 tests and skips seven tests that require an
unavailable mount namespace. Documentation checks pass for 243 Markdown files
and 506 local links. S1 remains open at 9.93% line reduction and 8.87% token
reduction.

A shared cache/background temporary-file preparation helper was also measured
and rejected. It adds three real lines and 24 tokens after all module and caller
costs. Publication error ownership, failed-file lifetime, and durability rules
remain separate. The rejected helper was not compiled, applied, or credited.

## Verification

The Unix platform follow-up passes the full checks below on both hosts.

The capture, typed configuration, typed Catalog Index, and command-cause
changes pass the full checks below.
The latest coverage and saved-tap collection changes also pass all checks.
The independent Standards and Spec reviews found no concrete issue in either change.
The borrowed constraint and artifact decoder changes pass the full checks on both hosts.
Their independent Standards and Spec reviews found no concrete issue.
The latest unused interface cleanup also passes the full checks on both hosts.
The catalog rule and retained child-ID simplifications pass the same full checks.
The app-cause and saved-catalog construction changes also pass the full checks.
The shared CLI request, maintainer cancellation, and Formula metadata changes pass them as well.
The borrowed direct-command arguments also pass the full checks on both hosts.
The latest Nix, Step, and enum changes have passed both independent reviews.
Their full macOS and Linux checks also passed.
The latest native budget and borrowed replacement path changes pass the same checks.
The process state, launcher status, catalog lookup, reference, and decision-field batch
has passed both reviews and full macOS and Linux checks.
The command-cause regression confirms access to the original host error.
Linux source hashes match the local tree.

The macOS host uses Apple silicon and Rust 1.96.1.
The Linux sandbox uses x86_64 and Rust 1.96.1.
The native checks use the installed Nix runtime on each host.
The latest Linux verification compares 177 source and check-file hashes with the local tree.
The final hashes match after the full checks.
Test logs were saved before the sandbox stopped.

| Check | macOS | Linux |
|---|---|---|
| Workspace tests, locked, all features | Pass | Pass |
| Four real Nix discovery tests | Pass | Pass |
| Native CLI lifecycle checks | Pass | Pass |
| Clippy, all targets and features, warnings denied | Pass | Pass |
| Workspace build, all targets and features | Pass | Covered by test and Clippy builds |
| Rustdoc, private items, warnings denied | Pass | Not repeated |
| Format check | Pass | Same source files |
| Dependency advisories, bans, licenses, and sources | Pass | Same lock file |
| Documentation links and OpenSpec structure | Pass | Same documentation |

Native macOS checks include Formula aliases, package state, duplicate install,
refusals, upgrade, rollback, removal, cask host bounds, original cask sources,
helper startup, launcher sync, launcher removal, source isolation, namespace
choices, and cancellation.
Linux runs the applicable native lifecycle, discovery, source, and cancellation checks.

Installer syntax, ShellCheck, and installer tests also passed during this change.
The installer source did not change.
The latest macOS installer run passed 37 tests and skipped seven Nix-detection
tests. Those tests require an unavailable mount namespace. No installer test
failed.

All changes are local. No commit, pull request, merge, or release was made.
