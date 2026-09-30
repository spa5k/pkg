# Generic cask-plan builders (see ./README.md for provenance).
#
# Data-driven only: every input comes from a generated `pkg-cask-catalog/4`
# plan. Vendor strings never reach the shell: the plan travels as JSON into
# the stdlib Python helper (./plan.py) which does sniffing, pre-write member
# and link validation, extraction, artifact placement, plist checks, and the
# post-install audit. One derivation shape covers app/binary/pkg/manpage and
# completion artifacts; AppImages go through Nixpkgs `appimageTools.wrapType2`.
#
# Ported from brew-nix at 16131ae4126c54b1502aa7eaf6573d7fbf16b656 (MIT; see
# ./brew-nix-LICENSE): archive dispatch, DMG volume nesting, bundle
# placement, binary linking, xar/pbzx/cpio pkg extraction.
{
  pkgs,
  lib ? pkgs.lib,
}:

let
  inherit (lib) optionals;
  inherit (lib.strings) escapeShellArg;

  planPy = pkgs.writeText "cask-plan.py" (builtins.readFile ./plan.py);

  # Both vendor-source network fetches go through the shared safe-fetch
  # fixed-output derivation (SSRF-guarded HTTPS, flat hash contract).
  publicFetch = import ./public-fetch.nix { inherit pkgs; };

  # Bounded generic runtime-library closure for Linux dynamic ELF patching.
  # autoPatchelf fails naming any soname outside this list; no host provision.
  linuxRuntimeLibs =
    with pkgs;
    optionals stdenv.hostPlatform.isLinux [
      stdenv.cc.cc.lib # glibc + dynamic loader
      glib
      zlib
      bzip2
      xz
      openssl
    ];

  stdenv = if pkgs.stdenv.hostPlatform.isLinux then pkgs.stdenv else pkgs.stdenvNoCC;

  # version strings come from vendor metadata; sanitize for store names.
  safeVersion =
    v:
    let
      cleaned = builtins.replaceStrings [ " " "/" ":" ] [ "-" "" "" ] (toString v);
    in
    if cleaned == "" then "unknown" else cleaned;

  commonMeta =
    {
      token,
      description,
      homepage,
      system,
    }:
    {
      description =
        if description != null && description != "" then
          description
        else
          "Homebrew cask ${token} (native Nix build)";
      homepage = if homepage != null then homepage else "https://example.invalid/";
      sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
      license = lib.licenses.unfree;
      platforms = [ system ];
    };

  # Display name for bare-file payloads; only used for sniff naming.
  sourceName = plan: if plan.source ? url then baseNameOf plan.source.url else "payload";

  # The one derivation for archive kinds `auto` and `raw-binary`.
  buildPlan =
    {
      token,
      system,
      plan,
      version,
      description ? null,
      homepage ? null,
      baseline,
      pnameSuffix ? "",
    }:
    let
      # Contract source is {url,sha256} through the safe public fetch
      # FOD. `file` is a verification-harness-only override
      # (tools/cask-build-check): a plain derivation reference used as
      # src; generated catalogs never emit it.
      src =
        plan.source.file or (publicFetch {
          url = plan.source.url;
          sha256 = plan.source.sha256;
        });
      planFile = pkgs.writeText "install-plan.json" (
        builtins.toJSON {
          inherit
            token
            system
            plan
            baseline
            ;
        }
      );
    in
    stdenv.mkDerivation {
      pname = "cask-${token}${pnameSuffix}";
      version = safeVersion version;

      inherit src;

      dontUnpack = true;
      dontConfigure = true;
      dontBuild = true;
      # Native vendor payloads (macOS bundles) must stay byte-identical:
      # no strip, no shebang patching, no libtool pruning, no config-script
      # rewriting — any of these invalidates code signatures.
      dontStrip = true;
      dontPatchShebangs = true;
      dontPruneLibtoolFiles = true;
      dontUpdateAutotoolsGnuConfigScripts = true;

      nativeBuildInputs =
        with pkgs;
        [
          python3
          file
          unzip
          libarchive
          _7zz
          xar
          cpio
          gzip
          pbzx
        ]
        ++ optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
      buildInputs = linuxRuntimeLibs;

      installPhase = ''
        set -euo pipefail
        staging="$(mktemp -d)"
        rawName=""
        fallbackName=""
        ${
          let
            binaries = builtins.filter (a: a.kind == "binary") plan.artifacts;
          in
          if plan.archive.kind == "raw-binary" then
            (
              if builtins.length binaries == 1 then
                "rawName=${escapeShellArg (builtins.head binaries).source}"
              else
                throw "cask-${token}: raw-binary plans need exactly one binary artifact"
            )
          else if builtins.length binaries == 1 then
            "fallbackName=${escapeShellArg (builtins.head binaries).source}"
          else
            ""
        }
        python3 ${planPy} unpack "$src" "$staging" \
          ${escapeShellArg (sourceName plan)} "$rawName" "$fallbackName"
        python3 ${planPy} install "${toString planFile}" "$staging" "$out"
      '';

      meta =
        commonMeta {
          inherit
            token
            description
            homepage
            system
            ;
        }
        // {
          mainProgram =
            let
              bins = builtins.filter (a: a.kind == "binary") plan.artifacts;
            in
            if bins == [ ] then token else (builtins.head bins).target;
        };
    };

  # AppImage plans: pinned Nixpkgs appimageTools.wrapType2 only (API verified
  # against nixpkgs 567a49d1: pname+version required; build-time extraction is
  # libarchive via appimage-exec -x; runtime is the bubblewrap FHS env).
  # Launcher name = the metadata-derived target. Extra artifacts reject clear.
  buildAppImagePlan =
    {
      token,
      system,
      plan,
      version,
      description ? null,
      homepage ? null,
      pnameSuffix ? "",
    }:
    let
      images = builtins.filter (a: a.kind == "appimage") plan.artifacts;
      image =
        if builtins.length images != 1 then
          throw "cask-${token}: appimage plans need exactly one appimage artifact"
        else
          builtins.head images;
      others = builtins.filter (a: a.kind != "appimage") plan.artifacts;
      # wrapType2's launcher is bin/<pname>. The exposed launcher name is
      # exactly the metadata-derived target; the store name stays the token.
      # A launcher must be one safe POSIX path component: a non-empty string
      # that is neither "." nor ".." and contains no "/" or control
      # characters. Spaces, quotes, dollar signs, backticks, semicolons,
      # leading dashes, and Unicode are legitimate in vendor names, so they
      # are preserved verbatim — the shell below only ever sees the value
      # through escapeShellArg, and `mv --` defeats a leading dash.
      launcher =
        let
          t = image.target;
        in
        if !builtins.isString t then
          throw "cask-${token}: appimage artifact target must be a string"
        else if t == "" then
          throw "cask-${token}: appimage artifact needs a target"
        else if t == "." || t == ".." then
          throw "cask-${token}: appimage target is not a safe launcher name: ${t}"
        else if builtins.match ".*[[:cntrl:]/].*" t != null then
          throw "cask-${token}: appimage target is not a safe launcher name: ${t}"
        else
          t;
      pkg = "cask-${token}${pnameSuffix}";
    in
    if others != [ ] then
      throw "cask-${token}: appimage plans support only the appimage artifact (got ${
        toString (map (a: a.kind) others)
      })"
    else
      pkgs.appimageTools.wrapType2 {
        pname = pkg;
        version = safeVersion version;
        src = publicFetch {
          url = plan.source.url;
          sha256 = plan.source.sha256;
        };
        # rename the helper launcher to the exact metadata target; both
        # operands are single-quoted shell words (never double-quoted
        # interpolation, which would allow injection from the name)
        extraInstallCommands = ''
          mv -- "$out/bin/"${escapeShellArg pkg} "$out/bin/"${escapeShellArg launcher}
        '';
        meta =
          commonMeta {
            inherit
              token
              description
              homepage
              system
              ;
          }
          // {
            mainProgram = launcher;
          };
      };
in
{
  # Entry point used by the flake. `plan` comes from the generated catalog;
  # `version`/`description`/`homepage` are the entry's effective merged
  # values passed explicitly by the flake (the plan has none of them).
  buildEntry =
    {
      token,
      system,
      plan,
      version,
      description ? null,
      homepage ? null,
      baseline,
      # Source-qualified suffix appended to the store name so entries
      # from different taps never collide (raw tap imports).
      pnameSuffix ? "",
    }:
    let
      kind = plan.archive.kind;
    in
    if kind == "appimage" then
      buildAppImagePlan {
        inherit
          token
          system
          plan
          version
          description
          homepage
          pnameSuffix
          ;
      }
    else if kind == "auto" || kind == "raw-binary" then
      buildPlan {
        inherit
          token
          system
          plan
          version
          description
          homepage
          baseline
          pnameSuffix
          ;
      }
    else
      throw "cask-${token}: unresolved archive kind: ${toString kind}";
}
