#!/bin/sh
# Sign the checksum list of a draft release and attach the signature.
#
# Usage: tools/release/sign.sh TAG [PUBLIC_KEY]
#
# PUBLIC_KEY defaults to ~/.ssh/id_ed25519.pub. Its private key must be in
# ssh-agent or beside it. The key must be in RELEASE_KEYS in
# crates/pkg-cli/src/client.rs, because `pkg update` accepts only those keys.
set -eu

tag=${1:?usage: tools/release/sign.sh TAG [PUBLIC_KEY]}
key=${2:-$HOME/.ssh/id_ed25519.pub}
repo=spa5k/pkg
root=$(git rev-parse --show-toplevel)

# The quoted base64 lines between `const RELEASE_KEYS` and `];`.
expected=$(awk '/^const RELEASE_KEYS/ { on = 1; next } on && /^];/ { exit } on' \
  "$root/crates/pkg-cli/src/client.rs" | sed -n 's/^ *"\([A-Za-z0-9+\/=]*\)",$/\1/p')
if [ -z "$expected" ] || ! printf '%s\n' "$expected" | grep -qxF "$(awk '{print $2}' "$key")"; then
  echo "sign.sh: $key is not in RELEASE_KEYS in crates/pkg-cli/src/client.rs." >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh release download "$tag" --repo "$repo" --dir "$work" \
  --pattern SHA256SUMS --pattern 'pkg-*.tar.gz'
# Sign only a list that matches the archives in the draft.
(cd "$work" && shasum -a 256 -c SHA256SUMS)

ssh-keygen -Y sign -f "$key" -n pkg-release "$work/SHA256SUMS"
printf 'pkg-release %s\n' "$(awk '{print $1, $2}' "$key")" > "$work/allowed"
ssh-keygen -Y verify -f "$work/allowed" -I pkg-release -n pkg-release \
  -s "$work/SHA256SUMS.sig" < "$work/SHA256SUMS"
gh release upload "$tag" --repo "$repo" --clobber "$work/SHA256SUMS.sig"
echo "sign.sh: attached SHA256SUMS.sig to $tag. Publish the draft as the latest release."
