# Builder verification fixtures: one small derivation per archive. Malicious
# members/links, a fake Mach-O, fake bundles, relocatable and hostile pkgs.
# stdlib python3 + bsdtar only; no network; deterministic contents.
{
  pkgs ?
    import
      (fetchTarball "https://github.com/NixOS/nixpkgs/archive/567a49d1913ce81ac6e9582e3553dd90a955875f.tar.gz")
      { },
}:
let
  mkFixture =
    name: script:
    pkgs.runCommand "cask-fixture-${name}"
      {
        nativeBuildInputs = with pkgs; [
          python3
          libarchive
        ];
      }
      ''
        set -euo pipefail
        export outname="${name}"
        ${script}
      '';

  zipLib = ''
    import os, stat, zipfile
    out = os.environ["out"]  # single-file output: the archive itself

    def zip_symlink(zf, arcname, target):
        zi = zipfile.ZipInfo(arcname)
        zi.external_attr = (stat.S_IFLNK | 0o777) << 16
        zf.writestr(zi, target)

    def plist(min_ver=None):
        keys = [("CFBundleName", "HI"), ("CFBundleExecutable", "hi")]
        if min_ver:
            keys.append(("LSMinimumSystemVersion", min_ver))
        return ("<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>"
                + "".join(f"<key>{k}</key><string>{v}</string>" for k, v in keys)
                + "</dict></plist>").encode()

    def app_files(min_ver=None):
        return {
            "HI.app/Contents/Info.plist": plist(min_ver),
            "HI.app/Contents/MacOS/hi": b"#!/bin/sh\necho hi-from-app\n",
            "HI.app/Contents/Resources/tool": b"#!/bin/sh\necho tool\n",
        }

    def write(files, symlinks=None):
        with zipfile.ZipFile(out, "w") as zf:
            for arcname, data in files.items():
                zi = zipfile.ZipInfo(arcname)
                zi.external_attr = (0o755 if "MacOS/" in arcname or arcname.endswith((".sh", "tool")) else 0o644) << 16
                zf.writestr(zi, data)
            for arcname, target in (symlinks or {}).items():
                zip_symlink(zf, arcname, target)
  '';

  pkgLib = ''
    import gzip, os, subprocess
    out = os.environ["out"]  # single-file output: the pkg itself

    def plist(min_ver=None):
        keys = [("CFBundleName", "HI"), ("CFBundleExecutable", "hi")]
        if min_ver:
            keys.append(("LSMinimumSystemVersion", min_ver))
        return ("<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>"
                + "".join(f"<key>{k}</key><string>{v}</string>" for k, v in keys)
                + "</dict></plist>").encode()

    def app_files(min_ver=None):
        return {
            "Applications/HI.app/Contents/Info.plist": plist(min_ver),
            "Applications/HI.app/Contents/MacOS/hi": b"#!/bin/sh\necho hi-from-pkg\n",
        }

    def make_pkg(extra_files=None, payload_tree=None):
        stage = os.path.join(os.environ["NIX_BUILD_TOP"], "stage")
        subprocess.run(["rm", "-rf", stage], check=True)
        os.makedirs(os.path.join(stage, "tree"), exist_ok=True)
        tree = os.path.join(stage, "tree")
        for n, data in (payload_tree or {}).items():
            p = os.path.join(tree, n)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as g:
                g.write(data)
        cpio = subprocess.run(["bsdtar", "-cf", "-", "--format=cpio", "."],
                              cwd=tree, capture_output=True, check=True).stdout
        with open(os.path.join(stage, "Payload"), "wb") as f:
            f.write(gzip.compress(cpio))
        with open(os.path.join(stage, "Bom"), "wb") as f:
            f.write(b"BOM000000")
        for n, data in (extra_files or {}).items():
            p = os.path.join(stage, n)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as g:
                g.write(data)
        # archive only Payload/Bom/extra files, never the cpio source tree
        subprocess.run(["bsdtar", "-cf", out, "--format=xar",
                        "Payload", "Bom"]
                       + list((extra_files or {}).keys()),
                      cwd=stage, check=True)
  '';

  pyRun = lib: body: ''
    cat > make.py <<'EOF'
    ${lib}
    ${body}
    EOF
    python3 make.py
  '';
in
{
  good-app = mkFixture "good-app.zip" (
    pyRun zipLib ''
      files = {f"HI 1.0/{k}": v for k, v in app_files().items()}
      files["hi.1"] = b".TH HI 1\n"
      files["hi.bash"] = b"# bash completion\n"
      files["hi.zsh"] = b"#compdef hi\n"
      files["hi.fish"] = b"# fish completion\n"
      # standard DMG shortcut (top-level, absolute: omitted) + legit in-tree link
      write(files, {"Applications": "/Applications",
                 "HI 1.0/HI.app/Contents/Resources/cur": "tool"})
    ''
  );

  min26-app = mkFixture "min26-app.zip" (
    pyRun zipLib ''
      write(app_files("26.0"))
    ''
  );

  evil-traversal = mkFixture "evil-traversal.zip" (
    pyRun zipLib ''
      with zipfile.ZipFile(out, "w") as zf:
          zf.writestr("../evil.txt", "no")
    ''
  );

  evil-link = mkFixture "evil-link.zip" (
    pyRun zipLib ''
      write(app_files(),
            {"HI.app/Contents/Resources/out": "../../../../outside"})
    ''
  );

  macho-bin = mkFixture "macho.bin" ''
    printf '\376\355\372\317' > "$out"
    head -c 100 /dev/zero >> "$out"
  '';

  good-pkg = mkFixture "good.pkg" (
    pyRun pkgLib ''
      make_pkg(payload_tree=app_files("11.0"))
    ''
  );

  evil-scripts-pkg = mkFixture "evil-scripts.pkg" (
    pyRun pkgLib ''
      make_pkg(extra_files={"Scripts": gzip.compress(b"#!/bin/sh\nexit 0")},
               payload_tree={"HI.app/Contents/Info.plist": plist("11.0")})
    ''
  );

  evil-dist-pkg = mkFixture "evil-dist.pkg" (
    pyRun pkgLib ''
      make_pkg(extra_files={"Distribution": b"<?xml?><installer-script/>"},
               payload_tree={"HI.app/Contents/Info.plist": plist("11.0")})
    ''
  );

  evil-sys-pkg = mkFixture "evil-sys.pkg" (
    pyRun pkgLib ''
      make_pkg(payload_tree={"Library/LaunchDaemons/evil.plist": b"launchd",
                             "HI.app/Contents/Info.plist": plist("11.0")})
    ''
  );
  # dynamic ELF with a HOST-style interpreter and no rpath - what vendor
  # Linux binaries look like; bytes generated with the pinned toolchain,
  # no host library guessing
  dyn-elf-ok =
    pkgs.runCommand "cask-fixture-dyn-elf"
      {
        nativeBuildInputs = with pkgs; [
          stdenv.cc
          patchelf
          zlib
        ];
      }
      ''
        set -euo pipefail
        printf '#include <stdio.h>\n#include <zlib.h>\nint main(void){printf("ZLIB_OK %%s\\n", zlibVersion());return 0;}\n' > t.c
        $CC t.c -lz -o "$out"
        patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath "$out"
      '';

  # dynamic ELF that NEEDS libpkg-intentionally-missing.so: the build must
  # fail naming that soname, never silently ignore it
  dyn-elf-missing =
    pkgs.runCommand "cask-fixture-dyn-elf-missing"
      {
        nativeBuildInputs = with pkgs; [
          stdenv.cc
          patchelf
          zlib
        ];
      }
      ''
        set -euo pipefail
        printf '#include <stdio.h>\n#include <zlib.h>\nint main(void){printf("ZLIB_OK %%s\\n", zlibVersion());return 0;}\n' > t.c
        $CC t.c -lz -o "$out"
        patchelf --add-needed libpkg-intentionally-missing.so "$out"
      '';
}
