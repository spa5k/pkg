#!/usr/bin/env bash
# Package the standalone pkg client (NN-08).
#
# Usage: package_client.sh <x86_64-linux|aarch64-darwin>
#
# Builds the pkg-cli release binary for the host system, generates shell
# completions, collects notices, and writes dist/:
#   pkg-<version>-<system>.tar.gz
#   SHA256SUMS-<system>
#
# The version comes from Cargo metadata for pkg-cli. The archive contains
# only the client, completions, and notices: no privileged helper, private
# Nix bundle, or product catalog asset.
set -euo pipefail

system="${1:?usage: package_client.sh <x86_64-linux|aarch64-darwin>}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

case "$system" in
  x86_64-linux)
    [[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] \
      || { echo "x86_64-linux must be packaged on x86_64 Linux" >&2; exit 1; }
    rust_triple="x86_64-unknown-linux-gnu" ;;
  aarch64-darwin)
    [[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] \
      || { echo "aarch64-darwin must be packaged on Apple silicon macOS" >&2; exit 1; }
    rust_triple="aarch64-apple-darwin" ;;
  *) echo "unknown system: $system" >&2; exit 1 ;;
esac

version="$(python3 - <<'PY'
import json, subprocess
meta = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        text=True,
    )
)
for pkg in meta["packages"]:
    if pkg["name"] == "pkg-cli":
        print(pkg["version"])
        break
else:
    raise SystemExit("pkg-cli package not found in workspace metadata")
PY
)"

echo "packaging pkg-cli ${version} for ${system}"

# Version agreement guard: install.sh pins a static version for its
# downloader. A version bump that does not update install.sh would ship a
# stale downloader, so packaging fails instead.
install_sh_version="$(sed -n 's/^version="\([^"]*\)"$/\1/p' install.sh | head -1)"
[[ -n "$install_sh_version" ]] \
  || { echo "cannot read the pinned version from install.sh" >&2; exit 1; }
[[ "$install_sh_version" == "$version" ]] \
  || { echo "install.sh pins version ${install_sh_version} but pkg-cli is ${version}; update install.sh before packaging" >&2; exit 1; }

cargo build --locked --release -p pkg-cli
bin="target/release/pkg"
test -x "$bin"

stage="$(mktemp -d)/pkg-${version}-${system}"
trap 'rm -rf "$(dirname "$stage")"' EXIT
mkdir -p "${stage}/bin" "${stage}/completions"

install -m 0755 "$bin" "${stage}/bin/pkg"

for shell in bash zsh fish; do
  "$bin" completion "$shell" > "${stage}/completions/${shell}.txt"
done

# Ship the actual license text, plus a short, accurate notice. The
# notice links only public URLs; no file that is absent from the archive.
cp LICENSE "${stage}/LICENSE"
cat > "${stage}/NOTICES.md" <<EOF
# pkg ${version} (${system}) notices

Contents: the standalone pkg client (bin/pkg), shell completions, and
third-party license texts under licenses/.

- License: Apache-2.0. The full text is in LICENSE next to this file.
- Third-party Rust dependency licenses and notices are under licenses/.
- pkg requires an external Nix installation. pkg does not install,
  update, repair, or remove Nix.
- Supported systems: x86_64 Linux and Apple silicon macOS.
- Project: https://github.com/spa5k/pkg
EOF

# Collect third-party Rust dependency license texts and notices (D9).
# Driven by the locked Cargo dependency metadata at package time, filtered
# to the crates reachable from pkg-cli on the actual Rust target being
# packaged: the resolve graph is seeded from the pkg-cli package id and
# walked over normal and build dependency edges (dev-only edges are
# excluded), so workspace siblings such as the maintainer-only cask-catalog
# generator and their dependencies can never enter the client archive. One
# licenses/<crate>-<version>/ directory per registry crate with the license,
# copying, copyright, and notice files from the unpacked crate source in the
# local registry, plus licenses/THIRDPARTY.md listing every crate with its
# version, declared license, and collected files. Crates that exist only
# for other targets (for example UEFI-only code) are not distributed and
# are not indexed. A crate with no license file and no declared license
# fails the packaging.
python3 - "$stage" "$rust_triple" <<'PY'
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

stage = Path(sys.argv[1])
rust_triple = sys.argv[2]
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--filter-platform", rust_triple,
     "--format-version", "1"], text=True))

registry_src = Path.home() / ".cargo" / "registry" / "src"
license_name = re.compile(r"^(licen[cs]e|copying|copyright|notice)", re.IGNORECASE)

packages = {p["id"]: p for p in meta["packages"]}
resolve = meta.get("resolve") or {}
nodes = {n["id"]: n for n in resolve.get("nodes", [])}

# Seed from pkg-cli itself and walk normal/build dependency edges only.
# Dev-only edges (kind == "dev") never distribute a crate, and workspace
# members are skipped as non-distributed local packages but still walked
# through so a future client-side workspace split stays honest.
seeds = [pid for pid, p in packages.items() if p["name"] == "pkg-cli"]
if not seeds:
    sys.exit("pkg-cli package not found in workspace metadata")
reachable, queue = set(), list(seeds)
while queue:
    pid = queue.pop()
    if pid in reachable:
        continue
    reachable.add(pid)
    node = nodes.get(pid)
    if not node:
        continue
    for dep in node.get("deps", []):
        kinds = [k.get("kind") for k in dep.get("dep_kinds", [])]
        if any(kind in (None, "build") for kind in kinds):
            queue.append(dep["pkg"])

rows = []
out = stage / "licenses"
for pid in sorted(reachable, key=lambda pid: (packages[pid]["name"], packages[pid]["version"])):
    pkg = packages[pid]
    source = pkg.get("source") or ""
    if not source:
        continue  # workspace member, not a distributed dependency
    if not source.startswith("registry+"):
        sys.exit(f"non-registry dependency source: {pkg['name']} {source}")
    crate_dir = Path(pkg["manifest_path"]).parent
    if registry_src not in crate_dir.parents:
        sys.exit(
            f"registry crate not unpacked locally: {pkg['name']} "
            f"{pkg['version']} at {crate_dir}; run cargo build first")

    wanted = [p for p in sorted(crate_dir.iterdir())
              if p.is_file() and license_name.match(p.name)]
    extra = pkg.get("license_file") or ""
    if extra:
        candidate = crate_dir / extra
        if candidate.is_file() and candidate not in wanted:
            wanted.append(candidate)

    dest = out / f"{pkg['name']}-{pkg['version']}"
    dest.mkdir(parents=True, exist_ok=True)
    copied = []
    for path in wanted:
        shutil.copy2(path, dest / path.name)
        copied.append(path.name)

    declared = (pkg.get("license") or "").strip()
    if not copied and not declared:
        sys.exit(
            f"no license file and no declared license for {pkg['name']} "
            f"{pkg['version']}")
    rows.append((pkg["name"], pkg["version"], declared or "(none declared)", copied))

lines = [
    "# Third-party Rust dependencies",
    "",
    f"One directory per crate holds the license and notice files copied",
    "from the crate source in the local Cargo registry. The list comes from",
    f"Cargo metadata for the locked dependency graph reachable from pkg-cli",
    f"through normal and build edges on the packaged Rust target",
    f"({rust_triple}) at package time; dev-only dependencies, workspace",
    "siblings such as maintainer tools, and crates reachable only on other",
    "targets are not distributed and are not listed. An",
    "empty file list means the crate declares a license without shipping",
    "a license file.",
    "",
    "| Crate | Version | Declared license | Files |",
    "| --- | --- | --- | --- |",
]
for name, version, declared, copied in rows:
    files = ", ".join(copied) if copied else "(no license file in source)"
    lines.append(f"| {name} | {version} | {declared} | {files} |")
(out / "THIRDPARTY.md").write_text("\n".join(lines) + "\n")
print(f"collected license files for {len(rows)} third-party crates")
PY

# Packaging isolation guard (RC-01): the archive carries client outputs
# only — no maintainer-tool binary, no generator license text, no catalog
# data. Fail closed on any staged path that names the generator.
if find "$stage" -name '*cask-catalog*' | grep -q .; then
  echo "archive would contain maintainer-tool output (cask-catalog); refusing" >&2
  exit 1
fi
binaries="$(find "${stage}/bin" -type f | wc -l)"
[[ "${binaries//[[:space:]]/}" == "1" ]] \
  || { echo "archive must contain exactly one client binary; found $binaries" >&2; exit 1; }

mkdir -p dist
out="dist/pkg-${version}-${system}.tar.gz"
tar -czf "$out" -C "$(dirname "$stage")" "$(basename "$stage")"

cd dist
case "$(uname -s)" in
  Darwin) shasum -a 256 "$(basename "$out")" > "SHA256SUMS-${system}" ;;
  *) sha256sum "$(basename "$out")" > "SHA256SUMS-${system}" ;;
esac

echo "wrote ${out}"
cat "SHA256SUMS-${system}"
