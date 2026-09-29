# raw-export.nix — trusted-builder interface to the pkg raw-tap
# metadata export runtime (sandbox behavior probe + pinned-Homebrew
# cask export).
#
# This is an ORDINARY derivation: no outputHash, no __noChroot, ever.
# The Nix sandbox (and its network/host denials) therefore applies to
# every byte of third-party tap Ruby executed by export.rb.
#
# Prerequisites verified on the clean host before the build (full
# background: IMPLEMENTATION.md):
#   * macOS Nix daemon: sandbox=true, fallback=false, daemon TMPDIR
#     outside any public tmp, minimal read-only paths; only the DAEMON
#     config counts. Never alter user daemon settings.
#   * This derivation is built with __darwinAllowLocalNetworking=false
#     on macOS.
#   * Positive control, FRESH per import (no cached probe): a
#     fresh-nonce canary file at CANARY_PATH, mode 0644, inside a
#     FRESH random public directory /tmp/<random> (directory mode
#     0777, parent IS the tmp root — never /tmp or /private/tmp
#     itself); a host listener holds the canary directory alive for
#     the whole build, and a live loopback listener on
#     127.0.0.1:CANARY_PORT stays up for the whole build. Both were
#     verified reachable from the host immediately before the build.
#
# Usage. tapSource is a DIRECTORY tree: the Rust importer validates
# and unpacks archives before this point, and this file consumes the
# tree as data only (passing Nix code as tapSource is not possible and
# not attempted):
#
#   nix-build nix/casks/lib/tap/raw-export.nix \
#     --arg tapSource /path/to/fixture-tap-tree \
#     --argstr source fixture/test \
#     --argstr revision 40-hex-chars \
#     --argstr system native \
#     --arg canary '{ path = "/tmp/pkg-canary-XXXX/canary"; sha256 = "<64 hex>"; port = 54321; }'
#
# `system` is the single NATIVE export target: "native" resolves to the
# build host (which must then be aarch64-darwin or x86_64-linux); an
# explicit target must EQUAL the build host. No cross evaluation, no
# dual targets, no empty/insecure defaults (canary, revision, system
# are all mandatory and validated).
#
# Result: <out>/export.json (envelope schema pkg-tap-raw-export/1) and
# <out>/provenance.txt.
#
# `pkgs` defaults to the pinned nixpkgs tarball below (the same one
# that carries ruby_4_0); override with --arg pkgs if you must.
{
  pkgs ? import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/567a49d1913ce81ac6e9582e3553dd90a955875f.tar.gz";
    sha256 = "1vq77hlx8mi3z03pw2nf6r5h7473r1p9yxyf58ym3fh01zppmfln";
  }) { },
  lib ? pkgs.lib,
  tapSource,
  # Canonical source id `<owner>/<repo>` (e.g. homebrew/cask or
  # nikitabobko/tap); the `homebrew-` prefix of the tap directory is
  # stripped from the identity, per upstream Tap semantics.
  source,
  # Mandatory exact 40-hex tap revision recorded in the envelope.
  revision,
  # "native" (build host) | "aarch64-darwin" | "x86_64-linux".
  system ? "native",
  # Mandatory host canary + live listener parameters; the build fails
  # unless the sandbox DENIES them (see lib/tap/probe.sh).
  canary ? {
    path = "";
    sha256 = "";
    port = 0;
  },
  # Pinned Homebrew source (full tree incl. committed vendor bundle).
  # `sha256` is the hash of the UNPACKED tree as produced by
  # fetchFromGitHub; a flat fetchurl tarball hash it is NOT.
  homebrew ? {
    owner = "Homebrew";
    repo = "brew";
    revision = "cc9ff034b1d98cdd4061f68cf715bb967c483a8f";
    sha256 = "0x763xn8imvfbp3z2hjf91jbpkh2hifvx8hrjwawyz7zmh9gf89m";
  },
}:
let
  sourceMatch = builtins.match "([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)" source;
  owner = builtins.elemAt sourceMatch 0;
  repo = builtins.elemAt sourceMatch 1;
  # Full tap layout inside the staged tree:
  #   Library/Taps/<owner>/homebrew-<repo>
  # with tap identity owner/<repo without the homebrew- prefix>.
  repoDir = if lib.hasPrefix "homebrew-" repo then repo else "homebrew-" + repo;
  tapName = lib.removePrefix "homebrew-" repo;
  current = builtins.currentSystem;
  target = if system == "native" then current else system;
  canaryPortInt =
    if builtins.isInt canary.port then
      canary.port
    else if builtins.isString canary.port && builtins.match "[0-9]+" canary.port != null then
      lib.toInt canary.port
    else
      -1;
  canaryPort = toString canaryPortInt;
  portOk = canaryPortInt >= 1 && canaryPortInt <= 65535;
  checks = [
    (
      if sourceMatch == null then
        "source must be <owner>/<name> with [A-Za-z0-9_.-] parts (got ${toString source})"
      else
        null
    )
    (
      if
        !(builtins.isPath tapSource || (builtins.isString tapSource && builtins.pathExists tapSource))
        || !builtins.pathExists (toString tapSource + "/.")
      then
        "tapSource must be a directory tree (the Rust importer validates and unpacks archives before export)"
      else
        null
    )
    (
      if !(builtins.isString revision && builtins.match "[0-9a-f]{40}" revision != null) then
        "revision must be exactly 40 hex chars; an empty or partial revision is not accepted"
      else
        null
    )
    (
      if !(system == "native" || system == "aarch64-darwin" || system == "x86_64-linux") then
        "system must be native, aarch64-darwin, or x86_64-linux (got ${toString system}); an empty system is not accepted"
      else
        null
    )
    (
      if system != "native" && system != current then
        "native-target-only: requested ${system} but the build host is ${current}; export on a matching host"
      else
        null
    )
    (
      if !(target == "aarch64-darwin" || target == "x86_64-linux") then
        "the native host ${target} is not a catalog target (aarch64-darwin or x86_64-linux)"
      else
        null
    )
    (
      if !(builtins.isString canary.path && canary.path != "") then
        "canary.path is empty: the sandbox behavior probe is mandatory, refusing to evaluate"
      else if !(builtins.match "(/tmp|/private/tmp)/[^/]+/.+" canary.path != null) then
        "canary.path must be a file inside a fresh random subdirectory of the public tmp, not directly in /tmp or /private/tmp (got ${canary.path})"
      else
        null
    )
    (
      if !(builtins.isString canary.sha256 && builtins.match "[0-9a-f]{64}" canary.sha256 != null) then
        "canary.sha256 must be the 64-hex sha256 of the fresh nonce"
      else
        null
    )
    (
      if !portOk then "canary.port must be a TCP port in 1..65535 (got ${toString canary.port})" else null
    )
  ];
  errors = builtins.filter (e: e != null) checks;
  exportScript = builtins.path {
    path = ./export.rb;
    name = "pkg-tap-export.rb";
  };
  # Bounded reader helpers (lib/tap/reader.rb). export.rb loads it via
  # require_relative, so BOTH trusted files are staged ADJACENT in the
  # build dir and the staged export.rb is invoked: a single copied
  # store file would make require_relative look for a nonexistent
  # /nix-store/reader.rb. The importer writes these two files next to
  # each other after delivery, same layout.
  readerScript = builtins.path {
    path = ./reader.rb;
    name = "pkg-tap-reader.rb";
  };
  probeScript = builtins.path {
    path = ./probe.sh;
    name = "pkg-tap-probe.sh";
  };
  # Controlled boot adapter (lib/tap/brew-boot-adapter.sh): performs
  # only the environment startup upstream bin/brew performs, then execs
  # the UNMODIFIED upstream brew.sh with the full environment (no
  # PATH reset, no `env -i`). See the adapter header comment.
  bootAdapter = builtins.path {
    path = ./brew-boot-adapter.sh;
    name = "pkg-tap-brew-boot-adapter.sh";
  };
  exportRuby = pkgs.ruby_4_0;
  # Pinned-nixpkgs fiddle gem for the Ruby 4 runtime. Homebrew's macOS
  # code path (extend/os/mac/sandbox.rb) requires "fiddle"; the Nix Ruby
  # does not ship that bundled gem on every platform, and Homebrew's own
  # GemSetup resets GEM_HOME/GEM_PATH to the vendored bundle (which does
  # not vendor fiddle). RUBYLIB keeps the Nix-built gem load path
  # available regardless of any GEM_HOME/GEM_PATH reset; verified
  # available as an exact pinned nixpkgs attribute (rubyPackages_4_0.fiddle
  # == ruby4.0-fiddle-1.1.8, lib + native extension included).
  fiddleGem = pkgs.rubyPackages_4_0.fiddle;
in
if errors != [ ] then
  throw "raw-export: ${builtins.concatStringsSep "; " errors}"
else
  pkgs.stdenvNoCC.mkDerivation {
    name = "pkg-tap-export-${owner}-${tapName}-${target}-${lib.strings.substring 0 12 revision}";

    # Data inputs only (copied into the build, never executed as Nix).
    inherit tapSource;

    homebrewSource = pkgs.fetchFromGitHub {
      inherit (homebrew) owner;
      inherit (homebrew) repo;
      rev = homebrew.revision;
      # Hash of the UNPACKED tree (vendor bundle included).
      inherit (homebrew) sha256;
    };

    nativeBuildInputs = with pkgs; [
      ruby_4_0
      gitMinimal
      curl
      bash
      coreutils
    ];

    # Probe contract (uppercase): lib/tap/probe.sh reads exactly these.
    CANARY_PATH = canary.path;
    CANARY_SHA256 = canary.sha256;
    CANARY_PORT = canaryPort;

    exportSource = source;
    exportOwner = owner;
    exportRepoDir = repoDir;
    exportRevision = revision;
    exportSystem = target;
    exportHomebrewRevision = homebrew.revision;

    # macOS: the sandbox profile must deny loopback networking too.
    __darwinAllowLocalNetworking = false;

    phases = [
      "probePhase"
      "exportPhase"
      "installPhase"
    ];

    # 1. Sandbox behavior probe: must run before ANY tap Ruby.
    probePhase = ''
      runHook preProbe
      bash ${probeScript}
      runHook postProbe
    '';

    # 2. Stage sources at transient paths, then execute the PINNED
    #    bin/brew (upstream shell env startup + vendored bundle) to run
    #    our reader script. No brew install lifecycle: `brew ruby` only
    #    evaluates the given script.
    exportPhase = ''
      runHook preExport
      # Native target: C on Linux (proven), en_US.UTF-8 on macOS. On macOS
      # upstream setup-locale otherwise forces LC_ALL from `locale charmap`
      # output; with the adaptation below it keeps this value as-is.
      if [ "${target}" = "aarch64-darwin" ]; then
        export LC_ALL=en_US.UTF-8
      else
        export LC_ALL=C
      fi
      export TZ=UTC
      export HOME="$NIX_BUILD_TOP/home"
      mkdir -p "$HOME"
      export HOMEBREW_CACHE="$NIX_BUILD_TOP/homebrew-cache"
      export HOMEBREW_LOGS="$NIX_BUILD_TOP/homebrew-logs"
      export HOMEBREW_TEMP="$NIX_BUILD_TOP/homebrew-temp"
      mkdir -p "$HOMEBREW_CACHE" "$HOMEBREW_TEMP"

      # Transient staging of the pinned Homebrew tree. fetchFromGitHub
      # yields the unpacked tree at the top level (vendor bundle
      # included); there is no brew-*/ wrapper directory to strip.
      stage="$NIX_BUILD_TOP/stage"
      mkdir -p "$stage"
      cp -a "$homebrewSource" "$stage/brew"
      chmod -R u+w "$stage/brew"

      # Controlled source patch #1 (the ONLY source patch): upstream utils/ruby.sh unconditionally unsets HOMEBREW_RUBY_PATH
      # and then prefers vendor portable-ruby (which does not exist on
      # Nix). Keep a pre-supplied, version-valid HOMEBREW_RUBY_PATH (the
      # Nix Ruby) when no vendored portable ruby is staged. Everything
      # else in setup-ruby-path (vendored bundle via GEM_HOME,
      # BUNDLE_GEMFILE, brew ruby dispatch) stays upstream.
      ruby_sh="$stage/brew/Library/Homebrew/utils/ruby.sh"
      grep -q '^  unset HOMEBREW_RUBY_PATH$' "$ruby_sh" || {
        echo "raw-export: utils/ruby.sh no longer matches the pinned unset HOMEBREW_RUBY_PATH line" >&2
        exit 1
      }
      sed -i 's|^  unset HOMEBREW_RUBY_PATH$|  if [[ -n "''${HOMEBREW_RUBY_PATH:-}" ]] \&\& test_ruby "''${HOMEBREW_RUBY_PATH}" \&\& [[ ! -x "''${vendor_ruby_path}" ]]\n  then\n    # pkg boot adapter: prefer the supplied (Nix) Ruby over PATH search\n    # and the absent portable-ruby; setup-ruby-path keeps it as-is.\n    return 0\n  fi\n  unset HOMEBREW_RUBY_PATH|' "$ruby_sh"
      grep -q 'pkg boot adapter: prefer the supplied' "$ruby_sh" || {
        echo "raw-export: the controlled utils/ruby.sh adaptation did not apply" >&2
        exit 1
      }
      # Controlled source patch #2: upstream
      # extend/os/mac sandbox.rb requires "fiddle" at load time on macOS;
      # RUBYLIB (set above) supplies the Nix-built gem. No tree change is
      # needed for that. The remaining macOS startup noise is
      # utils/os.sh setup-locale: its macOS branch runs `locale charmap`
      # without checking that a `locale` binary exists. Pinned nixpkgs has
      # NO `locale` program for darwin (darwin.locale is data only), and
      # host paths must not be added, so mirror upstream's own Linux-branch
      # guard: when `locale` is absent, keep the builder-set LC_ALL instead
      # of running a missing command. Exact-line guard + marker + bash -n,
      # same as adaptation #1.
      os_sh="$stage/brew/Library/Homebrew/utils/os.sh"
      grep -q 'if \[\[ -z "''${LC_ALL:-''${LC_CTYPE:-''${LANG:-}}}" \]\] || \[\[ "$(locale charmap)" != "UTF-8" \]\]' "$os_sh" || {
        echo "raw-export: utils/os.sh no longer matches the pinned macOS setup-locale line" >&2
        exit 1
      }
      sed -i 's@^    if \[\[ -z "''${LC_ALL:-''${LC_CTYPE:-''${LANG:-}}}" \]\] || \[\[ "$(locale charmap)" != "UTF-8" \]\]$@    if [[ -z "''${LC_ALL:-''${LC_CTYPE:-''${LANG:-}}}" ]] || { command -v locale >/dev/null 2>\&1 \&\& [[ "$(locale charmap)" != "UTF-8" ]]; }@' "$os_sh"
      grep -q ']] || { command -v locale' "$os_sh" || {
        echo "raw-export: the controlled utils/os.sh adaptation did not apply" >&2
        exit 1
      }
      bash -n "$os_sh" || {
        echo "raw-export: patched utils/os.sh is not valid bash" >&2
        exit 1
      }

      bash -n "$ruby_sh" || {
        echo "raw-export: patched utils/ruby.sh is not valid bash" >&2
        exit 1
      }

      # fiddle RUBYLIB (keeps the Nix-built fiddle
      # gem (lib + compiled extension) loadable AFTER Homebrew GemSetup
      # resets GEM_HOME/GEM_PATH to the vendored bundle: export RUBYLIB
      # with the gem's lib dir (fiddle.rb + fiddle.so live there). RUBYLIB
      # is a Ruby-native load path; upstream only unsets it inside the
      # compiler shim (shims/super/cc), which this run never executes.
      # Resolved at build time (no hardcoded gem version in a store path).
      fiddle_lib="$(find "${fiddleGem}/lib/ruby/gems" -type d -path '*/gems/fiddle-*/lib' | head -n1)"
      [ -n "$fiddle_lib" ] || {
        echo "raw-export: fiddle gem lib dir not found under ${fiddleGem}" >&2
        exit 1
      }
      [ -f "$fiddle_lib/fiddle.rb" ] || {
        echo "raw-export: fiddle.rb missing in $fiddle_lib" >&2
        exit 1
      }
      [ -f "$fiddle_lib/fiddle.so" ] || [ -f "$fiddle_lib/fiddle.bundle" ] || {
        echo "raw-export: fiddle native extension missing in $fiddle_lib" >&2
        exit 1
      }
      export RUBYLIB="$fiddle_lib"

      # Full tap layout inside the staged tree.
      tap_stage="$stage/brew/Library/Taps/$exportOwner"
      mkdir -p "$tap_stage"
      cp -a "$tapSource" "$tap_stage/$exportRepoDir"
      chmod -R u+w "$tap_stage/$exportRepoDir"

      mkdir -p "$stage/prefix"
      export HOMEBREW_PREFIX="$stage/prefix"
      export HOMEBREW_LIBRARY="$stage/brew/Library"
      export HOMEBREW_RUBY_PATH="${exportRuby}/bin/ruby"
      export HOMEBREW_CURL_PATH="${pkgs.curl}/bin/curl"
      export HOMEBREW_GIT_PATH="${pkgs.git}/bin/git"
      export HOMEBREW_NO_AUTO_UPDATE="1"
      export HOMEBREW_NO_ANALYTICS="1"
      export HOMEBREW_NO_INSTALL_FROM_API="1"
      export HOMEBREW_NO_ENV_HINTS="1"
      unset HOMEBREW_DEVELOPER
      export HOMEBREW_INTERNAL_ALLOW_PACKAGES_FROM_PATHS=1

      # Grant tap trust through UPSTREAM's own trust store (same file
      # `brew trust <source>` writes): the trusted builder pre-seeds trust for it. No DSL or
      # loader logic is bypassed; every cask still runs through the real
      # Cask::CaskLoader with its guards.
      mkdir -p "$HOME/.homebrew"
      trusted_source=$(printf '%s' "$exportSource" | tr '[:upper:]' '[:lower:]')
      printf '{"trustedtaps":["%s"]}\n' "$trusted_source" > "$HOME/.homebrew/trust.json"

      export PKG_EXPORT_TAP_PATH="$tap_stage/$exportRepoDir"
      export PKG_EXPORT_SOURCE="$exportSource"
      export PKG_EXPORT_REVISION="$exportRevision"
      export PKG_EXPORT_HOMEBREW_PIN="$exportHomebrewRevision"
      export PKG_EXPORT_SYSTEM="$exportSystem"
      export PKG_EXPORT_OUT="$NIX_BUILD_TOP/export.json"
      export PKG_EXPORT_PROBE="denied: host-read, host-write, loopback"

      # Bounds (defense in depth; export.rb enforces the reader-level
      # bounds itself). The parent drives this build with
      # `nix build -L --option max-build-log-size 1048576`; the exporter
      # additionally discards all incidental cask stdout/stderr in each
      # child (see export.rb), so a noisy cask cannot fill daemon logs.
      #   * wall clock: 900 s, SIGKILL after 10 s grace — a Ruby that
      #     traps SIGTERM cannot outlive the kill-after
      #   * file writes: 64 MiB coarse ulimit backstop (the real caps
      #     are the 32 MiB envelope caps enforced by export.rb)
      #   * 8 GiB address space (where supported)
      ulimit -f 131072
      ulimit -v 8388608 2>/dev/null || true

      cd "$NIX_BUILD_TOP"
      # Stage export.rb + reader.rb ADJACENT under the build dir so
      # require_relative "reader" in export.rb resolves (see readerScript
      # above); invoke the STAGED copy, never the single store file.
      cp "${exportScript}" "$NIX_BUILD_TOP/export.rb"
      cp "${readerScript}" "$NIX_BUILD_TOP/reader.rb"
      # Start upstream through the controlled boot adapter (upstream
      # bin/brew resets PATH to host paths and `env -i`-strips every
      # PKG_EXPORT_* variable; /usr/bin/env does not exist on Nix
      # Linux). The adapter keeps the full environment (PATH,
      # PKG_EXPORT_*, HOMEBREW_*) and execs the unmodified upstream
      # brew.sh, which loads the committed vendor bundle and dispatches
      # the real `brew ruby` command. --kill-after=10s guarantees the
      # whole build dies even if the Ruby traps SIGTERM.
      timeout --kill-after=10s 900s bash "${bootAdapter}" ruby "$NIX_BUILD_TOP/export.rb"
      [ -s "$PKG_EXPORT_OUT" ] || {
        echo "raw-export: exporter produced no output file" >&2
        exit 1
      }
      # Final checked byte cap: 32 MiB, the same cap the Rust consumer
      # enforces (export.rb refuses to build an oversized envelope;
      # this is the builder-side backstop on the written file).
      bytes=$(wc -c < "$PKG_EXPORT_OUT")
      if [ "$bytes" -gt 33554432 ]; then
        echo "raw-export: export.json is $bytes bytes (limit 33554432)" >&2
        exit 1
      fi
      runHook postExport
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      cp "$NIX_BUILD_TOP/export.json" "$out/export.json"
      printf '%s\n' "$exportSource" "$exportRevision" "$exportHomebrewRevision" "$exportSystem" \
        > "$out/provenance.txt"
      runHook postInstall
    '';

    passthru = {
      inherit source revision;
      system = target;
      result = "$out/export.json";
    };
  }
