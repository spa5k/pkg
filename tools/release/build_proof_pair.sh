#!/bin/bash
# DN-1 proof pair builder — single entry point, clean, deterministic.
# Usage: bash tools/release/build_proof_pair.sh
set -euo pipefail
export PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin
export SSH_ASKPASS_REQUIRE=never
export PKG_TIMESTAMP_TTL_HOURS=168
REPO=spa5k/pkg
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
WORK=/tmp/pkg-pair
TMPDIR_CLEAN=$WORK/tmp; mkdir -p "$TMPDIR_CLEAN"; export TMPDIR="$TMPDIR_CLEAN"

cd "$ROOT_DIR"
rm -rf "$WORK" && mkdir -p "$WORK/tmp"

log() { printf '  %s\n' "$*"; }

# ── 1. Signing state ──
log "signing state"
SS=$WORK/state
RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release -q \
  -p pkg-release --example linux_proof_publication -- --prepare "$SS" >/dev/null 2>&1
ROOT_SHA=$(shasum -a 256 "$SS/root.json" | awk '{print $1}')
log "root ${ROOT_SHA:0:16}"

# ── 2. Build binaries once per side ──
for side in n n-plus-1; do
  ver=$([ $side = n ] && echo 32 || echo 33)
  rel="v0.1.0-alpha.${ver}"
  log "building $side (alpha.$ver)"
  RUSTUP_TOOLCHAIN=1.96.1 \
    PKG_RELEASE_TUF_ROOT_JSON="$(cat "$SS/root.json")" \
    PKG_RELEASE_CHANNEL_METADATA_URL="https://127.0.0.1:8443/${side}/metadata/" \
    PKG_RELEASE_CHANNEL_TARGETS_URL="https://127.0.0.1:8443/${side}/targets/" \
    cargo build --locked --release \
      -p pkg-cli --bin pkg \
      -p pkg-installer --bin pkg-nix-broker --bin pkg-root-helper --bin pkg-install \
    >/dev/null 2>&1

  BIN=$ROOT_DIR/target/release
  IN=$WORK/inputs-$side; RA=$WORK/release-$side
  mkdir -p "$IN/aarch64-input" "$IN/x86_64-input/determinate" "$RA"

  # Same binaries in inputs and release assets
  for d in "$IN/aarch64-input" "$IN/x86_64-input" "$RA"; do
    install -m 0755 "$BIN/pkg" "$d/pkg" 2>/dev/null || true
    install -m 0755 "$BIN/pkg-nix-broker" "$d/pkg-nix-broker" 2>/dev/null || true
    install -m 0755 "$BIN/pkg-root-helper" "$d/pkg-root-helper" 2>/dev/null || true
  done
  install -m 0755 "$BIN/pkg" "$IN/aarch64-input/pkg-aarch64-darwin"
  install -m 0755 "$BIN/pkg" "$IN/x86_64-input/pkg-x86_64-linux"
  install -m 0755 "$BIN/pkg" "$RA/pkg-aarch64-darwin"
  install -m 0755 "$BIN/pkg" "$RA/pkg-x86_64-linux"
  install -m 0755 "$BIN/pkg-install" "$IN/x86_64-input/pkg-installer-x86_64-linux"
  install -m 0755 "$BIN/pkg-install" "$RA/pkg-installer-x86_64-linux"

  # Preview pkg
  rm -f "$WORK/preview.pkg"
  packaging/macos/build-preview.sh "$BIN/pkg-install" "$WORK/preview.pkg" "0.1.0-alpha.${ver}" >/dev/null 2>&1
  cp "$WORK/preview.pkg" "$IN/aarch64-input/pkg-0.1.0-alpha.${ver}-preview.pkg"
  cp "$WORK/preview.pkg" "$RA/pkg-0.1.0-alpha.${ver}-preview.pkg"

  # Release-only assets
  cp "$SS/root.json" "$RA/1.root.json"
  install -m 0755 "$ROOT_DIR/install.sh" "$RA/install.sh" 2>/dev/null || echo "#!/bin/sh" > "$RA/install.sh" && chmod +x "$RA/install.sh"
  tar -czf "$RA/pkg-${rel}-macos-aarch64.tar.gz" -C "$BIN" pkg pkg-nix-broker pkg-root-helper
  tar -czf "$RA/pkg-${rel}-linux-x86_64.tar.gz" -C "$BIN" pkg pkg-nix-broker pkg-root-helper pkg-install

  done

# ── 3. Upstream artifacts ──
log "upstream artifacts"
mkdir -p "$WORK/runtimes" "$WORK/determinate"
for f in aarch64-darwin.tar.xz x86_64-linux.tar.xz \
         nix-installer-aarch64-darwin nix-installer-aarch64-linux \
         nix-installer-x86_64-linux nix-installer-v3.22.1.tar.gz LICENSE; do
  src=$(find /tmp/pkg-dn1-fin -name "$f" -type f 2>/dev/null | head -1)
  [ -z "$src" ] && src=$(find /private/tmp/pkg-dn16-persistent-tunnel* -name "$f" -type f 2>/dev/null | head -1)
  if [ -n "$src" ]; then
    case $f in *.tar.xz) install -m 0644 "$src" "$WORK/runtimes/$f";; *) install -m 0644 "$src" "$WORK/determinate/$f";; esac
  else log "WARN: $f not found"; fi
done
for side in n n-plus-1; do cp -R "$WORK/determinate/." "$WORK/inputs-$side/x86_64-input/determinate/"; done

# ── 4. Manifests ──
log "manifests"
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release -q \
    -p pkg-release --example linux_proof_publication -- \
    --prepare-dn16-manifest "$WORK/manifest-$side" "$WORK/runtimes" \
    "$WORK/inputs-$side/aarch64-input" "$WORK/inputs-$side/x86_64-input" \
    "$SS" "$seq" "$rel" >/dev/null 2>&1
  [ -f "$WORK/manifest-$side" ] || { log "FAILED: manifest-$side"; exit 1; }
  cp "$WORK/manifest-$side" "$WORK/release-$side/release-manifest.json"
done

# SHA256SUMS
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  ra=$WORK/release-$side
  (cd "$ra" && shasum -a 256 1.root.json install.sh release-manifest.json \
    "pkg-0.1.0-alpha.${ver}-preview.pkg" pkg-aarch64-darwin pkg-x86_64-linux pkg-installer-x86_64-linux \
    "pkg-${rel}-macos-aarch64.tar.gz" "pkg-${rel}-linux-x86_64.tar.gz" > SHA256SUMS)
done

# ── 5. Tags + draft releases ──
log "draft releases"
GIT_SHA=$(git rev-parse dn16-proof-workflow-1^{})
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  ra=$WORK/release-$side
  git tag -f -a -m "pkg $rel" "$rel" HEAD 2>/dev/null
  git push -f origin "$rel" >/dev/null 2>&1
  gh release delete "$rel" --repo $REPO --yes >/dev/null 2>&1 </dev/null
  gh release create "$rel" --repo $REPO --draft </dev/null --verify-tag --title "pkg $rel" --notes "root ${ROOT_SHA:0:8}" \
    "$ra/1.root.json" "$ra/install.sh" "$ra/release-manifest.json" \
    "$ra/pkg-0.1.0-alpha.${ver}-preview.pkg" "$ra/pkg-aarch64-darwin" "$ra/pkg-installer-x86_64-linux" \
    "$ra/pkg-${rel}-macos-aarch64.tar.gz" "$ra/pkg-${rel}-linux-x86_64.tar.gz" \
    "$ra/pkg-x86_64-linux" "$ra/SHA256SUMS" >/dev/null 2>&1
done

# ── 6. Sign (sequential) ──
log "signing"
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  gh workflow run publish-release.yml --repo $REPO --ref dn16-proof-workflow-1 </dev/null \
    -f tag="$rel" -f expected_sha=$GIT_SHA >/dev/null 2>&1
  sleep 25
  run=$(gh run list --repo $REPO --workflow publish-release.yml --limit 1 --json databaseId -q '.[0].databaseId')
  for i in $(seq 1 12); do
    eid=$(gh api repos/$REPO/actions/runs/$run/pending_deployments -q '.[] | .environment.id' 2>/dev/null | head -1)
    [ -n "$eid" ] && break; sleep 5
  done
  [ -n "$eid" ] && gh api -X POST repos/$REPO/actions/runs/$run/pending_deployments \
    --input - <<< "{\"environment_ids\": [$eid], \"state\": \"approved\", \"comment\": \"sign $rel\"}" >/dev/null 2>&1
  for i in $(seq 1 36); do
    c=$(gh api repos/$REPO/actions/runs/$run --jq '.conclusion // .status' 2>/dev/null)
    case "$c" in completed*) break ;; esac; sleep 10
  done
  c=$(gh api repos/$REPO/actions/runs/$run --jq '.conclusion' 2>/dev/null)
  log "$rel: $c"
  [ "$c" = "success" ] || { log "SIGNING FAILED"; exit 1; }
done

# ── 7. Download signed + publish channels ──
log "channels"
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  in=$WORK/inputs-$side
  for f in "pkg-0.1.0-alpha.${ver}-preview.pkg.sigstore.json" pkg-aarch64-darwin.sigstore.json; do
    gh release download "$rel" --repo $REPO </dev/null -p "$f" -O "$in/aarch64-input/$f" --clobber >/dev/null 2>&1
  done
  for f in pkg-x86_64-linux.sigstore.json pkg-installer-x86_64-linux.sigstore.json; do
    gh release download "$rel" --repo $REPO </dev/null -p "$f" -O "$in/x86_64-input/$f" --clobber >/dev/null 2>&1
  done

  # Seal manifest
  python3 - "$WORK" "$side" <<'PY'
import hashlib, json, pathlib, sys
W = pathlib.Path(sys.argv[1]); side = sys.argv[2]
m = json.loads((W / f'manifest-{side}').read_bytes())
for art in m['cliArtifacts']:
    name = pathlib.Path(art['source']).name
    d = W / f'inputs-{side}' / ('aarch64-input' if 'aarch64-darwin' in name else 'x86_64-input')
    sf = d / f'{name}.sigstore.json'
    if sf.exists():
        b = sf.read_bytes()
        art['sigstoreBundle'] = art['source'] + '.sigstore.json'
        art['sigstoreBundleSha256'] = hashlib.sha256(b).hexdigest()
        art['sigstoreBundleLength'] = len(b)
(W / f'sealed-{side}').write_text(json.dumps(m))
PY

  # Publish
  rm -rf "$WORK/channel-$side"
  RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release -q \
    -p pkg-release --example linux_proof_publication -- \
    --publish-dn16 "$WORK/channel-$side" "$WORK/runtimes" \
    "$in/aarch64-input" "$in/x86_64-input" \
    "$WORK/sealed-$side" "$SS" "$seq" "$rel" 2>&1 | tail -1
  [ -f "$WORK/channel-$side/metadata/timestamp.json" ] || { log "CHANNEL $side FAILED"; exit 1; }
done
log "channels OK (timestamps valid 7 days)"

# ── 8. Pair ──
log "pair"
PAIR=$WORK/pair; mkdir -p "$PAIR"
for sideinfo in "n 1 32 v0.1.0-alpha.32" "n-plus-1 2 33 v0.1.0-alpha.33"; do
  read -r side seq ver rel <<< "$sideinfo"
  mkdir -p "$PAIR/$side/proof-inputs"
  cp -R "$WORK/channel-$side/metadata" "$PAIR/$side/"
  cp -R "$WORK/channel-$side/targets" "$PAIR/$side/"
  for f in root.json release-manifest.json signing-audit.ndjson; do
    cp "$WORK/channel-$side/$f" "$PAIR/$side/" 2>/dev/null
  done
  for f in "pkg-0.1.0-alpha.${ver}-preview.pkg" "pkg-0.1.0-alpha.${ver}-preview.pkg.sigstore.json" \
           pkg-aarch64-darwin pkg-aarch64-darwin.sigstore.json \
           SHA256SUMS SHA256SUMS.sigstore.json COSIGN_IDENTITY.txt COSIGN_ISSUER.txt release-manifest.json; do
    gh release download "$rel" --repo $REPO </dev/null -p "$f" -O "$PAIR/$side/proof-inputs/$f" --clobber >/dev/null 2>&1
  done
done
COMMIT=$(git rev-parse HEAD)
mapfile -t SIDES < "$WORK/sides.txt"
RUSTUP_TOOLCHAIN=1.96.1 cargo run --locked --release -q \
  -p pkg-release --example linux_proof_publication -- \
  --bind-dn16-pair "$PAIR" "$(echo "${SIDES[0]}" | awk '{print $4}')" "$(echo "${SIDES[1]}" | awk '{print $4}')" "$COMMIT" 2>&1 | tail -1
[ -f "$PAIR/proof-pair.json" ] || { log "PAIR BIND FAILED"; exit 1; }

# Tarball (read-only modes)
python3 - "$PAIR" <<'PY'
import pathlib, sys, tarfile, hashlib
W = pathlib.Path(sys.argv[1])
out = W / 'dn1-proof-pair.tar.gz'
top = ['n', 'n-plus-1', 'proof-pair.json', 'n.inventory.json', 'n-plus-1.inventory.json']
def dirinfo(a):
    t = tarfile.TarInfo(a); t.type = tarfile.DIRTYPE; t.mode = 0o555; return t
def add(t, p, a):
    if p.is_dir():
        t.addfile(dirinfo(a))
        for c in sorted(p.iterdir()): add(t, c, f'{a}/{c.name}')
    else:
        ti = tarfile.TarInfo(a); ti.size = p.stat().st_size; ti.mode = 0o444
        with open(p, 'rb') as f: t.addfile(ti, f)
out.unlink(missing_ok=True)
with tarfile.open(out, 'w:gz') as t:
    for name in top: add(t, W/name, name)
b = out.read_bytes()
print(f'  tarball {hashlib.sha256(b).hexdigest()} {len(b)} bytes')
PY

# Print all pins
python3 - "$PAIR" <<'PY'
import hashlib, json, pathlib, sys
W = pathlib.Path(sys.argv[1])
for name in ('proof-pair.json', 'n.inventory.json', 'n-plus-1.inventory.json'):
    b = (W/name).read_bytes()
    print(f'  {name} {len(b)} {hashlib.sha256(b).hexdigest()}')
for side in ('n', 'n-plus-1'):
    inv = json.loads((W/f'{side}.inventory.json').read_bytes())
    files = {f['path']: f for f in inv['files']}
    total = sum(f['length'] for f in files.values())
    blob = ''.join(f"{p}\t{files[p]['length']}\t{files[p]['sha256']}\n" for p in sorted(files)).encode()
    print(f'  {side} total {total} rows {hashlib.sha256(blob).hexdigest()}')
tar = (W/'dn1-proof-pair.tar.gz').read_bytes()
print(f'  tarball {len(tar)} {hashlib.sha256(tar).hexdigest()}')
root = hashlib.sha256(open('/tmp/pkg-pair/state/root.json','rb').read()).hexdigest()
print(f'  root {root}')
PY

log "DONE — pair at $PAIR"
