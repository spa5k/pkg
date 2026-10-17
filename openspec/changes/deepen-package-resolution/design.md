# Design

## Cancellation

The import runtime retains the first SIGINT, SIGTERM, or SIGHUP. It stops its owned process group. The import error retains that signal through staging. A failed supervision retains both the signal and the I/O cause. Native and import errors expose the same cancellation decision to their callers. The command reports the error and exits with `128 + signal`. Only a typed sandbox refusal can offer setup. Cancellation does not publish a generation or registry entry.

Setup, staging validation, host-version checks, and install suggestions retain cancellation. Supplied stdin is written without blocking signal checks. An input error retains the child's cancellation after reaping. A forced stop after an ordinary delivery failure remains an ordinary failure.

Mutation failure reporting retains the mutation signal first. If the mutation has no signal, a cancelled profile check supplies it. Cancellation stops further conflict discovery. A failed supervision still reports its I/O cause. Rollback, postflight, and prune reports use the same cause decision. Pure interruptions retain concise output. Background schedule and optional control failures retain typed causes and cancellation through command reporting.

## Package Resolution

One resolution session owns the target system, source routing, lazy saved catalogs, official catalogs, and formula aliases. Inspection returns the resolved package and its availability. Installation resolves and checks the complete batch before profile mutation. Qualified requests load their selected source. Bare requests check every source before they can select one package. Upgrade views use each Profile Entry's full original Source Reference. Different branches retain separate views.

## Catalog designs considered

Three parallel designs examined the broader catalog cluster.

The first design uses three complete operations: prepare, search, and resolve. Its request and result enums give one seam for each user operation. Callers must handle the result variants.

The second design separates an offline Catalog handle from a live handle. This makes the Nix dependency explicit. It adds a second handle for callers to learn.

The third design gives Catalog an offline default and explicit live methods. It offers the simplest common search call. This is the preferred direction for a broader Catalog refactor.

The current change retains the focused Package Resolution owner. It applies shared snapshot provenance, row construction, availability badges, and result retention. It does not add a new Catalog facade that only delegates to the same owners.

The three designs estimated 110–245 net production lines for a broader refactor. The direct index lookup and shared cache writer changes are now applied. Their actual deletion is included in the measured result. Moves receive no credit. Native traversal, generated-index validation, matching, and atomic publication still carry required behavior. The remaining design estimates do not meet the whole-repository reduction target.

## Shared policy owners

The validated Catalog Index owns target and entry lookups. Availability and information read its recorded entry status directly. The intermediate cask status type is removed.

Upgrade preparation owns selection, registry validation, removed-source checks, fixed-source skips, and the final batch gate. It checks removed sources before any fixed-source skip output. The checked nonempty target value still prevents an empty selection from reaching native upgrade.

The planner and classifier share one Catalog Exclusion value. An outcome is an eligible plan or that exclusion. The manual serializer retains field order and null values. Refusal reason classes and policy checks remain. Malformed-input detail text and shape-error order can change. Artifact kinds and target support use one closed mapping.

Typo matching uses the already locked `strsim` optimal string alignment algorithm. Character limits and distance limits remain.

Path references use the already locked percent encoder. The strict malformed-escape check remains before decoding. Raw Unix bytes and literal `+` characters remain unchanged. Staging uses `TempDir` directly for cleanup and retains the replaced generation through `keep()`. Search has one streamed table renderer. Mutation failures have one heading and state report owner.

The strict index decoder constructs the validated index directly. One ordered loop checks input and optional raw provenance fields. The generator borrows JSON records without a forwarding record type. Native commands share their argument, tracing, and color setup. Discovery retains its own environment and resource policy.

Atomic exchange and exclusive rename use the already locked `rustix` bindings. All pre-call checks and recovery rules remain. Nonempty rename flags cannot use an ordinary rename fallback.

Nonblocking pipe flags use borrowed descriptors through `rustix`. Input delivery, poll order, output budgets, and error behavior remain.

Tap capture constructs entries in its one conversion loop. Source origins share path, hash, and lowercase rules. Tap imports retain their exact tree, hash, and token binding checks. Origin failures still precede duplicate-token failures. The bounded export document retains object-only root and file inventory fields. JSON normalization keeps the original last-key-wins rule before typed export decoding. Consumed entries and the capture byte hash remain unchanged.

Effective Nix settings use typed decoding before host policy checks. The decoder retains object-only inputs, ignored metadata, missing/null rules, mandatory grants, and mapping override order. Malformed-shape diagnostics use sanitized setup advice. They can precede semantic policy diagnostics. No configuration values appear in shape errors.

Search output and completion retain typed I/O causes. Search storage, JSON, and checked integer errors retain their causes at the command owner. Output text, flush order, and ordinary failure exit status remain.

One typed Catalog Index document owns the decoded and encoded fields. Validation reads typed entries in place. A sorted copy compares target sets while the original vector retains target order and duplicates. Object-only sections, prior record-array acceptance, duplicate-key normalization, source bindings, null status rules, and coherent numeric bounds remain. The pinned `serde_path_to_error` adapter retains source, system, entry, and field paths in type failures. Semantic refusal diagnostics can change while retaining the relevant source or entry identity.

Coverage reports derive counts from the final classified entries. The generator no longer keeps two cross-target count maps. Strict saved-tap loading reuses its checked cache after all registry sources load in order. The intermediate keyed collection is removed.

macOS constraints and artifact options use borrowed typed decoders. Constraint fields still require objects with exactly one string per operator array. Missing fields remain optional. Explicit null artifact fields and unknown fields remain refused. Installer choices retain priority. Explicit source objects retain the pending source and artifact order. Numeric version, range, baseline, path, collision, and target rules remain. Malformed-shape diagnostic text and order can change within the same refusal reason class.

Package Resolution is the only strict whole-set saved-catalog loader. The retired public loaders and unused metadata interfaces are removed. Their behavior tests use the current command owner and serialized document. Serialized provenance fields and staging source ownership remain enforced. No production interface was added only for tests.

App errors share their context and cause construction. Derived cause handling exposes the existing AppErrorCause as one source step. Its source retains the original native or process error. CatalogError exposes the existing shared Arc<HostVersionError> source. The same host error object and deepest I/O cause remain accessible. Cancellation and the Dock-work flag remain. Saved-catalog loading constructs its result at the one loading site. Its first source check, binding checks, reference checks, error context, and held read lock remain.

Direct commands accept borrowed string arguments. Native Nix commands also
borrow their prefix, arguments, and profile path text until `Command` copies
them. Path conversion and command order remain. Native interruption uses the
shared classifier after the resource guard. Recorded parent cancellation wins
first; a resource failure wins next; the child's outcome wins last.

The setup plan has one argument-owning Step constructor. Its stdin builder
removes the second argument copy. The launchd retry comparison uses borrowed
literals. Consent, refusal, stdin delivery, readiness, and retry rules remain.

Static reason, source kind, source status, and mutation action names use the
pinned `strum_macros` derive. It emits literal matches at compile time.
Permanent literal tests protect public spelling when Rust variants change.
Source status JSON spelling remains checked separately. This adds two
compile-time packages: `strum_macros` 0.28.0 and `syn` 2.0.119. Existing
dependency versions remain.

Native discovery owns its metadata budgets at its command call. The process
module owns enforcement. The unused budget factory and forwarding function
are removed. Native discovery retains its memory, wall, and per-stream output
limits, Capture mode, native environment, original causes, and classification.
Profile replacement borrows both path conversions until command construction.

The process monitor uses its drain deadline as the observed-exit state.
Cancellation still records its first signal until active handlers finish.
The restoration caller owns the retained signal read. Group cleanup and reap
keep their order. Launcher status joins its existing borrowed name lists directly.

Token lookup checks the validated index's sorted source keys through exact record
lookup. Excluded choices remain part of ambiguity. Native snapshot matching uses
the revision already checked by the snapshot reader. The SQL owner still limits
pending jobs to 256. Reference classification computes its scheme once.
Target decisions select their outcome fields once and retain exact serialized
field order and nulls. The unused detail getter is removed; its behavior test
checks the published JSON detail.

The importer calls the actual tap export function with its named 45-minute limit.
The unused forwarding function and output type alias are removed. Runtime tests
use the same production function and retain their existing limits and assertions.

Repository verification owns one commit-response path. Default and pinned
revision rules retain their checks, request order, and diagnostics. Setup records
effective sandbox readiness after its complete custom-file readability check.
The unused text-derived readiness result is removed. Daemon environment
inspection records no dictionary, a dictionary without TMPDIR, or a dictionary
with TMPDIR. This removes the impossible combination of two Boolean fields.
Source refusal shares its
two identical conditions. Source labels reuse their existing normalized buffer.

Atomic rename calls borrow their paths directly through Rustix. Its path
conversion retains raw Unix bytes and rejects NUL before the syscall. Custom
C-string conversion is removed. Archive file counts use the checked entry
limit that runs before unpacking. The repeated file limit is removed because
every regular file is one counted entry. Other extraction limits remain.
Artifact source validation has one accepted-path predicate and malformed-source
detail. Accepted paths, refusal classes, and valid catalog bytes remain.

## Reduction

Use the production Rust files in all workspace source trees. Exclude test files, fixture files, inline test items, comments, documentation attributes, and test-only attributes. Record the baseline before production edits. Count Rust tokens as well as physical code lines. File moves and formatting cannot meet the target. Keep behavior tests at the existing command and runtime interfaces.

Catalog URL checks share the public-IP refusal. Domain suffixes and artifact summary kinds use borrowed values. Checksums share the existing SHA-256 rule. Token validation retains its ASCII grammar over bytes. Process observation and RSS measurement read the retained child ID directly. The signed signal PID conversion remains checked at construction. All refusal, signal, wait, and resource rules remain.

The maintainer CLI and importer share the import request. The CLI retains typed import failures through its final exit-status decision. Flags, defaults, help text, raw path bytes, and successful JSON output remain. Formula metadata decodes from borrowed fields. The validated index and its one required input prove each Formula entry source, so the repeated source traversal is removed. Error prefixes and aliases-first decoding remain.

The local config gate now returns its existing typed ImportError directly.
Staging returns CommandError and matches only its nested SandboxRefused error
for the one setup retry. The intermediate GateError and StageError types are
removed. Signals and original causes retain their existing owners.
Completion produces launcher names and final output text before app refresh.
It prints that text only after refresh succeeds. Its second completion dispatch
and parallel state are removed. Identity lookup reuses canonical source and
attribute decoding. Snapshot current lookup uses its existing prepared interface
before locking and after locking. The reduction requirement remains open.


Bare-name resolution uses one existing owner. Its optional native lane contains
only the Source Reference and cache path. Bare casks omit that lane. The two
forwarding functions are removed. Exact lookup uses the Source Revision in
place. Saved Catalog loading, source locking, and error order remain.
Catalog classification uses one entry point with the existing PlatformGate.
Official records supply Tags. Tap capture supplies the checked upstream Boolean.
The platform and dependency stages share borrowed string-array validation.
Validation adds no successful-path allocation. The existing target mapping owns
the exact platform tags. Refusal classes, check priority, early architecture
acceptance, and eligible output remain. Malformed detail wording can change.


The shared process module has one actual run owner. Runtime adapters supply
their existing stdio mode, sink, limits, and optional borrowed input directly.
The three forwarding functions and the setup input dispatch are removed.
The private Nix execution owner replaces its no-sink forwarding method.
The supervisor body, input completion, signal retention, group ownership,
resource limits, error causes, cleanup, and reap order remain. Product command
callers keep their existing runtime interfaces. The reduction target remains open.


Cask identity parsing uses its strict decoder and exactly three path segments.
Those checks prove the canonical round trip, so its repeated encoding is removed.
Source and token validation remain. The installed-entry parsers retain their
round-trip guards because their split tails can contain literal dots.
The reduction requirement remains open.


Retained child cleanup shares one protected wait and reap state update.
Exited cleanup still signals the group before consuming the wait. Running
cleanup still retains identity during force-stop. Signal restoration shares
one exact deactivation sequence. Read-only pipe forwarding methods are removed
inside their existing owner. Signal counts, memory order, I/O priority, limits,
original error causes, cleanup, and reap order remain.
ANSI removal uses one fixed compiled CSI pattern from the existing dependency.
It preserves ASCII termination, incomplete sequences, Unicode, and each
existing normalization call. These changes pass both reviews and full checks
on macOS and Linux. The reduction requirement remains open.


Setup records fixed plist presence without owning or converting its constant
path. The launcher destination keeps its absolute, non-root HOME guard and
removes its proven unreachable derived-parent guard. Filesystem ownership and
publication checks remain. Package ID parsing shares its one full cask branch.
Source precedence and refusal priority remain. Only the malformed full cask
diagnostic is normalized. These changes pass both reviews and full macOS and
Linux checks. The reduction requirement remains open.


Availability and cask visiting accept their validated Catalog Index directly.
Saved Catalog search does not construct a full metadata view. Reference fallback,
rows, rank, counts, cancellation, and original causes remain. Three signal
counters share one const initializer and retain independent atomic storage.
Both changes pass independent reviews and full macOS and Linux checks. The
production reduction requirement remains open.

Tap staging owns its SourceStore and exclusive lock. Publication consumes the stage and retains the same store through finish and undo. No caller can select a second store for publication. Provenance and binding validation remain. The staging guard drops before the store on failure. All fallible commit work runs before either field moves. The registry lock still covers CLI rollback reporting and return.

The process crate declares the Unix platform boundary once. Its supported active code, OS branches, signals, ownership, and cleanup remain. The official generator builds one final entry map and one private catalog envelope. Classification stays pure. Generated bytes, coverage, and integrity refusals remain; rejected snapshots can perform more prefix work.

Configured native-probe errors own Box<NixError>. The actual constructor, original objects, diagnostics, and cancellation remain. The immediate Rust source interface exposes the box. Its as_ref gives the original error. The full-commit validator directly re-exports the existing catalog predicate under its domain name. The production reduction requirement remains open.

## Required mutation profile and concrete archive drain

Filtered native mutations pass their existing lossy profile text directly.
The reporter requires that profile at construction. The argument scan, reporter
factory, configuration builder, and unreachable missing-profile state are removed.
Terminal detection, initialization order, one owned profile string, both ANSI
normalizations, and stream behavior remain.

The private bounded archive reader owns the sole actual memory gzip decoder.
Standard copying through a 64 KiB buffer replaces its final read loop.
The read size retains damage and decoded-limit error priority. The concrete
source cannot return Interrupted, which the standard copy operation retries.
Limits, path and PAX rules, collisions, permission handling, and tar alignment
remain. Plain copying is rejected because its smaller read size changes the
first error on damaged streams that also exceed the decoded limit.

These changes remove 19 substantive lines and 98 tokens after formatting
exclusions. The whole-repository 20% target remains open.

## Source and stdio ownership

Catalog routing owns the existing Sources record. Source cloning, allocation
and drop order, and owned independence remain. The tap map still moves into
routing. The duplicate source field declarations are removed.

Filtered IoMode owns its required mutable sink borrow. The supervisor no longer
accepts or stores a second optional sink. Finishing retires the borrow before
the callback. Stdio handling, limits, signals, child ownership, error priority,
cleanup, and callback order remain. The enum loses value and thread-safety
traits that no production caller uses. Its stored fields grow by eight bytes.

Host path validation chooses its policy from the checked operating system.
Its ordered path and impure-dependency loops are shared. The private correlated
boolean is removed. Grant merging, check order, exact refusal details, and the
runtime sandbox probe remain.

These changes remove 22 real production lines and 78 tokens after formatting
exclusions. The whole-repository 20% target remains open.

## Ordered install targets

The resolved batch owns one IndexMap from native target to resolved Package ID.
It keeps the first target position and the last display name for duplicates.
Every original request still runs resolution and availability checks before
insertion. Preparation checks each distinct target once in order. Its existing
Prepared vectors and native execution and reporting bodies remain.

The parallel name map, separately movable request field, second deduplication
set and branch, and key and label clones are removed. The direct dependency
uses the already locked indexmap 2.14.2 package. Only its direct lock edge is
added. No package or version changes. Container allocation and String drop
timing change; no performance reduction is claimed.

This removes eight real production lines and 79 tokens after excluding three
iterator wrapping lines. The whole-repository 20% target remains open.

## Unix platform scope

The client and generator libraries declare their existing Unix condition once.
Repeated production guards and inactive non-Unix fallback declarations are
removed. Every active brace scope, test attribute, and OS/architecture branch
remains. Raw Source Reference bytes, setup permission checks, canary controls,
resource cleanup, original causes, and cancellation remain in identical code.

On non-Unix the libraries now expose no items. The old complete source graphs
already required unavailable Unix-only pkg-process exports. The unchanged
binaries still require those exports. This unsupported Rust interface change
is a source conclusion; no Windows build or execution is claimed.

These changes remove 13 real production lines and 102 tokens. Both new crate
attributes are included. The whole-repository 20% target remains open.
