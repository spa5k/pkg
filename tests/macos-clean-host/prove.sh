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
    PKG_PROOF_REBOOT_MARKER PKG_PROOF_LIFECYCLE_RUN; do
    require_env "$name"
done

[ "${GITHUB_ACTIONS:-}" = true ] || fail "GitHub Actions did not identify this runner"
[ "${RUNNER_ENVIRONMENT:-}" = self-hosted ] || fail "the runner is not self-hosted"
[ "$(/usr/bin/uname -s)" = Darwin ] || fail "the host is not macOS"
[ "$(/usr/bin/uname -m)" = arm64 ] || fail "the host is not Apple Silicon"
[ "$(/usr/sbin/sysctl -n kern.hv_vmm_present)" = 1 ] || fail "the host is not virtualized"
case "$(/usr/sbin/sysctl -n hw.model)" in VirtualMac*) ;; *) fail "the host is not a VirtualMac" ;; esac
case "$PKG_PROOF_LIFECYCLE_RUN" in 1|2) ;; *) fail "the lifecycle run is invalid" ;; esac
for tag in "$PKG_PROOF_FROM_RELEASE" "$PKG_PROOF_TO_RELEASE"; do
    printf '%s\n' "$tag" | /usr/bin/grep -Eq '^v0\.1\.0-alpha\.[1-9][0-9]*$' \
        || fail "a release tag is invalid"
done
[ "$PKG_PROOF_FROM_RELEASE" != "$PKG_PROOF_TO_RELEASE" ] || fail "the release tags are equal"

disposable=/var/tmp/pkg-disposable-macos-proof
[ -f "$disposable" ] && [ ! -L "$disposable" ] \
    && [ "$(/usr/bin/stat -f '%z' "$disposable")" -le 128 ] \
    || fail "the disposable marker is unsafe"
[ "$(/usr/bin/stat -f '%Su:%Sg:%Lp' "$disposable")" = root:wheel:600 ] \
    || fail "the disposable marker is unsafe"
[ "$(/bin/cat "$disposable")" = \
    "PKG-DN16-DISPOSABLE-V1:${GITHUB_RUN_ID}:$PKG_PROOF_LIFECYCLE_RUN" ] \
    || fail "the disposable marker does not bind this run"
/usr/bin/sudo -n /usr/bin/true || fail "passwordless administrative authority is unavailable"

# The external runner creates this marker, records the old boot session, reboots,
# and only then starts the workflow runner. A workflow step cannot resume itself.
[ -f "$PKG_PROOF_REBOOT_MARKER" ] && [ ! -L "$PKG_PROOF_REBOOT_MARKER" ] \
    && [ "$(/usr/bin/stat -f '%z' "$PKG_PROOF_REBOOT_MARKER")" -le 128 ] \
    || fail "the real-reboot marker is unsafe or absent"
[ "$(/usr/bin/stat -f '%Su:%Sg:%Lp' "$PKG_PROOF_REBOOT_MARKER")" = root:wheel:600 ] \
    || fail "the real-reboot marker is unsafe or absent"
old_boot=$(/usr/bin/sed -n \
    "s/^PKG-DN16-REBOOT-V1:$PKG_PROOF_LIFECYCLE_RUN://p" "$PKG_PROOF_REBOOT_MARKER")
current_boot=$(/usr/sbin/sysctl -n kern.bootsessionuuid)
[ "$(printf '%s' "$old_boot" | /usr/bin/grep -Ec '^[0-9A-Fa-f-]{36}$')" -eq 1 ] \
    || fail "the saved boot session is invalid"
[ -n "$old_boot" ] && [ "$old_boot" != "$current_boot" ] \
    || fail "the external runner did not prove a real reboot"

root=$PKG_PROOF_ROOT
case "$root" in "${RUNNER_TEMP:-/no-runner-temp}"/*) ;; *) fail "the proof root is unsafe" ;; esac
harness=$(CDPATH= cd -- "$(/usr/bin/dirname "$0")" && /bin/pwd -P)
from="$root/candidate/from"
to="$root/candidate/to"
evidence="$root/evidence"
work="$root/work"
/bin/mkdir -p -m 0700 "$evidence" "$work"

from_version=${PKG_PROOF_FROM_RELEASE#v}
to_version=${PKG_PROOF_TO_RELEASE#v}
from_pkg="$from/pkg-$from_version-preview.pkg"
to_pkg="$to/pkg-$to_version-preview.pkg"
for path in "$from_pkg" "$to_pkg" "$from/pkg-aarch64-darwin" \
    "$to/pkg-aarch64-darwin" "$harness/pkg-installer-tests"; do
    [ -f "$path" ] && [ ! -L "$path" ] || fail "an authenticated input is absent or unsafe"
done
from_cli_sha=$(/usr/bin/shasum -a 256 "$from/pkg-aarch64-darwin" | /usr/bin/awk '{print $1}')
to_cli_sha=$(/usr/bin/shasum -a 256 "$to/pkg-aarch64-darwin" | /usr/bin/awk '{print $1}')
for side in from to; do
    eval "directory=\$$side"
    (cd "$directory" && /usr/bin/shasum -a 256 --check "$evidence/$side-selected-sha256.txt") \
        >"$evidence/$side-checksums.txt" 2>&1 \
        || fail "a local candidate checksum failed"
done

reviewed_commit=8ffd325a4be12a998f3a5684097b57841a11540e
for side in from to; do
    [ "$(/bin/cat "$evidence/$side-source-commit.txt")" = "$reviewed_commit" ] \
        || fail "release $side does not contain the reviewed DN-16 lifecycle"
done
/usr/bin/python3 - "$from/release-manifest.json" "$PKG_PROOF_FROM_RELEASE" \
    "$to/release-manifest.json" "$PKG_PROOF_TO_RELEASE" \
    >"$evidence/input-compatibility.txt" <<'PY'
import json
import sys

for path, release in zip(sys.argv[1::2], sys.argv[2::2]):
    manifest = json.load(open(path))
    assert manifest["schemaVersion"] == 2
    assert manifest["releaseId"] == release
    determinate = manifest["determinate"]
    assert determinate["version"] == "3.22.1"
    assert determinate["revision"] == "4132ad07a15ee7d88c096ac7172b7afb2672866b"
    artifacts = [
        item for item in determinate["artifacts"]
        if item.get("kind") == "installer" and item.get("system") == "aarch64-darwin"
    ]
    assert len(artifacts) == 1
    installer = artifacts[0]
    assert installer["target"] == "determinate/3.22.1/nix-installer-aarch64-darwin"
    assert installer["length"] == 58427232
    assert installer["sha256"] == "90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b"
    print(f"{release}: dn16-reviewed determinate-3.22.1")
PY

cat >"$evidence/expected-results.tsv" <<'EOF'
class	row	result
runner	fresh-runner-reboot	pass
input	dn16-release-compatibility	pass
compiled	process-and-handoff-faults	pass
native	fresh-install	pass
native	accepted-state-and-running-jobs	pass
native	exact-product-jobs	pass
native	repeat-noop	pass
native	offline-product-repair	pass
native	offline-product-upgrade	pass
native	package-lifecycle	pass
native	structured-uninstall-refusal	pass
native	terminal-uninstall	pass
native	final-absence	pass
native	vendor-residue	pass
EOF
printf 'class\trow\tresult\nrunner\tfresh-runner-reboot\tpass\ninput\tdn16-release-compatibility\tpass\n' \
    >"$evidence/results.tsv"
printf '%s\n' "lifecycle_run=$PKG_PROOF_LIFECYCLE_RUN" "status=incomplete" \
    "from=$PKG_PROOF_FROM_RELEASE" "to=$PKG_PROOF_TO_RELEASE" >"$evidence/result.txt"

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
    /usr/bin/sudo /usr/bin/python3 -c '
import json
record=json.load(open("/private/var/db/pkg-install/determinate-handoff-v1.json"))
assert record["schema_version"] == 1
assert record["state"]["kind"] == "accepted"
assert record["state"]["installer"]["length"] == 58427232
assert record["state"]["installer"]["sha256"] == "sha256-90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b"
'
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
pass compiled process-and-handoff-faults

echo "+ extract the authenticated package installers"
/usr/sbin/pkgutil --expand-full "$from_pkg" "$work/from-package"
/usr/sbin/pkgutil --expand-full "$to_pkg" "$work/to-package"
from_installer="$work/from-package/Scripts/pkg-install"
to_installer="$work/to-package/Scripts/pkg-install"
for path in "$from_installer" "$to_installer"; do
    [ -x "$path" ] && [ ! -L "$path" ] || fail "the package installer is absent"
    /usr/bin/codesign --verify --strict --verbose=2 "$path"
done

echo "+ clean install from signed release N"
capture fresh-install /usr/bin/sudo /usr/sbin/installer -pkg "$from_pkg" -target /
[ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" = "$from_cli_sha" ] \
    || fail "fresh install did not publish release N product CLI"
pass native fresh-install
assert_accepted
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

echo "+ offline product upgrade from N to N+1"
stop_product
capture offline-upgrade /usr/bin/sudo "$to_installer"
/usr/bin/grep -Fx 'pkg product files are upgraded. Product services remain offline.' \
    "$evidence/offline-upgrade.log" >/dev/null || fail "upgrade did not remain offline"
assert_accepted
[ "$(/usr/bin/shasum -a 256 /usr/local/bin/pkg | /usr/bin/awk '{print $1}')" = "$to_cli_sha" ] \
    || fail "upgrade did not publish release N+1 product CLI"
start_product
pass native offline-product-upgrade

echo "+ package lifecycle"
pkg=/usr/local/bin/pkg
capture package-install "$pkg" --yes --json install hello ripgrep
capture package-update "$pkg" --json update
capture package-upgrade "$pkg" --yes --json upgrade ripgrep --no-build
capture package-rollback "$pkg" --json rollback
hello_path=$(/usr/bin/python3 -c 'import os; print(os.path.realpath(os.path.expanduser("~/Library/Application Support/pkg/current/bin/hello")))')
case "$hello_path" in /nix/store/*/bin/hello) ;; *) fail "hello escaped the Nix store" ;; esac
/usr/bin/sudo /bin/chmod u+w "$hello_path"
printf 'damaged\n' | /usr/bin/sudo /usr/bin/tee "$hello_path" >/dev/null
/usr/bin/sudo /bin/chmod a-w "$hello_path"
capture package-repair "$pkg" --yes --json repair
capture package-gc "$pkg" --yes --json gc --keep-generations 1 --max-age-days 0
pass native package-lifecycle

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

echo "+ product cleanup, then terminal Determinate uninstall"
/bin/cp "$pkg" "$work/pkg-after-removal"
/bin/chmod 0755 "$work/pkg-after-removal"
capture terminal-uninstall /usr/bin/sudo "$work/pkg-after-removal" --yes uninstall
pass native terminal-uninstall

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
/usr/bin/sed -i '' 's/^status=incomplete$/status=passed/' "$evidence/result.txt"
echo "macOS Apple Silicon lifecycle proof passed"
