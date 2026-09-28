# WASM for local Cask conversion

Checked on 29 September 2026. Research only. No tap Ruby ran. No sandbox
was started. Context7 documentation was checked before primary source.

## Conclusion

A Rust catalog generator could bundle CRuby compiled to WASM and run it
locally through Wasmtime. This can avoid a separate machine or hosted
service. It can remain an update-time operation on a maintainer machine
or in CI. The client can still install from committed JSON through Nix.

This is a feasible direction, not a tested Homebrew integration. Running
Ruby in WASM is supported. Running the unchanged Homebrew Cask runtime in
that environment is unproved. A small bootstrap and host-data adapter
would be required. Do not promise compatibility with every public tap.

## What is already supported

`ruby.wasm` is a port of CRuby. Its current README provides Ruby 4.0
`wasm32-unknown-wasip1` builds and an example that packs Ruby files and its
standard library into a WASM application. Wasmtime runs the result.
The full build includes standard extensions; the minimal build omits
extensions such as JSON. The documented WASI build lacks networking and
Thread APIs. These statements describe that Ruby build, not every WASI
version. Source pin: `58a51e12b1bad5ba23155c2e2d49fef7e9e10472`.
[Ruby WASM README][ruby-wasm]

Wasmtime has a Rust WASIp1 embedding example. It supports the relevant
macOS/Linux and ARM64/x86-64 host platforms. The generator can therefore
embed the runtime instead of requiring a separate Wasmtime executable.
[Rust embedding][embedding] [Host platforms][platforms]

This runs the real Ruby interpreter. It does not require a Rust Ruby
parser. However, supplying our own implementations of `cask`, `version`,
`arch`, artifact classes, and serialization would still reimplement the
Homebrew DSL. Reusing pinned upstream Ruby modules avoids that second
source of semantics. WASM supplies an execution boundary, not the DSL.

## Homebrew compatibility work

The following source uses Homebrew commit
`cc9ff034b1d98cdd4061f68cf715bb967c483a8f`.

| Source evidence | Consequence for a WASM exporter |
| --- | --- |
| `global.rb` loads `startup.rb`. Startup expects variables normally exported by `bin/brew`. `startup/config.rb` raises if `HOMEBREW_BREW_FILE` is absent and creates/resolves a temporary directory. | Do not assume `require "cask/cask_loader"` is a standalone library entry point. Supply a controlled bootstrap, paths, and writable scratch space. [Startup][startup] [Configuration][configuration] [Globals][globals] |
| `standalone/init.rb` sets Ruby paths, vendored Bundler, and gem state. The Gemfile requires Ruby 4.0 or newer and declares the runtime gems. | Package the pinned Ruby libraries and dependencies. Verify that they load in WASI. `standalone.rb` alone does not initialize the complete Cask environment. [Initialization][init] [Gemfile][gemfile] |
| `Cask#to_h` reads `tap_git_head`. This reaches `Tap#git_head`, `GitRepository#head_ref`, and `Utils.popen_read` for `git rev-parse`. | Packing Ruby does not supply the native Git executable. Pass the host-verified source revision through a narrow adapter. Do not add arbitrary host command execution to satisfy this metadata call. [Cask metadata][cask] [Tap revision][tap] [Git call][git] |
| `OS.mac?` and `OS.linux?` read `RbConfig::CONFIG["host_os"]`. `Hardware::CPU.type` reads `RUBY_PLATFORM`. | WASI is not a native macOS/Linux target. Supply explicit target data for supported Homebrew DSL operations. Arbitrary tap host checks still need exclusion or a defined compatibility rule. [OS detection][os] [CPU detection][cpu] |

Bootsnap is optional and has an explicit disable switch. It is not an
unavoidable native-extension blocker. This review did not prove that every
runtime dependency loads in WASI, or that any optional developer gem is
needed for metadata export. Avoid claiming that all native gems must be
ported. [Bootsnap guard][bootsnap]

Homebrew's `SimulateSystem` can select target DSL branches. It cannot
provide macOS commands, installed application files, tap-specific native
extensions, or correct results for arbitrary `RUBY_PLATFORM` checks.
Bundled tap-local Ruby helpers can be made available as files, but their
host-dependent operations remain a limit. Never turn a failed required
operation into an empty successful metadata record. [Simulation][simulate]

## Local execution boundary

Wasmtime's WASI context starts without inherited environment variables or
preopened directories. Directory access can be limited to specific paths
and permissions. Give the guest pinned read-only source files and a small
output/scratch area. Keep source fetching and checksum verification in
Rust outside the guest. Do not expose a general shell or network bridge.
[WASI context][wasi-context]

Set execution and resource limits. Wasmtime provides fuel and epoch
interruption. Its memory-size limit applies per linear memory, so also
limit memory, table, and instance counts. These controls require explicit
configuration; they are not proof that a Cask's semantics are supported.
[Interruption][interrupt] [Store limits][limits]

Bundled native CRuby is another local option. It avoids WASI porting work,
but ordinary native Ruby has host-process capabilities. It would need an
OS isolation boundary and platform-specific runtime packaging. Moving
conversion into a local Nix build also does not by itself make macOS-only
Ruby operations run on a Linux host. These are design deductions, not
tested alternatives in this review.

## Scope of the next proof

Test whether a pinned Homebrew load/export path works with only bounded
bootstrap, Git-provenance, and target-data adapters. Use controlled Ruby
fixtures first. Keep active hooks and structured steps visible in output.
Compare supported metadata with native upstream export. Stop if supporting
the runtime requires a broad Homebrew fork or host-command emulation.

Local conversion is optional. It does not require changing the existing
catalog update and review workflow. It also does not require evaluating
Ruby during each user installation. No compatibility, performance, binary
size, or complete dependency claim was established by this source review.

[ruby-wasm]: https://github.com/ruby/ruby.wasm/blob/58a51e12b1bad5ba23155c2e2d49fef7e9e10472/README.md
[embedding]: https://docs.wasmtime.dev/examples-wasip1.html
[platforms]: https://docs.wasmtime.dev/stability-platform-support.html
[startup]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/startup.rb
[configuration]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/startup/config.rb
[globals]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/global.rb
[init]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/standalone/init.rb
[gemfile]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/Gemfile
[cask]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/cask/cask.rb#L555
[tap]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/tap.rb#L446
[git]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/git_repository.rb#L40
[os]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/os.rb#L12
[cpu]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/hardware.rb#L88
[bootsnap]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/startup/bootsnap.rb#L60
[simulate]: https://github.com/Homebrew/brew/blob/cc9ff034b1d98cdd4061f68cf715bb967c483a8f/Library/Homebrew/simulate_system.rb
[wasi-context]: https://docs.wasmtime.dev/api/wasmtime_wasi/struct.WasiCtxBuilder.html
[interrupt]: https://docs.wasmtime.dev/examples-interrupting-wasm.html
[limits]: https://docs.wasmtime.dev/api/wasmtime/struct.StoreLimitsBuilder.html
