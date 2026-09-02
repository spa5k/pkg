#!/usr/bin/env bash
# DN-1 PR-6: mint the loopback proof pair (alpha.N / alpha.N+1).
#
# Builds two releases with the in-VM loopback channel URL baked at compile
# time, packages the macOS preview installers, creates draft releases, signs
# them through the pinned publish-release workflow, assembles the pair
# channel trees with the DN-16 test TUF signing state, seals the pair, and
# uploads the bundle to the dedicated proof-pair tag.
#
# Grounding: PR6-GROUNDING.md. Every command mirrors the DN-16 mint.
#
# Usage:
#   tools/release/mint_dn1_proof_pair.sh \
#       N_SEQUENCE N_PLUS_1_SEQUENCE PAIR_TAG PRODUCT_COMMIT SIGNING_STATE_DIR
#
# Example (alpha.26/.27, first DN-1 pair):
#   tools/release/mint_dn1_proof_pair.sh 26 27 dn1-proof-pair-1 \
#       cbd3494443b94283430d8a48e9fec65699d0210a \
#       /private/tmp/pkg-dn16-proof-build.CoSikP/signing-state

set -euo pipefail

usage() {
    echo "usage: mint_dn1_proof_pair.sh N_SEQ N_PLUS_1_SEQ PAIR_TAG PRODUCT_COMMIT SIGNING_STATE_DIR" >&2
    exit 64
}

[ "$#" -eq 5 ] || usage
n_seq=$1
n1_seq=$2
pair_tag=$3
product_commit=$4
signing_state=$5

[[ "$n_seq" =~ ^[1-9][0-9]*$ ]] || usage
[[ "$n1_seq" =~ ^[1-9][0-9]*$ ]] || usage
[ "$n_seq" -lt "$n1_seq" ] || usage
[[ "$pair_tag" =~ ^[a-z0-9][a-z0-9-]*$ ]] || usage
[[ "$product_commit" =~ ^[0-9a-f]{40}$ ]] || usage
[ -f "$signing_state/1.root.json" ] || {
    echo "signing state is missing 1.root.json: $signing_state" >&2
    exit 1
}

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

repo="spa5k/pkg"
n_release="v0.1.0-alpha.${n_seq}"
n1_release="v0.1.0-alpha.${n1_seq}"
base_url="https://127.0.0.1:8443"
work="$(mktemp -d /tmp/pkg-dn1-mint.XXXXXXXX)"
echo "==> mint work dir: $work"

build_release() {
    local release=$1
    local side=$2 # n | n-plus-1
    local version=${release#v}
    local out="$work/$release"
    install -d -m 0700 "$out"

    echo "==> building $release with ${side}/ URLs baked"
    PKG_RELEASE_TUF_ROOT_JSON="$(cat "$signing_state/1.root.json")" \
    PKG_RELEASE_CHANNEL_METADATA_URL="$base_url/$side/metadata/" \
    PKG_RELEASE_CHANNEL_TARGETS_URL="$base_url/$side/targets/" \
    RUSTUP_TOOLCHAIN=1.96.1 cargo build --locked --release \
        -p pkg-cli --bin pkg \
        -p pkg-installer --bin pkg-nix-broker --bin pkg-root-helper --bin pkg-install \
        -p pkg-release --bin pkg-release-index

    echo "==> strings gate: the baked URLs are present"
    local installer=target/release/pkg-install
    /usr/bin/strings "$installer" | grep -F "$base_url/$side/metadata/" >/dev/null
    /usr/bin/strings "$installer" | grep -F "$base_url/$side/targets/" >/dev/null

    echo "==> preview package"
    packaging/macos/build-preview.sh \
        "$PWD/target/release/pkg-install" \
        "$out/pkg-${version}-preview.pkg" \
        "$version"

    echo "==> staging $release assets"
    install -m 0755 target/release/pkg "$out/pkg-aarch64-darwin"
    install -m 0755 target/release/pkg "$out/pkg-x86_64-linux"
    install -m 0755 target/release/pkg-install "$out/pkg-installer-x86_64-linux"
    install -m 0644 "$signing_state/1.root.json" "$out/1.root.json"
    install -m 0755 install.sh "$out/install.sh"
    tar -czf "$out/pkg-${release}-macos-aarch64.tar.gz" \
        -C target/release pkg pkg-nix-broker pkg-root-helper
    tar -czf "$out/pkg-${release}-linux-x86_64.tar.gz" \
        -C target/release pkg pkg-nix-broker pkg-root-helper pkg-install
    (
        cd "$out"
        shasum -a 256 pkg-aarch64-darwin pkg-x86_64-linux \
            pkg-installer-x86_64-linux "pkg-${version}-preview.pkg" \
            "pkg-${release}-macos-aarch64.tar.gz" \
            "pkg-${release}-linux-x86_64.tar.gz" install.sh >SHA256SUMS
    )
}

draft_release() {
    local release=$1
    local out="$work/$release"
    local version=${release#v}
    echo "==> draft release $release"
    gh release create "$release" --repo "$repo" --draft --verify-tag=false \
        --title "pkg $release" --notes "DN-1 proof pair mint $(date -u +%FT%TZ)" \
        "$out/1.root.json" \
        "$out/install.sh" \
        "$out/pkg-${version}-preview.pkg" \
        "$out/pkg-aarch64-darwin" \
        "$out/pkg-installer-x86_64-linux" \
        "$out/pkg-${release}-linux-x86_64.tar.gz" \
        "$out/pkg-${release}-macos-aarch64.tar.gz" \
        "$out/pkg-x86_64-linux" \
        "$out/SHA256SUMS"
}

echo "==> minting $n_release (N) then $n1_release (N+1)"
build_release "$n_release" n
build_release "$n1_release" n-plus-1

draft_release "$n_release"
draft_release "$n1_release"

cat <<NOTICE
==> drafts created. Next steps are manual-by-design (each is a reviewed gate):

1. Sign both drafts by dispatching publish-release.yml from the
   dn16-proof-workflow-1 tag checkout:
     git -C <tmp> checkout dn16-proof-workflow-1
     gh workflow run publish-release.yml --repo $repo \\
         --ref dn16-proof-workflow-1 \\
         -f tag=$n_release -f expected_sha=<tag commit sha>
     (repeat for $n1_release)

2. Assemble the pair channel trees and seal:
     cargo run --locked --release -p pkg-release --bin pkg-release-index -- \\
         --publish-dn16 <out.n> <runtimes> <aarch64-input> <x86_64-input> \\
         <sealed-manifest> $signing_state <seq> <release-id>   # per side
     cargo run --locked --release -p pkg-release --bin pkg-release-index -- \\
         --bind-dn16-pair <pair-dir> $n_release $n1_release $product_commit

3. Upload the pair bundle to the $pair_tag tag as release assets.

The signing-state root is $(shasum -a 256 "$signing_state/1.root.json" | awk '{print $1}')
NOTICE
echo "==> mint complete (drafts staged): $work"
