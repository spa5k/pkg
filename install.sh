#!/bin/sh
# pkg client downloader for one exact release.
#
# Downloads and verifies the standalone pkg client archive for this system,
# reads bin/pkg (plus completions) out of the archive, and installs those
# files under ~/.local.
#
# This script NEVER:
#   - installs, updates, configures, or removes Nix or any other runtime;
#   - uses sudo or installs privileged helpers or services;
#   - changes Nix trust settings or accepts new substituters or keys;
#   - removes or migrates old pkg installations.
#
# Nix must be installed separately, by you, from the vendor:
#   https://docs.determinate.systems/determinate-nix/
set -eu

# The version, tag, archive name, and checksum file must all agree with the
# release built by tools/release/package_client.sh. package_client.sh takes
# the version from pkg-cli Cargo metadata, and the client-release workflow
# rejects a tag that disagrees with that version.
repo="spa5k/pkg"
version="0.2.0-alpha.2"
tag="v${version}"

if [ "$#" -ne 0 ]; then
  echo "Usage: sh pkg-install.sh" >&2
  echo "  This downloader takes no arguments. It installs release ${tag}." >&2
  exit 2
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) system="x86_64-linux" ;;
  Darwin:arm64) system="aarch64-darwin" ;;
  *)
    echo "error: unsupported system. Supported: x86_64 Linux, Apple silicon macOS." >&2
    exit 1
    ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: missing tool: $1" >&2; exit 1; }; }
need curl
need tar
need install
need awk
need grep

# One checksum tool is enough. Check both before use: "need shasum ||
# need sha256sum" cannot express this, because need exits the script when
# the first tool is missing.
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  echo "error: missing tool: shasum or sha256sum" >&2
  exit 1
fi

archive="pkg-${version}-${system}.tar.gz"
root="pkg-${version}-${system}"
base="https://github.com/${repo}/releases/download/${tag}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${archive} from release ${tag}..."
curl -fsSL -o "$tmp/$archive" "${base}/${archive}"
curl -fsSL -o "$tmp/SHA256SUMS" "${base}/SHA256SUMS"

# Verify the archive against its published checksum.
want="$(grep " ${archive}\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
if [ -z "$want" ]; then
  echo "error: no checksum entry for ${archive} in SHA256SUMS." >&2
  exit 1
fi
if command -v shasum >/dev/null 2>&1; then
  got="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
  got="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
fi
if [ "$got" != "$want" ]; then
  echo "error: checksum mismatch for ${archive}." >&2
  echo "  expected $want" >&2
  echo "  got      $got" >&2
  exit 1
fi

# Read only the exact members this downloader installs. Every member's
# bytes go to stdout (`tar -xO`) and into a temporary file named below.
# The archive's directory tree is never extracted to disk, so archive
# paths (absolute, `..`, deep) and links cannot write outside these files.
tar -tvf "$tmp/$archive" > "$tmp/listing" || {
  echo "error: cannot list members of ${archive}." >&2
  exit 1
}

# exact_regular MEMBER: succeed only when MEMBER is stored exactly once,
# as one regular file, with nothing below MEMBER. Duplicate names, mixed
# member types, links, and directories are refused.
exact_regular() {
  awk -v m="$1" '
    {
      name = $NF
      if ($0 ~ / -> /) {
        line = $0
        sub(/ -> .*/, "", line)
        n = split(line, fields, " ")
        name = fields[n]
      }
      if (name == m) {
        named++
        if (substr($1, 1, 1) == "-") regular++
      }
      if (index($0, m "/")) below = 1
    }
    END { exit !(regular == 1 && named == 1 && !below) }
  ' "$tmp/listing"
}

# read_member MEMBER DEST: write MEMBER's bytes to DEST, or fail clearly.
read_member() {
  tar -xOzf "$tmp/$archive" -- "$1" > "$2" || {
    echo "error: tar failed while reading ${1} from ${archive}." >&2
    exit 1
  }
}

client="${root}/bin/pkg"
if ! exact_regular "$client"; then
  echo "error: ${archive} stores no single regular file ${client}." >&2
  exit 1
fi
read_member "$client" "$tmp/pkg"
if [ ! -s "$tmp/pkg" ]; then
  echo "error: ${client} from ${archive} is empty." >&2
  exit 1
fi

for shell in bash zsh fish; do
  member="${root}/completions/${shell}.txt"
  if exact_regular "$member"; then
    read_member "$member" "$tmp/${shell}.completion"
  fi
done

bindir="${PKG_INSTALL_BIN:-$HOME/.local/bin}"
compldir="${PKG_INSTALL_COMPLETIONS:-$HOME/.local/share/pkg/completions}"
mkdir -p "$bindir" "$compldir"
install -m 0755 "$tmp/pkg" "$bindir/pkg"
for shell in bash zsh fish; do
  if [ -f "$tmp/${shell}.completion" ]; then
    cp "$tmp/${shell}.completion" "$compldir/${shell}.txt"
  fi
done

echo "Installed $bindir/pkg and completions under $compldir."
echo "pkg requires an external Nix installation. pkg does not install Nix."
echo "Next steps: https://github.com/${repo}/blob/main/docs/install.md"
