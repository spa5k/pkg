# Focused builder verification: drives lib/builders.nix over fixture plans
# (source.file = fixture derivation; generated catalogs use url+sha256) and
# over one real URL plan. Positive cases are derivations; expected-failure
# cases are listed for run.sh, which asserts nonzero exit + message.
{
  pkgs ?
    import
      (fetchTarball "https://github.com/NixOS/nixpkgs/archive/567a49d1913ce81ac6e9582e3553dd90a955875f.tar.gz")
      { config.allowUnfree = true; },
}:
let
  builders = import ../../nix/casks/lib/builders.nix { inherit pkgs; };
  fx = import ./fixtures.nix { inherit pkgs; };

  common = {
    system = "x86_64-linux";
    baseline = "15.7.7";
    description = "cask build check";
    homepage = "https://example.invalid/";
  };
  appArtifacts = [
    {
      kind = "app";
      source = "HI.app";
      target = "Renamed.app";
    }
    {
      kind = "binary";
      source = "$APPDIR/HI.app/Contents/MacOS/hi";
      target = "hicli";
    }
    {
      kind = "binary";
      source = "$APPDIR/HI.app/Contents/Resources/cur";
      target = "hicur";
    }
    {
      kind = "manpage";
      source = "hi.1";
      target = "hicli.1";
    }
    {
      kind = "bash-completion";
      source = "hi.bash";
      target = "hicli";
    }
    {
      kind = "zsh-completion";
      source = "hi.zsh";
      target = "_hicli";
    }
    {
      kind = "fish-completion";
      source = "hi.fish";
      target = "hicli.fish";
    }
  ];

  filePlan = file: artifacts: {
    source.file = file;
    archive.kind = "auto";
    inherit artifacts;
    minMacos = null;
  };

  mk =
    name: plan:
    builders.buildEntry (
      common
      // {
        inherit plan;
        token = name;
        version = "1.0";
      }
    );
  expectFailList = [
    {
      name = "scratch-evil-traversal";
      pattern = "unsafe archive member";
      plan = filePlan fx.evil-traversal [
        {
          kind = "binary";
          source = "evil.txt";
          target = "evil";
        }
      ];
    }
    {
      name = "scratch-evil-link";
      pattern = "above staging root";
      plan = filePlan fx.evil-link [
        {
          kind = "app";
          source = "HI.app";
          target = "HI.app";
        }
      ];
    }
    {
      name = "scratch-min26";
      pattern = "requires macOS 26";
      plan = filePlan fx.min26-app [
        {
          kind = "app";
          source = "HI.app";
          target = "HI.app";
        }
      ];
    }
    {
      name = "scratch-macho";
      pattern = "Mach-O";
      plan = {
        source.file = fx.macho-bin;
        archive.kind = "raw-binary";
        artifacts = [
          {
            kind = "binary";
            source = "macho.bin";
            target = "macho";
          }
        ];
        minMacos = null;
      };
    }
    {
      name = "scratch-pkg-scripts";
      pattern = "installer scripts";
      plan = filePlan fx.evil-scripts-pkg [
        {
          kind = "pkg";
          source = "evil-scripts.pkg";
          target = null;
        }
      ];
    }
    {
      name = "scratch-pkg-dist";
      pattern = "Distribution installer logic";
      plan = filePlan fx.evil-dist-pkg [
        {
          kind = "pkg";
          source = "evil-dist.pkg";
          target = null;
        }
      ];
    }
    {
      name = "scratch-pkg-sys";
      pattern = "system component";
      plan = filePlan fx.evil-sys-pkg [
        {
          kind = "pkg";
          source = "evil-sys.pkg";
          target = null;
        }
      ];
    }
    {
      name = "scratch-dangling";
      pattern = "missing file";
      plan = filePlan fx.good-app [
        {
          kind = "app";
          source = "HI.app";
          target = "Renamed.app";
        }
        {
          kind = "binary";
          source = "$APPDIR/HI.app/Contents/MacOS/nope";
          target = "nope";
        }
      ];
    }
    {
      name = "scratch-dyn-elf-missing";
      pattern = "libpkg-intentionally-missing";
      plan = {
        source.file = fx.dyn-elf-missing;
        archive.kind = "raw-binary";
        artifacts = [
          {
            kind = "binary";
            source = "tool";
            target = "dynmiss";
          }
        ];
        minMacos = null;
      };
    }
  ];

  # dynamic ELF pair: host interpreter/rpath repaired generically and the
  # binary runs with store-only runtime paths
  dynelfPkg = mk "scratch-dyn-elf" {
    source.file = fx.dyn-elf-ok;
    archive.kind = "raw-binary";
    artifacts = [
      {
        kind = "binary";
        source = "tool";
        target = "dynelf";
      }
    ];
    minMacos = null;
  };

in
{
  op = builders.buildEntry (
    common
    // {
      token = "scratch-op";
      version = "2.39.0";
      plan = {
        source = {
          url = "https://cache.agilebits.com/dist/1P/op2/pkg/v2.39.0/op_linux_amd64_v2.39.0.zip";
          sha256 = "6fba7f376b6c6dec49f41b06408930a43ad064cce103c6a2ce5b3d0413a86434";
        };
        archive.kind = "auto";
        artifacts = [
          {
            kind = "binary";
            source = "op";
            target = "op";
          }
        ];
        minMacos = null;
      };
    }
  );

  # normal directories, renamed app, $APPDIR mapping to the RENAMED target,
  # in-tree link, DMG shortcut omission, completions, manpage
  app = mk "scratch-app" (filePlan fx.good-app appArtifacts);

  # relocatable pkg: bundle payload inside Applications/
  pkg = mk "scratch-pkg" (
    filePlan fx."good-pkg" [
      {
        kind = "pkg";
        source = "good.pkg";
        target = null;
      }
    ]
  );

  dynelf = dynelfPkg;

  dynelf-verify = pkgs.runCommand "check-dynelf" { nativeBuildInputs = with pkgs; [ patchelf ]; } ''
    set -euo pipefail
    exe="${dynelfPkg}/bin/dynelf"
    out_line="$($exe)"
    case "$out_line" in
      ZLIB_OK*) ;;
      *) echo "run failed: $out_line" >&2; exit 1 ;;
    esac
    interp="$(patchelf --print-interpreter "$exe")"
    case "$interp" in
      /nix/store/*) ;;
      *) echo "interpreter not in store: $interp" >&2; exit 1 ;;
    esac
    while IFS= read -r p; do
      case "$p" in
        /nix/store/*|"") ;;
        *) echo "runpath leaves store: $p" >&2; exit 1 ;;
      esac
    done < <(patchelf --print-rpath "$exe" | tr ':' '\n')
    echo "interpreter=$interp run=$out_line" > "$out"
  '';

  expectFail = expectFailList;
  expectDrv = builtins.listToAttrs (
    map (e: {
      inherit (e) name;
      value = mk e.name e.plan;
    }) expectFailList
  );
}
