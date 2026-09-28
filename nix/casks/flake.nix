{
  description = "pkg Cask source: a supported subset of Homebrew Casks as native Nix packages, without Brew";

  inputs = {
    # Converter, pinned to the inspected revision (see
    # docs/research/2026-09-27-homebrew-casks.md).
    brew-nix = {
      url = "github:BatteredBunny/brew-nix/16131ae4126c54b1502aa7eaf6573d7fbf16b656";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.brew-api.follows = "brew-api";
      inputs.nix-darwin.follows = "nix-darwin";
    };

    # Cask metadata, pinned explicitly. Upstream warns its default can be
    # stale; this flake never relies on the default.
    brew-api = {
      url = "github:BatteredBunny/brew-api/245947c0b920cbe83f4003bb7c6be737352c8314";
      flake = false;
    };

    # Same Nixpkgs revision the converter was locked against upstream.
    nixpkgs.url = "github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f";

    # Only consumed by brew-nix's own checks output; pinned for completeness.
    nix-darwin = {
      url = "github:nix-darwin/nix-darwin/a1fa429e945becaf60468600daf649be4ba0350c";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      brew-api,
      nix-darwin,
      brew-nix,
    }:
    let
      system = "aarch64-darwin";

      lib = nixpkgs.lib;

      # Same metadata revision the converter itself reads; no IFD, plain JSON.
      caskData = builtins.fromJSON (builtins.readFile (brew-api + "/cask.json"));
      byToken = builtins.listToAttrs (map (c: {
        name = c.token;
        value = c;
      }) caskData);

      # "target" is a link-destination modifier, not an artifact kind.
      artifactKinds = cask:
        lib.unique (
          lib.filter (k: k != "target") (lib.concatMap builtins.attrNames (cask.artifacts or [ ]))
        );
      hasKind = cask: k: lib.elem k (artifactKinds cask);

      # Artifact kinds that are inert for archive extraction. `zap` and
      # `uninstall` are cleanup lifecycle data; pkg does not execute them.
      inertKinds = [
        "app"
        "binary"
        "zap"
        "uninstall"
        "generate_completions_from_executable"
      ];

      classify =
        cask:
        let
          kinds = artifactKinds cask;
          sha = cask.sha256 or null;
          unsupported = builtins.filter (k: !lib.elem k inertKinds) kinds;
        in
        if lib.elem "pkg" kinds then
          { supported = false; reason = "native-installer"; }
        else if lib.elem "installer" kinds then
          { supported = false; reason = "installer-script"; }
        else if unsupported != [ ] then
          {
            supported = false;
            reason = "unsupported-artifact";
            detail = "artifact kinds: ${toString unsupported}";
          }
        else if sha == null || sha == "no_check" then
          { supported = false; reason = "missing-checksum"; }
        else if !(cask ? url) || cask.url == null then
          {
            supported = false;
            reason = "missing-url";
            detail = "aarch64-darwin uses the top-level cask url";
          }
        else if (hasKind cask "app") && (hasKind cask "binary") then
          { supported = true; kind = "app+cli"; }
        else if hasKind cask "app" then
          { supported = true; kind = "app"; }
        else if hasKind cask "binary" then
          { supported = true; kind = "binary"; }
        else
          { supported = false; reason = "no-installable-artifact"; };

      # Locked source revisions, read from the actual inputs. Publishing
      # hand-copied pins here would drift from `flake.lock` on updates.
      lockedRev =
        name: input:
        input.rev or (throw "nix/casks: input ${name} is not locked to an exact revision");

      # The CLI link Homebrew intends for a cask: the first `binary`
      # artifact, named by its declared target when the metadata carries
      # one, else by its source basename. Link names are never invented
      # here; they come from the pinned cask metadata.
      caskBinaryLink =
        cask:
        let
          withBinary = builtins.filter (a: a ? binary) (cask.artifacts or [ ]);
        in
        if withBinary == [ ] then
          null
        else
          let
            items = (builtins.head withBinary).binary;
            first = builtins.head items;
            firstName =
              if builtins.isString first then baseNameOf first else first.target or (baseNameOf (first.source or ""));
            paired =
              builtins.length items > 1
              && builtins.isAttrs (builtins.elemAt items 1)
              && (builtins.elemAt items 1) ? target;
            name = if paired then (builtins.elemAt items 1).target else firstName;
            # `$APPDIR` is Homebrew's application-directory anchor; inside a
            # converted derivation the app root is `$out/Applications`.
            source =
              lib.strings.removePrefix "$APPDIR/" (
                if builtins.isString first then first else first.source or ""
              );
          in
          { inherit name source; };

      # Bounded, curated per-token derivation overrides. Nothing here is
      # user-controlled; additions require a review of this file.
      #
      # The converter defines `installPhase` as a plain string. A plain
      # string phase replaces the stdenv phase function, so `postInstall`
      # hooks never run for cask derivations; overrides must append to
      # `installPhase` itself (verified against brew-nix at the locked
      # revision and nixpkgs stdenv `runPhase`).
      overrides = {
        # Expose the CLI that the Cursor cask itself links: the pinned
        # metadata declares the binary
        # `$APPDIR/Cursor.app/Contents/Resources/app/bin/code` with link
        # name `cursor`. The build fails if that file is absent, and the
        # link replaces the converter's GUI wrapper of the same name so
        # `cursor` on PATH is the CLI Homebrew documents.
        cursor =
          let
            link = caskBinaryLink byToken.cursor;
          in
          if link == null || link.source == "" then
            throw "casks/cursor: metadata no longer declares a binary artifact"
          else
            drv:
            drv.overrideAttrs (old: {
              installPhase = (old.installPhase or "") + ''
                pkg_cli="$out/Applications/${link.source}"
                if [ ! -f "$pkg_cli" ]; then
                  echo "casks/cursor: vendored CLI is missing: $pkg_cli" >&2
                  exit 1
                fi
                mkdir -p "$out/bin"
                ln -sfn "$pkg_cli" "$out/bin/${link.name}"
              '';
            });
      };
      applyOverride = token: drv: (overrides.${token} or (d: d)) drv;

      # Initial supported list. Representatives: one app archive, one
      # standalone executable, one app with an extra CLI link.
      supportedTokens = [
        "iterm2"
        "chromedriver"
        "cursor"
      ];

      # Inspected, explicitly excluded tokens. Reasons are computed from the
      # pinned metadata at evaluation time, not copied by hand. The small
      # `inspectedExclusions` map carries reasons that metadata cannot
      # express; each one records an on-macOS verification.
      excludedTokens = [
        "zoom"
        "4peaks"
        "font-0xproto"
        "1password@nightly"
        "raycast"
      ];

      # Verified exclusions that cask metadata cannot express.
      inspectedExclusions = {
        raycast = {
          reason = "minimum-os";
          detail = "metadata ships LSMinimumSystemVersion 26.0; `open` fails with -10825 on the supported macOS 15.7.7 (verified on-device)";
        };
      };

      supportedPackages = builtins.listToAttrs (
        map (
          token:
          let
            meta = classify byToken.${token};
          in
          {
            name = token;
            value =
              if !meta.supported then
                throw "token ${token} is listed as supported but classifies as ${meta.reason}"
              else
                applyOverride token brew-nix.packages.${system}.${token};
          }
        ) supportedTokens
      );

      supportMetadata = {
        schema = "pkg-cask-support/1";
        inherit system;
        sources = {
          brew-nix = {
            url = "github:BatteredBunny/brew-nix/${lockedRev "brew-nix" brew-nix}";
            rev = lockedRev "brew-nix" brew-nix;
          };
          brew-api = {
            url = "github:BatteredBunny/brew-api/${lockedRev "brew-api" brew-api}";
            rev = lockedRev "brew-api" brew-api;
          };
          nixpkgs = {
            url = "github:NixOS/nixpkgs/${lockedRev "nixpkgs" nixpkgs}";
            rev = lockedRev "nixpkgs" nixpkgs;
          };
          nix-darwin = {
            url = "github:nix-darwin/nix-darwin/${lockedRev "nix-darwin" nix-darwin}";
            rev = lockedRev "nix-darwin" nix-darwin;
          };
        };
        supported = map (
          token:
          let
            cask = byToken.${token};
            meta = classify cask;
          in
          {
            inherit token;
            kind = meta.kind;
            version = cask.version or null;
            homepage = cask.homepage or null;
            artifactKinds = artifactKinds cask;
            override = builtins.hasAttr token overrides;
          }
        ) supportedTokens;
        excluded = map (
          token:
          let
            cask = byToken.${token};
            meta = classify cask;
            inspected = inspectedExclusions.${token} or { };
          in
          {
            inherit token;
            version = cask.version or null;
            reason = inspected.reason or meta.reason;
            detail = inspected.detail or meta.detail or null;
          }
        ) excludedTokens;
      };
    in
    {
      packages.${system} = supportedPackages;

      # JSON-valued output consumed by the pkg CLI without realization; the
      # only published form of the support list. Consistency is enforced at
      # evaluation time: `supportedPackages` throws if a curated token stops
      # classifying as supported in the pinned metadata.
      caskSupport.${system} = supportMetadata;
    };
}
