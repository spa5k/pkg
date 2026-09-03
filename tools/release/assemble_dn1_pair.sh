#!/usr/bin/env bash
# DN-1 PR-6: assemble, seal, and upload the proof pair from signed releases.
#
# Prerequisites (see mint_dn1_proof_pair.sh for the build+draft stage):
#   - drafts v0.1.0-alpha.{N,N+1} exist and are SIGNED (sigstore assets present)
#   - signing state at SIGNING_STATE with root digest c317d2ad...
#   - upstream artifacts (nix tarballs, determinate installers) available in
#     the preserved DN-16 pair tree - reused byte-exact, digest-verified
#
# Usage: assemble_dn1_pair.sh N_SEQ N_PLUS_1_SEQ PAIR_TAG UPSTREAM_PAIR_DIR SIGNING_STATE
set -euo pipefail

usage() {
    echo "usage: assemble_dn1_pair.sh N_SEQ N_PLUS_1_SEQ PAIR_TAG UPSTREAM_PAIR_DIR SIGNING_STATE" >&2
    exit 64
}

[ "$#" -eq 5 ] || usage
n_seq=$1; n1_seq=$2; pair_tag=$3; upstream=$4; signing_state=$5
[[ "$n_seq" =~ ^[1-9][0-9]*$ ]] && [[ "$n1_seq" =~ ^[1-9][0-9]*$ ]] && [ "$n_seq" -lt "$n1_seq" ] || usage
[[ "$pair_tag" =~ ^[a-z0-9][a-z0-9-]*$ ]] || usage

repo=spa5k/pkg
n_release="v0.1.0-alpha.${n_seq}"
n1_release="v0.1.0-alpha.${n1_seq}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
work="$(mktemp -d /tmp/pkg-dn1-assemble.XXXXXXXX)"
echo "==> assemble work: $work"

expected_root=c317d2ad134e0e9efe7c0e836b9b62fa386309e78fa859a516d3ecc943168dd8
[ "$(shasum -a 256 "$signing_state/1.root.json" | awk '{print $1}')" = "$expected_root" ] || {
    echo "signing-state root mismatch" >&2; exit 1; }

# ---- upstream artifacts from the preserved DN-16 pair (digest-pinned reuse)
mkdir -p "$work/runtimes" "$work/determinate"
up_n="$upstream/n"
nx() { find "$up_n/targets" -name "$1" -type f | head -1; }
copy_upstream() { # file name dest
    src=$(nx "$1")
    [ -n "$src" ] || { echo "upstream artifact missing: $1" >&2; exit 1; }
    install -m 0644 "$src" "$2"
    # verify digest matches the target dir name it lives in
    d=$(basename "$(dirname "$(dirname "$src")")" | cut -d. -f1)
    [ "$(shasum -a 256 "$src" | awk '{print $1}')" = "$d" ] || {
        echo "upstream digest mismatch: $1" >&2; exit 1; }
}
copy_upstream aarch64-darwin.tar.xz "$work/runtimes/aarch64-darwin.tar.xz"
copy_upstream x86_64-linux.tar.xz   "$work/runtimes/x86_64-linux.tar.xz"
for f in nix-installer-aarch64-darwin nix-installer-aarch64-linux \
         nix-installer-x86_64-linux nix-installer-v3.22.1.tar.gz LICENSE; do
    copy_upstream "$f" "$work/determinate/$f"
done
echo "==> upstream artifacts staged and digest-verified"

# ---- per-side: download signed draft assets, prepare manifest, publish channel
side() { # release side seq
    local release=$1 side_name=$2 seq=$3
    local version=${release#v}
    local dir="$work/$side_name"
    local a="$dir/aarch64-input" x="$dir/x86_64-input"
    mkdir -p "$a" "$x/determinate"

    echo "==> [$side_name] downloading signed assets of $release"
    for f in "pkg-aarch64-darwin" "pkg-${version}-preview.pkg"; do
        gh release download "$release" --repo "$repo" -p "$f" -O "$a/$f" --clobber
    done
    for f in pkg-x86_64-linux pkg-installer-x86_64-linux; do
        gh release download "$release" --repo "$repo" -p "$f" -O "$x/$f" --clobber
    done
    for f in "pkg-aarch64-darwin.sigstore.json" "pkg-${version}-preview.pkg.sigstore.json"; do
        gh release download "$release" --repo "$repo" -p "$f" -O "$a/$f" --clobber
    done
    for f in pkg-x86_64-linux.sigstore.json pkg-installer-x86_64-linux.sigstore.json; do
        gh release download "$release" --repo "$repo" -p "$f" -O "$x/$f" --clobber
    done
    cp -R "$work/determinate/." "$x/determinate/"

    echo "==> [$side_name] preparing the sealed manifest"
    RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release \
        -p pkg-release --example linux_proof_publication -- \
        --prepare-dn16-manifest "$dir/prepared" "$work/runtimes" "$a" "$x" \
        "$signing_state" "$seq" "$release"

    echo "==> [$side_name] publishing the channel tree"
    RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release \
        -p pkg-release --example linux_proof_publication -- \
        --publish-dn16 "$dir/channel" "$work/runtimes" "$a" "$x" \
        "$dir/prepared/release-manifest.json" "$signing_state" "$seq" "$release"

    echo "==> [$side_name] attaching proof-inputs"
    mkdir -p "$dir/channel/proof-inputs"
    for f in "pkg-${version}-preview.pkg" "pkg-${version}-preview.pkg.sigstore.json" \
             pkg-aarch64-darwin pkg-aarch64-darwin.sigstore.json \
             SHA256SUMS SHA256SUMS.sigstore.json COSIGN_IDENTITY.txt COSIGN_ISSUER.txt \
             release-manifest.json 1.root.json; do
        gh release download "$release" --repo "$repo" -p "$f" \
            -O "$dir/channel/proof-inputs/$f" --clobber
    done
}

side "$n_release"  n         "$n_seq"
side "$n1_release" n-plus-1  "$n1_seq"

# ---- seal the pair
pair_dir="$work/pair"
mkdir -p "$pair_dir"
mv "$work/n/channel"          "$pair_dir/n"
mv "$work/n-plus-1/channel"   "$pair_dir/n-plus-1"

product_commit=$(git rev-parse HEAD)
echo "==> binding pair with product commit $product_commit"
RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release \
    -p pkg-release --example linux_proof_publication -- \
    --bind-dn16-pair "$pair_dir" "$n_release" "$n1_release" "$product_commit"

ls -la "$pair_dir"
echo "==> pair sealed at $pair_dir"
echo "==> next: upload to tag $pair_tag and fill the workflow pins"
echo "PAIR_DIR=$pair_dir"
