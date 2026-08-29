#!/bin/sh
set -eu

fail() {
    echo "macOS lifecycle proof failed: $1" >&2
    exit 1
}

require_env() {
    eval "value=\${$1:-}"
    [ -n "$value" ] || fail "$1 is absent"
}

for name in PKG_PROOF_FROM_RELEASE PKG_PROOF_TO_RELEASE PKG_PROOF_ROOT \
    PKG_PROOF_CHANNEL_URL PKG_PROOF_PAIR_SHA256 PKG_PROOF_REBOOT_MARKER \
    PKG_PROOF_LIFECYCLE_RUN PKG_PROOF_PHASE GITHUB_RUN_ID GITHUB_RUN_ATTEMPT \
    GITHUB_WORKFLOW_SHA RUNNER_NAME; do
    require_env "$name"
done

[ "${GITHUB_ACTIONS:-}" = true ] || fail "GitHub Actions did not identify this runner"
[ "${RUNNER_ENVIRONMENT:-}" = self-hosted ] || fail "the runner is not self-hosted"
[ "$(/usr/bin/uname -s)" = Darwin ] || fail "the host is not macOS"
[ "$(/usr/bin/uname -m)" = arm64 ] || fail "the host is not Apple Silicon"
[ "$(/usr/sbin/sysctl -n kern.hv_vmm_present)" = 1 ] || fail "the host is not virtualized"
case "$(/usr/sbin/sysctl -n hw.model)" in VirtualMac*) ;; *) fail "the host is not a VirtualMac" ;; esac
case "$PKG_PROOF_LIFECYCLE_RUN" in 1|2) ;; *) fail "the lifecycle run is invalid" ;; esac
case "$PKG_PROOF_PHASE" in prepare|resume) ;; *) fail "the proof phase is invalid" ;; esac
for tag in "$PKG_PROOF_FROM_RELEASE" "$PKG_PROOF_TO_RELEASE"; do
    printf '%s\n' "$tag" | /usr/bin/grep -Eq '^v0\.1\.0-alpha\.[1-9][0-9]*$' \
        || fail "a release tag is invalid"
done
[ "$PKG_PROOF_FROM_RELEASE" != "$PKG_PROOF_TO_RELEASE" ] || fail "the release tags are equal"

root=$PKG_PROOF_ROOT
case "$root" in "${RUNNER_TEMP:-/no-runner-temp}"/*) ;; *) fail "the proof root is unsafe" ;; esac
evidence="$root/evidence"
preflight="$evidence/preflight.txt"
/usr/bin/sudo -n /usr/bin/true || fail "passwordless administrative authority is unavailable"
instance_marker=/var/tmp/pkg-disposable-macos-instance
/usr/bin/sudo -n /bin/test -f "$instance_marker" \
    && /usr/bin/sudo -n /bin/test ! -L "$instance_marker" \
    && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%z' "$instance_marker")" -le 128 ] \
    && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' "$instance_marker")" = \
        root:wheel:600 ] \
    || fail "the instance marker is unsafe"
instance_record=$(/usr/bin/sudo -n /bin/cat "$instance_marker")
printf '%s\n' "$instance_record" \
    | /usr/bin/grep -Eq '^PKG-DN16-INSTANCE-V1:[0-9a-f]{64}$' \
    || fail "the instance marker is invalid"
instance_nonce=${instance_record#PKG-DN16-INSTANCE-V1:}
if [ "$PKG_PROOF_PHASE" = prepare ]; then
    instance_age=$(( $(/bin/date +%s) - \
        $(/usr/bin/sudo -n /usr/bin/stat -f '%m' "$instance_marker") ))
    [ "$instance_age" -ge 0 ] && [ "$instance_age" -le 300 ] \
        || fail "the instance marker is stale"
fi
[ -f "$preflight" ] && [ ! -L "$preflight" ] \
    && [ "$(/usr/bin/stat -f '%z' "$preflight")" -le 512 ] \
    && [ "$(/usr/bin/wc -l <"$preflight" | /usr/bin/tr -d ' ')" -eq 5 ] \
    || fail "the bounded preflight evidence is invalid"
preflight_slot=$(/usr/bin/sed -n 's/^lifecycle_run=//p' "$preflight")
preflight_runner=$(/usr/bin/sed -n 's/^runner_name=//p' "$preflight")
preflight_nonce=$(/usr/bin/sed -n 's/^instance_nonce=//p' "$preflight")
[ "$(/bin/cat "$preflight")" = "$(printf '%s\n' \
    "lifecycle_run=$PKG_PROOF_LIFECYCLE_RUN" \
    "phase=$PKG_PROOF_PHASE" \
    "runner_name=$RUNNER_NAME" \
    "instance_nonce=$preflight_nonce" \
    "status=preflight-passed")" ] || fail "the preflight evidence does not bind this job"
[ "$preflight_slot" = "$PKG_PROOF_LIFECYCLE_RUN" ] \
    && [ "$preflight_runner" = "$RUNNER_NAME" ] \
    && [ "$preflight_nonce" = "$instance_nonce" ] \
    || fail "the preflight identity is invalid"

disposable=/var/tmp/pkg-disposable-macos-proof
/usr/bin/sudo -n /bin/test -f "$disposable" \
    && /usr/bin/sudo -n /bin/test ! -L "$disposable" \
    && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%z' "$disposable")" -le 128 ] \
    || fail "the disposable marker is unsafe"
[ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' "$disposable")" = root:wheel:600 ] \
    || fail "the disposable marker is unsafe"
[ "$(/usr/bin/sudo -n /bin/cat "$disposable")" = \
    "PKG-DN16-DISPOSABLE-V1:${GITHUB_RUN_ID}:$PKG_PROOF_LIFECYCLE_RUN" ] \
    || fail "the disposable marker does not bind this run"

# The provisioner binds the initial fresh-VM reboot to the prepare job.
if [ "$PKG_PROOF_PHASE" = prepare ]; then
/usr/bin/sudo -n /bin/test -f "$PKG_PROOF_REBOOT_MARKER" \
    && /usr/bin/sudo -n /bin/test ! -L "$PKG_PROOF_REBOOT_MARKER" \
    && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%z' "$PKG_PROOF_REBOOT_MARKER")" -le 256 ] \
    && [ "$(/usr/bin/sudo -n /bin/cat "$PKG_PROOF_REBOOT_MARKER" \
        | /usr/bin/wc -l | /usr/bin/tr -d ' ')" -eq 1 ] \
    && [ "$(/usr/bin/sudo -n /usr/bin/tail -c 1 "$PKG_PROOF_REBOOT_MARKER" \
        | /usr/bin/od -An -tuC \
        | /usr/bin/tr -d ' ')" = 10 ] \
    || fail "the fresh-runner reboot marker is unsafe or absent"
[ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' \
    "$PKG_PROOF_REBOOT_MARKER")" = root:wheel:600 ] \
    || fail "the fresh-runner reboot marker is unsafe or absent"
IFS=: read -r reboot_kind reboot_run reboot_slot reboot_runner reboot_nonce old_boot \
    reboot_time reboot_extra <<EOF
$(/usr/bin/sudo -n /bin/cat "$PKG_PROOF_REBOOT_MARKER")
EOF
[ "$reboot_kind" = PKG-DN16-REBOOT-V2 ] \
    && [ "$reboot_run" = "$GITHUB_RUN_ID" ] \
    && [ "$reboot_slot" = "$PKG_PROOF_LIFECYCLE_RUN" ] \
    && [ "$reboot_runner" = "$RUNNER_NAME" ] \
    && [ "$reboot_nonce" = "$instance_nonce" ] \
    && [ -z "$reboot_extra" ] \
    || fail "the fresh-runner reboot marker does not bind this job and VM"
printf '%s\n' "$reboot_nonce" | /usr/bin/grep -Eq '^[0-9a-f]{64}$' \
    || fail "the reboot instance nonce is invalid"
printf '%s\n' "$old_boot" | /usr/bin/grep -Eq \
    '^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$' \
    || fail "the saved boot session is invalid"
case "$reboot_time" in ''|*[!0-9]*) fail "the reboot timestamp is invalid" ;; esac
current_boot=$(/usr/sbin/sysctl -n kern.bootsessionuuid)
[ "$old_boot" != "$current_boot" ] || fail "the external runner did not reboot"
reboot_age=$(( $(/bin/date +%s) - reboot_time ))
marker_age=$(( $(/bin/date +%s) - \
    $(/usr/bin/sudo -n /usr/bin/stat -f '%m' "$PKG_PROOF_REBOOT_MARKER") ))
[ "$reboot_age" -ge 0 ] && [ "$reboot_age" -le 300 ] \
    && [ "$marker_age" -ge 0 ] && [ "$marker_age" -le 300 ] \
    || fail "the fresh-runner reboot marker is stale"

# Recheck the full clean-host boundary before the first product mutation.
launchd_labels="$root/preflight-launchd-labels.txt"
(ulimit -f 2048; /usr/bin/sudo -n /bin/launchctl list) >"$launchd_labels"
[ "$(/usr/bin/stat -f '%z' "$launchd_labels")" -le 1048576 ] \
    || fail "the launchd inventory is too large"
/usr/bin/awk 'NR > 1 {
    label=tolower($3)
    if (label ~ /nix.*daemon/ || label ~ /daemon.*nix/) found=1
} END { exit found ? 1 : 0 }' "$launchd_labels" \
    || fail "a Nix daemon is already loaded"
for path in /nix /etc/nix /opt/pkg /private/var/db/pkg-install \
    /private/var/db/pkg-install-journal /private/var/db/pkg-install-auth \
    /Library/LaunchDaemons/org.pkg.root-helper.plist \
    /Library/LaunchDaemons/org.pkg.nix-broker.plist \
    /Library/LaunchDaemons/org.nixos.nix-daemon.plist \
    /Library/LaunchDaemons/systems.determinate.nix-daemon.plist; do
    [ ! -e "$path" ] && [ ! -L "$path" ] || fail "$path already exists"
done
users="$root/preflight-users.txt"
groups="$root/preflight-groups.txt"
(ulimit -f 128; /usr/bin/dscl . -list /Users) >"$users"
(ulimit -f 128; /usr/bin/dscl . -list /Groups) >"$groups"
[ "$(/usr/bin/stat -f '%z' "$users")" -le 65536 ] \
    && [ "$(/usr/bin/stat -f '%z' "$groups")" -le 65536 ] \
    || fail "an account inventory is too large"
! /usr/bin/grep -Eq '^(_?nixbld([0-9]+)?|_?pkg-nix-broker)$' "$users" \
    || fail "a product or Nix user already exists"
! /usr/bin/grep -Eq '^(_?nixbld|_?pkg-nix-broker)$' "$groups" \
    || fail "a product or Nix group already exists"
for file in /private/etc/synthetic.conf /private/etc/fstab; do
    [ ! -e "$file" ] || [ -f "$file" ] || fail "$file is not a regular file"
    [ ! -L "$file" ] || fail "$file is a symlink"
    [ ! -e "$file" ] || [ -r "$file" ] || fail "$file is unreadable"
done
if [ -f /private/etc/synthetic.conf ]; then
    /usr/bin/awk '!/^[[:space:]]*#/ && $1 == "nix" { found=1 } \
        END { exit found ? 1 : 0 }' /private/etc/synthetic.conf \
        || fail "synthetic.conf already defines /nix"
fi
if [ -f /private/etc/fstab ]; then
    /usr/bin/awk '!/^[[:space:]]*#/ { \
        for (i=1; i<=NF; i++) if ($i == "/nix") found=1 \
    } END { exit found ? 1 : 0 }' /private/etc/fstab \
        || fail "fstab already defines /nix"
fi
for directory in /Library/LaunchDaemons /Library/LaunchAgents; do
    /usr/bin/find "$directory" -maxdepth 1 -type f -print | while IFS= read -r path; do
        name=$(/usr/bin/basename "$path" | /usr/bin/tr '[:upper:]' '[:lower:]')
        case "$name" in
            *org.nixos.*|*determinate*nix*|*nix*daemon*|nix-*|nix.*|_nixbld*)
                fail "a Nix launchd file already exists"
                ;;
        esac
    done
done
for home in /private/var/root /Users/*; do
    for name in .nix-profile .nix-defexpr .nix-channels; do
        [ ! -e "$home/$name" ] && [ ! -L "$home/$name" ] \
            || fail "a Nix user profile already exists"
    done
done
for directory in /bin /usr/bin /usr/local/bin /opt/homebrew/bin; do
    for name in nix nix-daemon nix-store nix-env nix-build; do
        [ ! -e "$directory/$name" ] && [ ! -L "$directory/$name" ] \
            || fail "a Nix command already exists"
    done
done
for name in nix nix-daemon nix-store nix-env nix-build; do
    ! command -v "$name" >/dev/null 2>&1 || fail "a Nix command is on PATH"
done
require_unloaded() {
    set +e
    /bin/launchctl print "system/$1" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" -eq 113 ] || fail "$1 is loaded or its state is uncertain"
}
for label in org.nixos.nix-daemon systems.determinate.nix-daemon \
    org.nixos.darwin-store systems.determinate.nix-store org.pkg.nix-daemon \
    org.pkg.root-helper org.pkg.nix-broker; do
    require_unloaded "$label"
done
fi

harness=$(CDPATH= cd -- "$(/usr/bin/dirname "$0")" && /bin/pwd -P)
from="$root/candidate/from"
to="$root/candidate/to"
channel="$root/channel"
work="$root/work"
/bin/mkdir -p -m 0700 "$evidence" "$work"

from_version=${PKG_PROOF_FROM_RELEASE#v}
to_version=${PKG_PROOF_TO_RELEASE#v}
from_pkg="$from/pkg-$from_version-preview.pkg"
to_pkg="$to/pkg-$to_version-preview.pkg"
for path in "$from_pkg" "$to_pkg" "$from/pkg-aarch64-darwin" \
    "$to/pkg-aarch64-darwin" "$harness/pkg-installer-tests" \
    "$channel/proof-pair.json" "$channel/n.inventory.json" \
    "$channel/n-plus-1.inventory.json" "$channel/n/release-manifest.json" \
    "$channel/n/root.json" "$channel/n-plus-1/release-manifest.json" \
    "$channel/n-plus-1/root.json"; do
    [ -f "$path" ] && [ ! -L "$path" ] || fail "an authenticated input is absent or unsafe"
done
from_cli_sha=$(/usr/bin/shasum -a 256 "$from/pkg-aarch64-darwin" | /usr/bin/awk '{print $1}')
to_cli_sha=$(/usr/bin/shasum -a 256 "$to/pkg-aarch64-darwin" | /usr/bin/awk '{print $1}')
from_pkg_sha=$(/usr/bin/shasum -a 256 "$from_pkg" | /usr/bin/awk '{print $1}')
to_pkg_sha=$(/usr/bin/shasum -a 256 "$to_pkg" | /usr/bin/awk '{print $1}')
[ "$from_pkg_sha" != "$to_pkg_sha" ] \
    || fail "release N and N+1 packages have the same authenticated digest"
for side in from to; do
    eval "directory=\$$side"
    (cd "$directory" && /usr/bin/shasum -a 256 --check "$evidence/$side-selected-sha256.txt") \
        >"$evidence/$side-checksums.txt" 2>&1 \
        || fail "a local candidate checksum failed"
done

printf '%s\n' "$PKG_PROOF_PAIR_SHA256" | /usr/bin/grep -Eq '^[0-9a-f]{64}$' \
    || fail "the proof pair digest is invalid"
[ "$PKG_PROOF_CHANNEL_URL" = "${PKG_PROOF_CHANNEL_URL%/}" ] \
    || fail "the proof channel URL has a trailing slash"
[ "$(/usr/bin/shasum -a 256 "$channel/proof-pair.json" | /usr/bin/awk '{print $1}')" \
    = "$PKG_PROOF_PAIR_SHA256" ] || fail "the proof pair digest does not match"

reviewed_commit=8ffd325a4be12a998f3a5684097b57841a11540e
for side in from to; do
    [ "$(/bin/cat "$evidence/$side-source-commit.txt")" = "$reviewed_commit" ] \
        || fail "release $side does not contain the reviewed DN-16 lifecycle"
done
/usr/bin/python3 -I - "$from/release-manifest.json" "$PKG_PROOF_FROM_RELEASE" \
    "$to/release-manifest.json" "$PKG_PROOF_TO_RELEASE" \
    >"$evidence/input-compatibility.txt" <<'PY'
import json
import sys

def require(condition, message):
    if not condition:
        raise SystemExit(message)

for path, release in zip(sys.argv[1::2], sys.argv[2::2]):
    manifest = json.load(open(path))
    require(manifest.get("schemaVersion") == 2, "invalid release manifest schema")
    require(manifest.get("releaseId") == release, "release manifest identity mismatch")
    determinate = manifest["determinate"]
    require(determinate.get("version") == "3.22.1", "Determinate version mismatch")
    require(determinate.get("revision") == "4132ad07a15ee7d88c096ac7172b7afb2672866b",
            "Determinate revision mismatch")
    artifacts = [
        item for item in determinate["artifacts"]
        if item.get("kind") == "installer" and item.get("system") == "aarch64-darwin"
    ]
    require(len(artifacts) == 1, "Apple Silicon Determinate installer is not unique")
    installer = artifacts[0]
    require(installer.get("target") == "determinate/3.22.1/nix-installer-aarch64-darwin",
            "Determinate target mismatch")
    require(installer.get("length") == 58427232, "Determinate length mismatch")
    require(installer.get("sha256") ==
            "90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b",
            "Determinate digest mismatch")
    print(f"{release}: manifest-consistent dn16-reviewed determinate-3.22.1")
PY

printf '%s\n' \
    "channel_url=$PKG_PROOF_CHANNEL_URL" \
    "proof_pair_sha256=$PKG_PROOF_PAIR_SHA256" \
    >"$evidence/proof-channel.txt"
/usr/bin/python3 -I - "$channel" "$from/release-manifest.json" \
    "$to/release-manifest.json" "$PKG_PROOF_FROM_RELEASE" "$PKG_PROOF_TO_RELEASE" \
    >>"$evidence/proof-channel.txt" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

def require(condition, message):
    if not condition:
        raise SystemExit(message)

channel = pathlib.Path(sys.argv[1])
release_manifests = [pathlib.Path(path) for path in sys.argv[2:4]]
releases = sys.argv[4:6]
require({path.name for path in channel.iterdir()} == {
    "proof-pair.json", "n.inventory.json", "n-plus-1.inventory.json", "n", "n-plus-1",
}, "downloaded proof pair has extra top-level entries")
require(all(not path.is_symlink() and (path.is_file() or path.is_dir())
            for path in channel.rglob("*")),
        "downloaded proof pair contains a symlink or special file")
pair = json.loads((channel / "proof-pair.json").read_bytes())
require(set(pair) == {"schemaVersion", "channels", "productCommit"},
        "invalid proof pair")
require(pair["schemaVersion"] == 1, "invalid proof pair schema")
require(pair["productCommit"] == "8ffd325a4be12a998f3a5684097b57841a11540e",
        "proof pair product commit mismatch")
require(isinstance(pair["channels"], list) and len(pair["channels"]) == 2,
        "invalid proof pair channels")
roots = set()

for record, name, sequence, release, sealed_path in zip(
    pair["channels"], ("n", "n-plus-1"), (1, 2), releases, release_manifests
):
    require(isinstance(record, dict) and set(record) == {
        "channelSequence", "inventory", "inventoryLength", "inventorySha256",
        "manifestSchemaVersion", "name", "releaseId", "requiredMetadataPaths",
        "requiredTargetPrefix", "timestampVersion", "trustedRootSha256",
    }, "invalid proof channel record")
    require(record["name"] == name and record["releaseId"] == release,
            "proof channel release mismatch")
    require(record["channelSequence"] == sequence
            and record["timestampVersion"] == sequence
            and record["manifestSchemaVersion"] == 2,
            "proof channel sequence mismatch")
    require(record["inventory"] == f"{name}.inventory.json"
            and record["requiredTargetPrefix"] == "targets/"
            and record["requiredMetadataPaths"] == [
        "metadata/1.root.json",
        f"metadata/{sequence}.targets.json",
        f"metadata/{sequence}.snapshot.json",
        "metadata/timestamp.json",
    ], "proof channel layout mismatch")
    require(re.fullmatch(r"[0-9a-f]{64}", record["trustedRootSha256"]) is not None,
            "invalid trusted root digest")
    roots.add(record["trustedRootSha256"])

    inventory_raw = (channel / record["inventory"]).read_bytes()
    require(len(inventory_raw) == record["inventoryLength"]
            and hashlib.sha256(inventory_raw).hexdigest() == record["inventorySha256"],
            "proof inventory digest mismatch")
    inventory = json.loads(inventory_raw)
    require(set(inventory) == {"schemaVersion", "files"}
            and inventory["schemaVersion"] == 1
            and isinstance(inventory["files"], list),
            "invalid proof inventory")
    files = {}
    for item in inventory["files"]:
        require(isinstance(item, dict) and set(item) == {"length", "path", "sha256"},
                "invalid proof inventory entry")
        path = item["path"]
        require(isinstance(path, str)
                and re.fullmatch(r"[A-Za-z0-9._/-]+", path) is not None
                and not path.startswith("/")
                and all(part not in {"", ".", ".."} for part in path.split("/"))
                and path not in files,
                "unsafe or duplicate proof path")
        require(isinstance(item["length"], int) and item["length"] >= 0
                and re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is not None,
                "invalid proof file identity")
        files[path] = item
    fixed = {
        "root.json", "release-manifest.json", "signing-audit.ndjson",
        *record["requiredMetadataPaths"],
    }
    version = release[1:]
    proof_inputs = {
        "proof-inputs/SHA256SUMS",
        "proof-inputs/SHA256SUMS.sigstore.json",
        "proof-inputs/release-manifest.json",
        "proof-inputs/COSIGN_IDENTITY.txt",
        "proof-inputs/COSIGN_ISSUER.txt",
        f"proof-inputs/pkg-{version}-preview.pkg",
        f"proof-inputs/pkg-{version}-preview.pkg.sigstore.json",
        "proof-inputs/pkg-aarch64-darwin",
        "proof-inputs/pkg-aarch64-darwin.sigstore.json",
    }
    require(set(files) >= fixed | proof_inputs
            and all(path in fixed or path in proof_inputs
                    or path.startswith("targets/") for path in files)
            and any(path.startswith("targets/") for path in files),
            "proof inventory has missing or extra entries")
    require(list(files) == sorted(files), "proof inventory is not canonical")
    actual = {
        path.relative_to(channel / name).as_posix()
        for path in (channel / name).rglob("*") if path.is_file()
    }
    require(actual == set(files), "downloaded proof tree does not match its inventory")
    expected_directories = {
        parent.as_posix()
        for path in files
        for parent in pathlib.PurePosixPath(path).parents
        if parent.as_posix() != "."
    }
    actual_directories = {
        path.relative_to(channel / name).as_posix()
        for path in (channel / name).rglob("*") if path.is_dir()
    }
    require(actual_directories == expected_directories,
            "downloaded proof tree has missing or extra directories")
    for path, item in files.items():
        candidate = channel / name / path
        require(candidate.is_file() and not candidate.is_symlink(), "unsafe proof file")
        raw = candidate.read_bytes()
        require(len(raw) == item["length"]
                and hashlib.sha256(raw).hexdigest() == item["sha256"],
                "downloaded proof file identity mismatch")
    require(files["root.json"]["sha256"] == record["trustedRootSha256"],
            "trusted root digest mismatch")

    channel_manifest = json.loads((channel / name / "release-manifest.json").read_bytes())
    release_manifest = json.loads(sealed_path.read_bytes())
    require(channel_manifest == release_manifest,
            "channel manifest does not match authenticated proof-input manifest")
    require(channel_manifest.get("releaseId") == release
            and channel_manifest.get("channelSequence") == sequence
            and channel_manifest.get("timestampVersion") == sequence
            and channel_manifest.get("trustedRootSha256") == record["trustedRootSha256"],
            "channel manifest identity mismatch")
    cli = [
        artifact for artifact in channel_manifest["cliArtifacts"]
        if artifact["kind"] == "pkg" and artifact["system"] == "aarch64-darwin"
    ]
    require(len(cli) == 1, "Apple Silicon CLI is not unique")
    cli = cli[0]
    require(cli.get("source") == "cli/pkg-aarch64-darwin"
            and cli.get("sigstoreBundle") == "cli/pkg-aarch64-darwin.sigstore.json",
            "Apple Silicon CLI paths are invalid")
    for source, sha_key, length_key in (
        (cli["source"], "sha256", "length"),
        (cli["sigstoreBundle"], "sigstoreBundleSha256", "sigstoreBundleLength"),
    ):
        raw = (sealed_path.parent / pathlib.PurePosixPath(source).name).read_bytes()
        require(len(raw) == cli[length_key]
                and hashlib.sha256(raw).hexdigest() == cli[sha_key],
                "signed Apple Silicon CLI input mismatch")
    print(
        f"{name} release={release} sequence={sequence} "
        f"inventory_sha256={record['inventorySha256']} "
        f"root_sha256={record['trustedRootSha256']}"
    )

require(len(roots) == 1, "proof channels use different roots")
PY

echo "+ inspect the two authenticated packages"
/usr/sbin/pkgutil --expand-full "$from_pkg" "$work/from-package"
/usr/sbin/pkgutil --expand-full "$to_pkg" "$work/to-package"
from_installer="$work/from-package/Scripts/pkg-install"
to_installer="$work/to-package/Scripts/pkg-install"
for path in "$from_installer" "$to_installer"; do
    [ -x "$path" ] && [ ! -L "$path" ] || fail "the package installer is absent"
    /usr/bin/codesign --verify --strict --verbose=2 "$path"
done
/usr/bin/strings "$from_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n/metadata/" >/dev/null \
    || fail "release N does not embed the selected proof channel"
/usr/bin/strings "$from_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n/targets/" >/dev/null \
    || fail "release N does not embed the selected proof targets"
/usr/bin/strings "$to_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n-plus-1/metadata/" >/dev/null \
    || fail "release N+1 does not embed the selected proof channel"
/usr/bin/strings "$to_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n-plus-1/targets/" >/dev/null \
    || fail "release N+1 does not embed the selected proof targets"
! /usr/bin/strings "$from_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n-plus-1/" >/dev/null \
    || fail "release N also embeds the N+1 proof channel"
! /usr/bin/strings "$to_installer" | /usr/bin/grep -F \
    "$PKG_PROOF_CHANNEL_URL/n/metadata/" >/dev/null \
    || fail "release N+1 also embeds the N proof channel"
/usr/bin/python3 -I - "$work/from-package/PackageInfo" "$from_version" \
    "$work/to-package/PackageInfo" "$to_version" <<'PY'
import sys
import xml.etree.ElementTree as ET

for path, version in zip(sys.argv[1::2], sys.argv[2::2]):
    package = ET.parse(path).getroot()
    if (
        package.tag != "pkg-info"
        or package.attrib.get("identifier") != "org.pkg.installer.preview"
        or package.attrib.get("version") != version
    ):
        raise SystemExit("package identity mismatch")
PY
printf '%s\n' \
    "from_release=$PKG_PROOF_FROM_RELEASE package_sha256=$from_pkg_sha" \
    "to_release=$PKG_PROOF_TO_RELEASE package_sha256=$to_pkg_sha" \
    >"$evidence/authenticated-package-identities.txt"

if [ "$PKG_PROOF_PHASE" = prepare ]; then
cat >"$evidence/expected-results.tsv" <<'EOF'
class	row	result
runner	fresh-runner-reboot	pass
input	dn16-source-compatibility	pass
input	release-manifest-consistency	pass
input	authenticated-package-distinctness	pass
input	sealed-proof-pair	pass
compiled	process-handoff-and-ordering	pass
native	fresh-release-n-install	pass
native	accepted-state-and-running-jobs	pass
native	exact-product-jobs	pass
native	repeat-noop	pass
native	offline-product-repair	pass
native	representative-package-state	pass
external	staged-channel-n-to-n-plus-1-upgrade	pass
native	prepare-state-preserved	pass
runner	continuation-recorded	pass
EOF
printf 'class\trow\tresult\nrunner\tfresh-runner-reboot\tpass\ninput\tdn16-source-compatibility\tpass\ninput\trelease-manifest-consistency\tpass\ninput\tauthenticated-package-distinctness\tpass\ninput\tsealed-proof-pair\tpass\n' \
    >"$evidence/results.tsv"
else
cat >"$evidence/expected-results.tsv" <<'EOF'
class	row	result
input	dn16-source-compatibility	pass
input	release-manifest-consistency	pass
input	authenticated-package-distinctness	pass
input	sealed-proof-pair	pass
runner	continuation-reboot	pass
external	n-plus-1-resumed-offline	pass
native	resume-state-preserved	pass
native	services-activated	pass
native	package-lifecycle	pass
native	package-remove	pass
native	structured-uninstall-refusal	pass
native	terminal-uninstall-completion	pass
native	final-absence	pass
native	vendor-residue	pass
EOF
printf 'class\trow\tresult\ninput\tdn16-source-compatibility\tpass\ninput\trelease-manifest-consistency\tpass\ninput\tauthenticated-package-distinctness\tpass\ninput\tsealed-proof-pair\tpass\n' \
    >"$evidence/results.tsv"
fi
printf '%s\n' "lifecycle_run=$PKG_PROOF_LIFECYCLE_RUN" "status=incomplete" \
    "phase=$PKG_PROOF_PHASE" "from=$PKG_PROOF_FROM_RELEASE" \
    "to=$PKG_PROOF_TO_RELEASE" >"$evidence/result.txt"

finish() {
    if /usr/bin/grep -Fx status=incomplete "$evidence/result.txt" >/dev/null 2>&1; then
        /usr/bin/sed -i '' 's/^status=incomplete$/status=failed/' "$evidence/result.txt"
    fi
}
trap finish EXIT HUP INT TERM

pass() { printf '%s\t%s\tpass\n' "$1" "$2" >>"$evidence/results.tsv"; }

capture() {
    label=$1
    shift
    set +e
    "$@" >"$work/$label.log" 2>&1
    status=$?
    set -e
    /usr/bin/tail -c 65536 "$work/$label.log" >"$evidence/$label.log"
    [ "$status" -eq 0 ] || {
        /bin/cat "$evidence/$label.log" >&2
        fail "$label failed"
    }
}

assert_accepted() {
    /usr/bin/sudo /usr/bin/python3 -I -c '
import json
record=json.load(open("/private/var/db/pkg-install/determinate-handoff-v1.json"))
if not (
    record.get("schema_version") == 1
    and record.get("state", {}).get("kind") == "accepted"
    and record["state"].get("installer", {}).get("length") == 58427232
    and record["state"]["installer"].get("sha256") ==
        "sha256-90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b"
):
    raise SystemExit("Determinate handoff is not accepted")
'
}

assert_installed_release() {
    /usr/bin/sudo /usr/bin/python3 -I - "$1" >"$2" <<'PY'
import json
import sys

release = json.load(open(sys.argv[1]))
descriptors = [item for item in release["artifacts"] if item["kind"] == "descriptor"]
if len(descriptors) != 1:
    raise SystemExit("authenticated descriptor is not unique")
expected = f"sha256-{descriptors[0]['sha256']}"
receipt = json.load(open("/opt/pkg/uninstall/manifest.json"))
if receipt.get("ownershipManifestDigest") != expected:
    raise SystemExit("installed ownership identity mismatch")
print(f"ownership_manifest_digest={expected}")
PY
}

assert_jobs_running() {
    for label in org.pkg.root-helper org.pkg.nix-broker; do
        /bin/launchctl print "system/$label" | /usr/bin/grep -F 'state = running' >/dev/null \
            || fail "$label is not running"
    done
}

assert_exact_jobs() {
    /usr/bin/find /Library/LaunchDaemons -maxdepth 1 -type f -name 'org.pkg.*.plist' \
        -exec /usr/bin/basename {} \; | LC_ALL=C /usr/bin/sort >"$work/product-jobs"
    printf '%s\n' org.pkg.nix-broker.plist org.pkg.root-helper.plist \
        | LC_ALL=C /usr/bin/sort | /usr/bin/cmp - "$work/product-jobs" \
        || fail "the product launchd set is not exact"
    [ ! -e /opt/pkg/nix ] && [ ! -L /opt/pkg/nix ] || fail "the private Nix path exists"
}

stop_product() {
    for label in org.pkg.nix-broker org.pkg.root-helper; do
        /usr/bin/sudo /bin/launchctl bootout "system/$label"
        /usr/bin/sudo /bin/launchctl disable "system/$label"
        ! /bin/launchctl print "system/$label" >/dev/null 2>&1 || fail "$label remains active"
    done
}

start_product() {
    for label in org.pkg.root-helper org.pkg.nix-broker; do
        /usr/bin/sudo /bin/launchctl enable "system/$label"
        /usr/bin/sudo /bin/launchctl bootstrap system "/Library/LaunchDaemons/$label.plist"
    done
    assert_jobs_running
}

assert_services_offline() {
    for label in org.pkg.root-helper org.pkg.nix-broker; do
        ! /bin/launchctl print "system/$label" >/dev/null 2>&1 \
            || fail "$label became active while Product services must stay offline"
    done
}

snapshot_service_state() {
    assert_services_offline
    printf '%s\n' \
        'org.pkg.root-helper=offline' \
        'org.pkg.nix-broker=offline' >"$1"
}

snapshot_package_state() {
    /usr/bin/python3 -I - "$HOME/Library/Application Support/pkg" >"$1" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
if not root.is_dir() or root.is_symlink():
    raise SystemExit("package state root is absent or unsafe")
for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
    relative = path.relative_to(root).as_posix()
    metadata = path.lstat()
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISREG(metadata.st_mode):
        raw = path.read_bytes()
        print(f"file\t{mode:o}\t{len(raw)}\t{hashlib.sha256(raw).hexdigest()}\t{relative}")
    elif stat.S_ISDIR(metadata.st_mode):
        print(f"dir\t{mode:o}\t{relative}")
    elif stat.S_ISLNK(metadata.st_mode):
        print(f"link\t{mode:o}\t{os.readlink(path)}\t{relative}")
    else:
        raise SystemExit("package state contains a special file")
PY
}

snapshot_base_nix() {
    /usr/bin/sudo /usr/bin/python3 -I - >"$1" <<'PY'
import hashlib
import os
import pathlib
import stat

paths = (
    pathlib.Path("/nix/var/nix/db/db.sqlite"),
    pathlib.Path("/nix/var/nix/profiles/default"),
    pathlib.Path("/etc/nix/nix.conf"),
    pathlib.Path("/Library/LaunchDaemons/systems.determinate.nix-daemon.plist"),
)
for path in paths:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        print(f"absent\t{path}")
        continue
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISREG(metadata.st_mode):
        raw = path.read_bytes()
        print(f"file\t{mode:o}\t{len(raw)}\t{hashlib.sha256(raw).hexdigest()}\t{path}")
    elif stat.S_ISLNK(metadata.st_mode):
        print(f"link\t{mode:o}\t{os.readlink(path)}\t{path}")
    else:
        raise SystemExit(f"unsafe Base Nix state path: {path}")
PY
    /usr/bin/grep -F $'file\t' "$1" >/dev/null \
        || fail "the Base Nix snapshot has no regular file"
}

continuation=/private/var/db/pkg-dn16-proof-continuation-v1
continuation_state=/private/var/db/pkg-dn16-proof-continuation-state-v1

persist_prepare_state() {
    snapshot_service_state "$work/services.before"
    snapshot_package_state "$work/package-state.before"
    snapshot_base_nix "$work/base-nix.before"
    /usr/bin/sudo /bin/cp \
        /private/var/db/pkg-install/determinate-handoff-v1.json "$work/handoff.before"
    /usr/bin/sudo /bin/chown "$(/usr/bin/id -u):$(/usr/bin/id -g)" "$work/handoff.before"
    /usr/bin/sudo -n /bin/mkdir -p "$continuation_state"
    /usr/bin/sudo -n /bin/chown root:wheel "$continuation_state"
    /usr/bin/sudo -n /bin/chmod 0700 "$continuation_state"
    for name in handoff base-nix package-state services; do
        /usr/bin/sudo -n /usr/bin/install -o root -g wheel -m 0600 \
            "$work/$name.before" "$continuation_state/$name.before"
    done
}

compare_prepare_state() {
    snapshot_service_state "$work/services.after"
    snapshot_package_state "$work/package-state.after"
    snapshot_base_nix "$work/base-nix.after"
    /usr/bin/sudo /bin/cp \
        /private/var/db/pkg-install/determinate-handoff-v1.json "$work/handoff.after"
    /usr/bin/sudo /bin/chown "$(/usr/bin/id -u):$(/usr/bin/id -g)" "$work/handoff.after"
    for name in handoff base-nix package-state services; do
        /usr/bin/sudo -n /usr/bin/cmp "$continuation_state/$name.before" \
            "$work/$name.after" \
            || fail "$name changed across the offline N+1 transition"
    done
}

write_continuation() {
    prepare_boot=$(/usr/sbin/sysctl -n kern.bootsessionuuid)
    ownership=$(/usr/bin/sed -n 's/^ownership_manifest_digest=//p' \
        "$evidence/release-n-plus-1-ownership.txt")
    [ -n "$ownership" ] || fail "the N+1 ownership identity is absent"
    record="$work/continuation"
    {
        printf '%s\n' \
            'schema=PKG-DN16-CONTINUATION-V1' \
            "run_id=$GITHUB_RUN_ID" \
            "run_attempt=$GITHUB_RUN_ATTEMPT" \
            "lifecycle_run=$PKG_PROOF_LIFECYCLE_RUN" \
            "runner_name=$RUNNER_NAME" \
            "instance_nonce=$instance_nonce" \
            "prepare_boot_uuid=$prepare_boot" \
            "workflow_sha=${GITHUB_WORKFLOW_SHA:-}" \
            "from_release=$PKG_PROOF_FROM_RELEASE" \
            "to_release=$PKG_PROOF_TO_RELEASE" \
            "proof_pair_sha256=$PKG_PROOF_PAIR_SHA256" \
            "to_cli_sha256=$to_cli_sha" \
            "ownership_manifest_digest=$ownership"
        for name in handoff base-nix package-state services; do
            digest=$(/usr/bin/sudo -n /usr/bin/shasum -a 256 \
                "$continuation_state/$name.before" | /usr/bin/awk '{print $1}')
            printf '%s\n' "${name}_snapshot_sha256=$digest"
        done
        printf '%s\n' 'status=awaiting-reboot'
    } >"$record"
    /usr/bin/sudo -n /usr/bin/install -o root -g wheel -m 0600 "$record" "$continuation"
}

verify_continuation() {
    /usr/bin/sudo -n /bin/test -f "$continuation" \
        && /usr/bin/sudo -n /bin/test ! -L "$continuation" \
        && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' "$continuation")" = \
            root:wheel:600 ] \
        && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%z' "$continuation")" -le 4096 ] \
        || fail "the continuation record is absent or unsafe"
    /usr/bin/sudo -n /bin/test -d "$continuation_state" \
        && /usr/bin/sudo -n /bin/test ! -L "$continuation_state" \
        && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' \
            "$continuation_state")" = root:wheel:700 ] \
        || fail "the continuation state directory is absent or unsafe"
    /usr/bin/sudo -n /bin/cat "$continuation" >"$work/continuation.actual"
    /usr/bin/python3 -I - "$work/continuation.actual" <<'PY'
import pathlib
import re
import sys

lines = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
records = {}
for line in lines:
    key, separator, value = line.partition("=")
    if separator != "=" or not key or key in records:
        raise SystemExit("the continuation record is malformed")
    records[key] = value
expected = {
    "schema", "run_id", "run_attempt", "lifecycle_run", "runner_name", "instance_nonce",
    "prepare_boot_uuid", "workflow_sha", "from_release", "to_release",
    "proof_pair_sha256", "to_cli_sha256", "ownership_manifest_digest",
    "handoff_snapshot_sha256", "base-nix_snapshot_sha256",
    "package-state_snapshot_sha256", "services_snapshot_sha256", "status",
}
if set(records) != expected:
    raise SystemExit("the continuation record fields are not exact")
for key in ("instance_nonce", "workflow_sha", "proof_pair_sha256", "to_cli_sha256",
            "handoff_snapshot_sha256", "base-nix_snapshot_sha256",
            "package-state_snapshot_sha256", "services_snapshot_sha256"):
    if re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", records[key]) is None:
        raise SystemExit(f"invalid continuation identity: {key}")
PY
    continuation_value() {
        /usr/bin/sed -n "s/^$1=//p" "$work/continuation.actual"
    }
    [ "$(continuation_value schema)" = PKG-DN16-CONTINUATION-V1 ] \
        && [ "$(continuation_value run_id)" = "$GITHUB_RUN_ID" ] \
        && [ "$(continuation_value run_attempt)" = "$GITHUB_RUN_ATTEMPT" ] \
        && [ "$(continuation_value lifecycle_run)" = "$PKG_PROOF_LIFECYCLE_RUN" ] \
        && [ "$(continuation_value runner_name)" = "$RUNNER_NAME" ] \
        && [ "$(continuation_value instance_nonce)" = "$instance_nonce" ] \
        && [ "$(continuation_value workflow_sha)" = "${GITHUB_WORKFLOW_SHA:-}" ] \
        && [ "$(continuation_value from_release)" = "$PKG_PROOF_FROM_RELEASE" ] \
        && [ "$(continuation_value to_release)" = "$PKG_PROOF_TO_RELEASE" ] \
        && [ "$(continuation_value proof_pair_sha256)" = "$PKG_PROOF_PAIR_SHA256" ] \
        && [ "$(continuation_value to_cli_sha256)" = "$to_cli_sha" ] \
        && [ "$(continuation_value status)" = awaiting-reboot ] \
        || fail "the continuation record does not bind this resume job"
    old_boot=$(continuation_value prepare_boot_uuid)
    printf '%s\n' "$old_boot" | /usr/bin/grep -Eq \
        '^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$' \
        || fail "the continuation boot UUID is invalid"
    current_boot=$(/usr/sbin/sysctl -n kern.bootsessionuuid)
    [ "$old_boot" != "$current_boot" ] \
        || fail "the VM did not reboot after prepare"
    for name in handoff base-nix package-state services; do
        path="$continuation_state/$name.before"
        /usr/bin/sudo -n /bin/test -f "$path" \
            && /usr/bin/sudo -n /bin/test ! -L "$path" \
            && [ "$(/usr/bin/sudo -n /usr/bin/stat -f '%Su:%Sg:%Lp' "$path")" = \
                root:wheel:600 ] \
            || fail "a continuation snapshot is absent or unsafe"
        expected=$(continuation_value "${name}_snapshot_sha256")
        actual=$(/usr/bin/sudo -n /usr/bin/shasum -a 256 "$path" \
            | /usr/bin/awk '{print $1}')
        [ "$actual" = "$expected" ] || fail "a continuation snapshot digest changed"
    done
    printf '%s\n' \
        "lifecycle_run=$PKG_PROOF_LIFECYCLE_RUN" \
        "runner_name=$RUNNER_NAME" \
        "instance_nonce=$instance_nonce" \
        "prepare_boot_uuid=$old_boot" \
        "resume_boot_uuid=$current_boot" \
        'status=reboot-verified' >"$evidence/continuation.txt"
}

snapshot_uninstall_boundary() {
    output=$1
    {
        for path in /usr/local/bin/pkg /opt/pkg/uninstall/manifest.json \
            /private/var/db/pkg-install/determinate-handoff-v1.json \
            /private/var/db/pkg-install-journal/macos-transaction-v1.json \
            /Library/LaunchDaemons/org.pkg.root-helper.plist \
            /Library/LaunchDaemons/org.pkg.nix-broker.plist; do
            if /usr/bin/sudo /bin/test -f "$path"; then
                /usr/bin/sudo /usr/bin/stat -f '%N %Su:%Sg:%Lp %z' "$path"
                /usr/bin/sudo /usr/bin/shasum -a 256 "$path"
            else
                printf '%s absent\n' "$path"
            fi
        done
        /bin/launchctl print system/org.pkg.root-helper 2>&1 | /usr/bin/head -40
        /bin/launchctl print system/org.pkg.nix-broker 2>&1 | /usr/bin/head -40
    } >"$output"
}

if [ "$PKG_PROOF_PHASE" = resume ]; then
    echo "+ verify the same VM resumed after the offline N+1 transition"
    verify_continuation
    pass runner continuation-reboot
    [ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" \
        = "$to_cli_sha" ] || fail "resume did not retain the N+1 product CLI"
    assert_accepted
    assert_installed_release "$to/release-manifest.json" \
        "$evidence/release-n-plus-1-ownership.txt"
    [ "$(/usr/bin/sed -n 's/^ownership_manifest_digest=//p' \
        "$evidence/release-n-plus-1-ownership.txt")" \
        = "$(/usr/bin/sed -n 's/^ownership_manifest_digest=//p' \
            "$work/continuation.actual")" ] \
        || fail "the resumed N+1 ownership identity changed"
    assert_services_offline
    pass external n-plus-1-resumed-offline
    compare_prepare_state
    pass native resume-state-preserved
    start_product
    assert_exact_jobs
    pass native services-activated
else

echo "+ deterministic compiled process and recovery proofs"
for test in \
    bootstrap::tests::spawn_and_wait_uncertainty_preserves_started_and_refuses_retry \
    bootstrap::tests::crash_before_vendor_start_preserves_started_and_refuses_retry \
    bootstrap::tests::signal_preserves_started_and_refuses_retry \
    bootstrap::tests::real_supervisor_loss_preserves_started_and_refuses_second_start \
    bootstrap::tests::failed_installed_state_validation_preserves_started \
    bootstrap::tests::exit_zero_plus_installed_state_validation_accepts_handoff_exactly_once \
    determinate::tests::synchronous_supervisor_reaps_child_before_return \
    determinate_handoff::tests::terminal_uninstall_consumes_handoff_only_after_identity_revalidation \
    determinate_handoff::tests::synchronous_exec_error_restores_exact_accepted_handoff \
    determinate_handoff::tests::synchronous_exec_and_restore_failure_is_fail_closed \
    determinate_handoff::tests::sigkill_after_consume_leaves_unmarked_determinate_state_for_install_refusal \
    determinate_handoff::tests::sigkill_after_vendor_exec_keeps_later_outcome_unknown_and_refuses_retry \
    uninstall::tests::macos_removes_receipt_and_directories_before_broker_account \
    uninstall::tests::product_cleanup_failure_never_dispatches_terminal_vendor \
    platform::macos::tests::darwin_readiness_requires_every_fail_closed_gate; do
    capture "test-$(printf '%s' "$test" | /usr/bin/tr ':_' '..')" \
        "$harness/pkg-installer-tests" --exact "$test" --nocapture
done
pass compiled process-handoff-and-ordering

echo "+ clean install from signed release N"
capture fresh-install /usr/bin/sudo /usr/sbin/installer -pkg "$from_pkg" -target /
[ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" = "$from_cli_sha" ] \
    || fail "fresh install did not publish the release N product CLI"
pass native fresh-release-n-install
assert_accepted
assert_installed_release "$from/release-manifest.json" "$evidence/release-n-ownership.txt"
assert_jobs_running
pass native accepted-state-and-running-jobs
assert_exact_jobs
pass native exact-product-jobs

echo "+ exact active repeat is a no-op"
handoff_before=$(/usr/bin/sudo /usr/bin/shasum -a 256 \
    /private/var/db/pkg-install/determinate-handoff-v1.json | /usr/bin/awk '{print $1}')
capture repeat-noop /usr/bin/sudo "$from_installer"
/usr/bin/grep -Fx 'pkg is installed.' "$evidence/repeat-noop.log" >/dev/null \
    || fail "the repeated install did not report success"
assert_accepted
assert_jobs_running
[ "$(/usr/bin/sudo /usr/bin/shasum -a 256 \
    /private/var/db/pkg-install/determinate-handoff-v1.json | /usr/bin/awk '{print $1}')" \
    = "$handoff_before" ] || fail "the repeated install changed the Accepted handoff"
pass native repeat-noop

echo "+ same-release offline Product Asset Repair"
original_pkg=$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')
stop_product
/usr/bin/sudo /bin/chmod u+w /usr/local/bin/pkg
printf 'damaged product asset\n' | /usr/bin/sudo /usr/bin/tee /usr/local/bin/pkg >/dev/null
/usr/bin/sudo /bin/chmod 0755 /usr/local/bin/pkg
capture offline-repair /usr/bin/sudo "$from_installer" --repair-product-assets
/usr/bin/grep -Fx 'pkg product files are repaired. Product services remain offline.' \
    "$evidence/offline-repair.log" >/dev/null || fail "repair did not remain offline"
[ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" = "$original_pkg" ] \
    || fail "repair did not restore the product CLI"
start_product
pass native offline-product-repair

echo "+ create representative package state under release N"
pkg=/usr/local/bin/pkg
capture package-state-install "$pkg" --yes --json install hello ripgrep
test -x "$HOME/Library/Application Support/pkg/current/bin/hello" \
    || fail "the representative hello package is absent"
test -x "$HOME/Library/Application Support/pkg/current/bin/rg" \
    || fail "the representative ripgrep package is absent"
pass native representative-package-state

echo "+ authenticated staged channel upgrade from N to N+1"
stop_product
persist_prepare_state
capture staged-channel-upgrade /usr/bin/sudo "$to_installer"
/usr/bin/grep -Fx 'pkg product files are upgraded. Product services remain offline.' \
    "$evidence/staged-channel-upgrade.log" >/dev/null \
    || fail "the staged N+1 upgrade did not remain offline"
[ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" = "$to_cli_sha" ] \
    || fail "the staged N+1 upgrade did not publish the N+1 product CLI"
[ "$from_cli_sha" != "$to_cli_sha" ] || fail "the N and N+1 product CLIs are equal"
assert_accepted
assert_installed_release "$to/release-manifest.json" "$evidence/release-n-plus-1-ownership.txt"
assert_services_offline
compare_prepare_state
pass external staged-channel-n-to-n-plus-1-upgrade
pass native prepare-state-preserved

write_continuation
pass runner continuation-recorded
/usr/bin/cmp "$evidence/expected-results.tsv" "$evidence/results.tsv" \
    || fail "the prepare result matrix is incomplete"
/usr/bin/sed -i '' 's/^status=incomplete$/status=passed/' "$evidence/result.txt"
exit 0
fi

echo "+ package lifecycle"
pkg=/usr/local/bin/pkg
capture package-install "$pkg" --yes --json install hello ripgrep
capture package-update "$pkg" --json update
capture package-upgrade "$pkg" --yes --json upgrade ripgrep --no-build
capture package-rollback "$pkg" --json rollback
hello_path=$(/usr/bin/python3 -I -c 'import os; print(os.path.realpath(os.path.expanduser("~/Library/Application Support/pkg/current/bin/hello")))')
case "$hello_path" in /nix/store/*/bin/hello) ;; *) fail "hello escaped the Nix store" ;; esac
/usr/bin/sudo /bin/chmod u+w "$hello_path"
printf 'damaged\n' | /usr/bin/sudo /usr/bin/tee "$hello_path" >/dev/null
/usr/bin/sudo /bin/chmod a-w "$hello_path"
capture package-repair "$pkg" --yes --json repair
capture package-gc "$pkg" --yes --json gc --keep-generations 1 --max-age-days 0
pass native package-lifecycle

echo "+ remove one package through the native generation lifecycle"
state_root="$HOME/Library/Application Support/pkg"
generation_before=$(/usr/bin/readlink "$state_root/current") \
    || fail "the active generation link is absent before remove"
case "$generation_before" in activations/gen-*) ;; *) fail "the active generation is invalid" ;; esac
capture package-remove "$pkg" --yes --json remove ripgrep
generation_after=$(/usr/bin/readlink "$state_root/current") \
    || fail "the active generation link is absent after remove"
case "$generation_after" in activations/gen-*) ;; *) fail "the new active generation is invalid" ;; esac
[ "$generation_before" != "$generation_after" ] \
    || fail "package remove did not create and activate a new generation"
test -x "$state_root/current/bin/hello" \
    || fail "package remove lost the retained hello package"
test ! -e "$state_root/current/bin/rg" && test ! -L "$state_root/current/bin/rg" \
    || fail "package remove retained ripgrep in the active generation"
before_id=${generation_before#activations/}
test -f "$state_root/generations/$before_id.json" \
    || fail "package remove did not retain the prior generation record"
pass native package-remove

echo "+ structured live uninstall refuses before mutation"
snapshot_uninstall_boundary "$work/uninstall-before"
for flag in --json --jsonl; do
    set +e
    "$pkg" "$flag" --yes uninstall >"$work/uninstall-$flag.log" 2>&1
    status=$?
    set -e
    [ "$status" -eq 78 ] || fail "$flag live uninstall did not return EX_CONFIG"
    /usr/bin/grep -F 'live uninstall requires plain output' "$work/uninstall-$flag.log" >/dev/null \
        || fail "$flag live uninstall did not return the fixed refusal"
done
snapshot_uninstall_boundary "$work/uninstall-after"
/usr/bin/cmp "$work/uninstall-before" "$work/uninstall-after" \
    || fail "structured uninstall changed product state"
pass native structured-uninstall-refusal

echo "+ run plain terminal uninstall"
/bin/cp "$pkg" "$work/pkg-after-removal"
/bin/chmod 0755 "$work/pkg-after-removal"
capture terminal-uninstall /usr/bin/sudo "$work/pkg-after-removal" --yes uninstall
/usr/bin/grep -F 'Nix was uninstalled successfully' "$evidence/terminal-uninstall.log" >/dev/null \
    || fail "the terminal vendor uninstall did not report completion"
pass native terminal-uninstall-completion

echo "+ final product and Base Nix absence"
for label in org.pkg.root-helper org.pkg.nix-broker org.nixos.nix-daemon \
    systems.determinate.nix-daemon; do
    ! /bin/launchctl print "system/$label" >/dev/null 2>&1 || fail "$label remains active"
done
for path in /nix /opt/pkg /usr/local/bin/pkg '/Library/Application Support/pkg' \
    "$HOME/Library/Application Support/pkg" \
    /private/var/db/pkg-install-auth /private/var/db/pkg-install-journal \
    /Library/LaunchDaemons/org.pkg.root-helper.plist \
    /Library/LaunchDaemons/org.pkg.nix-broker.plist; do
    [ ! -e "$path" ] && [ ! -L "$path" ] || fail "$path remains"
done
for record in /Users/pkg-nix-broker /Groups/pkg-nix-broker; do
    ! /usr/bin/dscl . -read "$record" >/dev/null 2>&1 || fail "$record remains"
done
users=$(/usr/bin/dscl . -list /Users) || fail "the final user list is unavailable"
groups=$(/usr/bin/dscl . -list /Groups) || fail "the final group list is unavailable"
! printf '%s\n' "$users" | /usr/bin/grep -Eq '^_?nixbld[0-9]+$' \
    || fail "a Base Nix build user remains"
! printf '%s\n' "$groups" | /usr/bin/grep -Eq '^_?nixbld$' \
    || fail "the Base Nix build group remains"
pass native final-absence

echo "+ record permitted vendor residue"
{
    for path in /private/var/db/pkg-install /private/etc/nix /private/etc/synthetic.conf; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            /usr/bin/sudo /usr/bin/stat -f '%N %HT %Su:%Sg %Sp' "$path"
        else
            printf '%s absent\n' "$path"
        fi
    done
} >"$evidence/vendor-residue.txt"
pass native vendor-residue

/usr/bin/cmp "$evidence/expected-results.tsv" "$evidence/results.tsv" \
    || fail "the proof result matrix is incomplete"
/usr/bin/sudo -n /bin/rm \
    "$continuation_state/handoff.before" \
    "$continuation_state/base-nix.before" \
    "$continuation_state/package-state.before" \
    "$continuation_state/services.before" \
    "$continuation"
/usr/bin/sudo -n /bin/rmdir "$continuation_state"
/usr/bin/sed -i '' 's/^status=incomplete$/status=passed/' "$evidence/result.txt"
