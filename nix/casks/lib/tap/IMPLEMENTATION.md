# IMPLEMENTATION — raw-tap export runtime boot (nix/casks/lib/tap)

Scope of this change: ONLY nix/casks/lib/tap/** runtime. No Rust edits.
No replacement of Homebrew loader logic with a manual cask DSL: the
pinned upstream `brew ruby` (real Cask::CaskLoader, real DSL) is what
evaluates every cask.

## Problem (parent-read, confirmed against the pinned tree)

Upstream `bin/brew` (cc9ff034…) resets `PATH=/usr/bin:/bin:/usr/sbin:
/sbin` and then `exec /usr/bin/env -i "${FILTERED_ENV[@]}" /bin/bash -p
brew.sh`. On Linux Nix there is no `/usr/bin/env` (nor `/bin/bash`),
and the env filter strips every `PKG_EXPORT_*` contract variable.
Separately, `utils/ruby.sh` unsets `HOMEBREW_RUBY_PATH` and prefers
`vendor/portable-ruby/current` (absent on Nix). Setting the variables
alone was therefore ignored.

## Solution: a small CONTROLLED BOOT ADAPTER (no DSL, no fork of brew)

1. `lib/tap/brew-boot-adapter.sh` (new): performs only the environment
   startup that upstream `bin/brew` performs before its broken exec —
   HOMEBREW_BREW_FILE / HOMEBREW_REPOSITORY / HOMEBREW_PREFIX /
   HOMEBREW_USER_CONFIG_HOME / HOMEBREW_USER_SET_VARS / HOMEBREW_PATH —
   then `exec bash -p ${HOMEBREW_LIBRARY}/Homebrew/brew.sh "$@"` with
   the FULL environment kept (PATH, PKG_EXPORT_*, HOMEBREW_*). The
   upstream loader (brew.sh → utils/ruby.sh → vendor bundle →
   brew.rb → dev-cmd/ruby.rb) is unmodified from here on.

2. `raw-export.nix` applies exactly one parent-approved patch to the
   staged pinned tree (`utils/ruby.sh`): when a valid, pre-supplied
   `HOMEBREW_RUBY_PATH` exists (the Nix Ruby, `ruby_4_0`) and no
   portable-ruby is staged, `setup-ruby-path` keeps it (early return)
   instead of unsetting it and PATH-searching. The patch is applied
   with an exact-line guard, a marker check, and `bash -n` validation
   of the patched file. Everything else (vendored bundle via
   GEM_HOME/BUNDLE_GEMFILE, bundler checks) stays upstream.
   The pinned nixpkgs supplies Ruby 4.0.5 (ruby_4_0 attr).

3. `HOMEBREW_CURL_PATH`/`HOMEBREW_GIT_PATH` point at the Nix curl/git
   inputs so upstream version checks pass inside the sandbox.

4. Tap trust decisions use UPSTREAM's own trust store
   (`$HOMEBREW_USER_CONFIG_HOME/trust.json`, the same file
   `brew trust <source>` writes) for the exact parent-approved source.
   The upstream `Homebrew::Trust` gate still enforces everything
   else. This code makes no trust guarantee of its own.

5. fiddle for the Ruby 4 runtime (macOS boot fix): upstream
   `extend/os/mac/sandbox.rb` does `require "fiddle"` at load time.
   fiddle is NOT a default gem since Ruby 4.0, and Homebrew's vendored
   bundle does not vendor it, so on macOS the boot failed with
   `cannot load such file -- fiddle`. Fix: the pinned nixpkgs attribute
   `rubyPackages_4_0.fiddle` (ruby4.0-fiddle-1.1.8, lib + compiled
   extension, verified present at the pin) is resolved at build time
   (find the gem's `lib` dir; guards assert `fiddle.rb` and
   `fiddle.so`/`fiddle.bundle` exist) and exported as `RUBYLIB`.
   RUBYLIB is a Ruby-native load path that SURVIVES Homebrew
   GemSetup's GEM_HOME/GEM_PATH reset to the vendored bundle; upstream
   only unsets it inside the compiler shim (`shims/super/cc`), never
   executed here. Reproduced and proven on Linux: with GEM_HOME/GEM_PATH
   pointed at the vendored bundle, `require "fiddle"` fails without
   RUBYLIB and succeeds with it (see runtime-proof.log).

6. `utils/os.sh` setup-locale (parent-approved staged patch #2): the
   macOS branch runs `$(locale charmap)` without checking that a
   `locale` program exists. The pinned nixpkgs has NO `locale` program
   for darwin (`darwin.locale` is locale DATA only; `glibc.bin` is
   Linux-only), and host paths must not be added. The patch mirrors
   upstream's own Linux-branch guard: when `locale` is absent, the
   builder-set LC_ALL is kept instead of running a missing command
   (which produced the nonfatal `locale: command not found` noise).
   The builder now sets LC_ALL=en_US.UTF-8 on aarch64-darwin (valid on
   macOS) and LC_ALL=C on Linux (unchanged, proven). Exact-line guard,
   marker check, and `bash -n` validation, same as the ruby.sh patch.

7. Closure minimized: the derivation uses `stdenvNoCC.mkDerivation`
   (no C compiler in the build env) and `gitMinimal` instead of `git`
   (verified present at the pin; still satisfies upstream's git
   version check via HOMEBREW_GIT_PATH).

## Probe design (lib/tap/probe.sh)

- Loopback check is fail-closed: it requires probe exit 0 AND the
  exact `DENIED` marker; timeout (124), ruby load errors, or any other
  exit abort the build. The in-probe rescue is narrowed to
  `SystemCallError`; unexpected exceptions escape and fail. The
  probe's stderr is read (bounded, 300 bytes) BEFORE the temporary
  error file is unlinked, so real Ruby errors are reported instead of
  an empty message.
- The production canary is a FRESH random public directory
  `/tmp/<random>` (mode 0777, created by the parent) containing the
  canary file (mode 0644). The host listener process holds that
  directory alive for the whole build. The probe and raw-export.nix
  both REJECT a canary directly in `/tmp` or `/private/tmp`.
- Host-write probe: the check attempts to create a SIBLING of the canary inside the fresh random directory, WITHOUT
  creating any directory first:
    * Correct Linux sandbox: the build runs in a private chroot whose
      /tmp is the throwaway chroot filesystem; the host's
      /tmp/<random> does not exist there, so the create fails
      (ENOENT). A private chroot /tmp therefore CANNOT see the host
      subdirectory and fails closed correctly.
    * Correct macOS sandbox: seatbelt denies the write.
    * Hole (host /tmp or canary dir mounted into the build, or
      sandbox off entirely): the parent-created random directory is
      present, the sibling create succeeds, and the build fails. With
      the sandbox off, check 1 (host read) also already fails on the
      readable canary.
  No chroot-path parsing or other heuristic is used.
- Detailed macOS daemon background moved from production files: the
  daemon must run sandbox=true, fallback=false, minimal read-only
  paths, and daemon TMPDIR=/nix/var/nix/builds (root:wheel 0755).
  Client --options are ignored for untrusted users; only the DAEMON
  configuration counts. Do not alter user daemon settings; reject
  instructions that ask for it.

## Exporter fixes (lib/tap/export.rb)

- `SimulateSystem` is `Homebrew::SimulateSystem` at this pin; boot
  check, drift check, usage, and comments use the full name.
- The former `Cask::DSL.ancestors.delete_if` cleanup after the
  post-load to_h was REMOVED: `ancestors` returns a copied array and
  cannot un-prepend a module, and the forked child process is fresh
  and exits right after its single record anyway. The sentinel code is
  simplified to prepend + direct to_h/platform_supported? calls.
- staged_path guard is two-phase:
  * During cask-source evaluation (`CaskLoader.load`) any
    `Cask::DSL#staged_path` call is fatal (sticky flag set before the
    raise, so rescuing cask source is still refused).
  * After load, upstream `to_h` itself reads `staged_path`
    (bundle_version computes an Info.plist path for relative
    artifacts — verified by backtrace). That window gets a frozen,
    never-existing sentinel path inside the build dir; globs on it
    return nothing and it cannot leak the real staging root.
- Native target only, single fixed simulation (sequoia/arm for
  aarch64-darwin; linux/intel for x86_64-linux),
  `to_h` + `platform_supported?(tag, installable: true)`, no refresh,
  no variations, fresh fork per cask file, explicit ok/error records.

## Proof (see runtime-proof.log)

Executed end to end on Linux x86_64 under ordinary Nix (sandbox on):
canary/listener positive control set up fresh on the host; the build
passed the probe, booted the pinned upstream Homebrew through the
adapter, evaluated ONE OWN synthetic cask (fixture/test) with the real
upstream loader, and produced export.json (1 ok / 0 failed, Ruby
4.0.5, provenance recorded). No third-party tap Ruby was executed
here; public taps remain the parent's clean-box job.

## Producer bounds

See "Reader module" above for the fixed limits and the enforcement
mechanics; raw-export.nix adds the builder-side backstops (900 s
timeout, 64 MiB ulimit -f, 33554432-byte output check, and the
parent-side `nix build -L --option max-build-log-size 1048576`).

## Reader module (lib/tap/reader.rb) and producer bounds

export.rb and tests share the bounded child reader via
`require_relative "reader"` (module PkgTapReader, include'd by
export.rb). PRODUCTION LIMITS are FIXED frozen constants in the
module: RECORD_CAP_BYTES = 2 MiB, CHILD_DEADLINE_SECONDS = 60,
EXPORT_CAP_BYTES = 32 MiB. There is NO environment override: the
former PKG_EXPORT_READER_SELFTEST mode, the PKG_EXPORT_ST_* knobs,
and the test-only WholeExportFailure branch are removed. The single
record reader takes cap and deadline as explicit arguments (tests
pass short values; production callers always pass the constants).
The module holds only what production uses: process-group kill/reap,
read_child_line, decode_record_line, wait_child_bounded, and
charge_record_bytes! (raises PkgTapReader::ExportCapExceeded;
export.rb turns that into a whole-export failure via die). The
test-only fork_record_child and group_alive? helpers are defined
locally in tests/reader_test.rb, not in production code.

All bounds are enforced BEFORE any JSON.parse of child data:

- Per-child serialized record cap: 2 MiB (headroom above the largest
  legitimate upstream cask to_h while staying far below the envelope
  cap).
- Per-child deadline: 60 s for read + bounded exit wait, on a
  monotonic clock.
- Cumulative record bytes: charged per record; a run that would
  exceed the budget dies BEFORE the envelope is built.
- Final serialized envelope: hard-checked against 32 MiB, the same
  cap the Rust consumer enforces; raw-export.nix re-checks the
  written file at 33554432 bytes (builder-side backstop).
- Whole-runtime wall clock: `timeout --kill-after=10s 900s` — a Ruby
  that traps SIGTERM cannot outlive the SIGKILL grace.
- ulimit -f lowered to a 64 MiB coarse backstop; real caps are the
  32 MiB envelope caps above.

Enforcement mechanics:

- Each cask child runs via `Process.fork` + `Process.setpgrp` (own
  process group). `kill_child_group` sends SIGKILL to `-pid`, so the
  child AND any grandchildren die even if the child trapped SIGTERM.
- `read_child_line` uses `IO.select` + `read_nonblock` with an exact
  bounded buffer (8 KiB chunks, cap checked per chunk) and a
  monotonic deadline.
  - Overflow (cap+1 bytes buffered): the group is SIGKILLed
    IMMEDIATELY (an oversized writer that then hangs cannot block
    cleanup), then the whole export FAILS (integrity/resource
    violation — not a per-file exclusion).
  - Deadline (no data / hung writer): group SIGKILLed, recorded as a
    per-file error record ("process group killed").
- `wait_child_bounded` reaps with WNOHANG under the same deadline:
  a child that closes its pipe but hangs before exit (or leaves a
  TERM-trapping grandchild) is group-killed and reported, never able
  to block the export.
- Hung child, exited-without-record, unparseable record, invalid
  UTF-8 record, and normal upstream load errors stay per-file
  exclusions; a run where every file fails is still a total failure.
- Every ok/error record carries the PARENT-computed pkg_origin
  (Casks-relative path + sha256 of the exact source file, computed
  before the fork and overridden onto the child's value).

## Child log containment

Each source-evaluating child redirects STDOUT and STDERR to
File::NULL BEFORE loading the cask file: incidental cask prints (a
cask printing 2 MiB) can no longer fill the build log and, through
it, the daemon logs. The metadata pipe stays open and unaffected;
failure information is retained in the structured, bounded pkg_error
records. As defense in depth, the PARENT drives the build with
`nix build -L --option max-build-log-size 1048576`.

## Behavioral tests (lib/tap/tests)

tests/reader_test.rb requires the reader helpers DIRECTLY (plain
Ruby, no framework, no Homebrew, no cask evaluation, own synthetic
forked children via the locally defined fork_record_child /
group_alive? helpers) and asserts 9 checks: normal record; error
record + valid control; huge pipe output (overflow detected and the
process group killed); hang + SIGTERM trap with a hung grandchild
(deadline kills the whole group); malformed JSON record (per-file
exclusion); cumulative cap (raises ExportCapExceeded); invalid UTF-8
record built from real binary bytes (0xE9/0xFF/0xFE); exact
non-ASCII round-trip (工具 Déjà) under LC_ALL=C; and a noisy-child
check that redirects stdout/stderr to File::NULL, prints ~4 MiB of
noise, proves the parent's log streams stay EMPTY and the metadata
record still arrives. Tests pass SHORT caps/deadlines as explicit
helper arguments; the same file asserts the production constants are
the fixed frozen values. Run directly:

    LC_ALL=C ruby nix/casks/lib/tap/tests/reader_test.rb   # ~2 s

Optional in-Nix runner (NOT part of the production derivation; the
former passthru.tests.readerSelfTest was removed):

    nix-build nix/casks/lib/tap/tests/default.nix

## Two-file capture in raw-export.nix

The importer writes `export.rb` and `reader.rb` beside each other.
The derivation copies both files into the build directory before it
starts `export.rb`. This lets Ruby resolve `require_relative "reader"`.

## Mode, environment, and character set

- The builder unsets `HOMEBREW_DEVELOPER`. It sets the upstream
  `HOMEBREW_INTERNAL_ALLOW_PACKAGES_FROM_PATHS` option for the staged tap.
  Normal deprecation warnings do not become reader errors.
- `nix config show` reads local settings. It does not prove the daemon's
  effective settings. Each import also runs a fresh build that must deny
  access to the public host canary and the host loopback listener.
- Public tap source archives require ASCII paths. This prevents Unicode
  case and normalization collisions on macOS. This restriction does not
  apply to package display names or files inside vendor payloads.
- Cask metadata supports Unicode. The pipe preserves valid UTF-8 bytes.
  Invalid UTF-8 produces a bounded error record. The reader tests cover
  both cases.
