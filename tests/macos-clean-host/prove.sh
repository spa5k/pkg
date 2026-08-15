#!/bin/sh
set -eu

fail() {
    echo "macOS clean-host proof failed: $1" >&2
    exit 1
}

[ "$(/usr/bin/uname -s)" = Darwin ] || fail "the host is not macOS"
[ "${GITHUB_ACTIONS:-}" = true ] || fail "GitHub Actions did not identify this runner"
[ "${RUNNER_ENVIRONMENT:-}" = github-hosted ] || fail "the runner is not GitHub-hosted"
[ "${PKG_DISPOSABLE_MACOS_PROOF:-}" = confirmed ] || fail "the disposable-host gate is absent"
[ -f /var/tmp/pkg-disposable-macos-proof ] || fail "the root-owned disposable marker is absent"
[ "$(/usr/bin/stat -f '%Su:%Sg:%Lp' /var/tmp/pkg-disposable-macos-proof)" = root:wheel:600 ] \
    || fail "the disposable marker is unsafe"
/usr/bin/sudo -n /usr/bin/true || fail "passwordless administrative authority is unavailable"

bundle=$(CDPATH= cd -- "$(/usr/bin/dirname "$0")" && /bin/pwd -P)
work=$(/usr/bin/mktemp -d "${RUNNER_TEMP:-/tmp}/pkg-macos-proof.XXXXXX")
server_pid=
ca_fingerprint=
cleanup() {
    if [ -n "$server_pid" ]; then
        /bin/kill "$server_pid" >/dev/null 2>&1 || true
        wait "$server_pid" >/dev/null 2>&1 || true
    fi
    if [ -n "$ca_fingerprint" ]; then
        /usr/bin/sudo /usr/bin/security delete-certificate -Z "$ca_fingerprint" \
            /Library/Keychains/System.keychain >/dev/null 2>&1 || true
    fi
    /bin/rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

for path in \
    "$bundle/pkg-0.1.0-alpha.1-preview.pkg" \
    "$bundle/pkg-install" \
    "$bundle/publication-1/root.json" \
    "$bundle/publication-2/root.json"; do
    [ -e "$path" ] || fail "the proof artifact is incomplete"
done

/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
    -config "$bundle/openssl.cnf" \
    -keyout "$work/ca.key" \
    -out "$work/ca.crt" >/dev/null 2>&1
/usr/bin/openssl req -newkey rsa:2048 -nodes \
    -subj '/CN=127.0.0.1' \
    -keyout "$work/server.key" \
    -out "$work/server.csr" >/dev/null 2>&1
/usr/bin/openssl x509 -req -days 2 \
    -in "$work/server.csr" \
    -CA "$work/ca.crt" \
    -CAkey "$work/ca.key" \
    -CAcreateserial \
    -extfile "$bundle/openssl.cnf" \
    -extensions server_extensions \
    -out "$work/server.crt" >/dev/null 2>&1
/usr/bin/sudo /usr/bin/security add-trusted-cert -d -r trustRoot \
    -k /Library/Keychains/System.keychain "$work/ca.crt"
ca_fingerprint=$(/usr/bin/openssl x509 -in "$work/ca.crt" -noout -fingerprint -sha1 \
    | /usr/bin/cut -d= -f2 | /usr/bin/tr -d :)
/bin/ln -s "$bundle/publication-1" "$work/release"
PKG_PROOF_ROOT="$work/release" \
PKG_PROOF_CERTIFICATE="$work/server.crt" \
PKG_PROOF_PRIVATE_KEY="$work/server.key" \
    /usr/bin/python3 "$bundle/pkg-proof-server.py" >"$work/server.log" 2>&1 &
server_pid=$!

ready=false
attempt=0
while [ "$attempt" -lt 60 ]; do
    if /usr/bin/curl --fail --silent --cacert "$work/ca.crt" \
        https://127.0.0.1:8443/root.json >/dev/null; then
        ready=true
        break
    fi
    attempt=$((attempt + 1))
    /bin/sleep 1
done
[ "$ready" = true ] || fail "the local signed-publication server did not start"

product_volume_present() {
    /usr/sbin/diskutil apfs list 2>/dev/null | /usr/bin/grep -F 'pkg Nix Store' >/dev/null
}

volume_uuid() {
    /usr/sbin/diskutil info -plist /nix \
        | /usr/bin/plutil -extract VolumeUUID raw -
}

assert_services_ready() {
    /bin/launchctl print system/org.pkg.store-volume >/dev/null
    for label in \
        org.pkg.nix-daemon \
        org.pkg.root-helper \
        org.pkg.nix-broker; do
        /bin/launchctl print "system/$label" | /usr/bin/grep -F 'state = running' >/dev/null
    done
}

assert_services_absent() {
    for label in \
        org.pkg.store-volume \
        org.pkg.nix-daemon \
        org.pkg.root-helper \
        org.pkg.nix-broker; do
        if /bin/launchctl print "system/$label" >/dev/null 2>&1; then
            fail "a pkg launchd job remains"
        fi
    done
}

echo "+ interrupt install after real APFS creation"
/usr/bin/sudo /bin/sh -c 'echo $$ > "$1"; shift; exec "$@"' sh \
    "$work/install.pid" "$bundle/pkg-install" >"$work/interrupted-install.log" 2>&1 &
install_launcher=$!
store_created=false
attempt=0
while [ "$attempt" -lt 600 ]; do
    if /usr/bin/sudo /usr/bin/grep -F '"kind":"storeVolume","state":"created"' \
        /private/var/db/pkg-install/macos-transaction-v1.json >/dev/null 2>&1; then
        store_created=true
        break
    fi
    if ! /bin/kill -0 "$install_launcher" >/dev/null 2>&1; then
        break
    fi
    attempt=$((attempt + 1))
    /bin/sleep 0.05
done
if [ "$store_created" != true ]; then
    /usr/bin/tail -n 200 "$work/interrupted-install.log" >&2 || true
    fail "the APFS install checkpoint was not observed"
fi
install_pid=$(/bin/cat "$work/install.pid")
[ -n "$install_pid" ] || fail "the shipping pkg-install process was not found"
/usr/bin/sudo /bin/kill -KILL "$install_pid"
wait "$install_launcher" >/dev/null 2>&1 || true
product_volume_present || fail "the interrupted install did not leave its APFS volume"
first_volume_uuid=$(volume_uuid)

echo "+ recover interrupted install with the shipping artifact"
/usr/bin/sudo "$bundle/pkg-install"
product_volume_present || fail "the recovered install has no APFS volume"
second_volume_uuid=$(volume_uuid)
[ "$first_volume_uuid" != "$second_volume_uuid" ] \
    || fail "install recovery did not roll back and replace the APFS volume"

echo "+ install the technical-preview package and retry pkg-install"
/usr/bin/sudo /usr/sbin/installer \
    -pkg "$bundle/pkg-0.1.0-alpha.1-preview.pkg" \
    -target /
/usr/bin/sudo "$bundle/pkg-install"
assert_services_ready

echo "+ verify ordinary-user isolation"
[ "$(/usr/local/bin/pkg --version | /usr/bin/cut -d' ' -f1)" = pkg ]
! command -v nix >/dev/null 2>&1
[ ! -x /opt/pkg/bin/pkg-root-helper ]
[ ! -x /opt/pkg/bin/pkg-nix-broker ]
[ ! -x /opt/pkg/nix/current/bin/nix ]
[ ! -r /opt/pkg/etc/pkg/nix.conf ]
/bin/cp /usr/local/bin/pkg "$work/pkg-after-uninstall"
/bin/chmod 0755 "$work/pkg-after-uninstall"

state_root="$HOME/Library/Application Support/pkg"
echo "+ pkg install hello"
/usr/local/bin/pkg --yes install hello
/usr/local/bin/pkg --json list | /usr/bin/grep -F '"name":"hello"' >/dev/null
"$state_root/current/bin/hello" | /usr/bin/grep -F 'Hello, world!' >/dev/null

echo "+ cached pkg install hello"
/usr/local/bin/pkg --yes remove hello
cached_output=$(/usr/local/bin/pkg --yes --jsonl install hello)
if printf '%s\n' "$cached_output" | /usr/bin/grep -F '"type":"build_started"' >/dev/null; then
    fail "the cached hello install started a local build"
fi
"$state_root/current/bin/hello" | /usr/bin/grep -F 'Hello, world!' >/dev/null

echo "+ pkg install ripgrep"
/usr/local/bin/pkg --yes install ripgrep
"$state_root/current/bin/rg" --version | /usr/bin/grep -F 'ripgrep 13.0.0' >/dev/null

echo "+ explicit one-shot local build"
local_build_output=$(/usr/local/bin/pkg --yes --jsonl install cxx-prettyprint)
printf '%s\n' "$local_build_output" | /usr/bin/grep -F '"type":"build_started"' >/dev/null
printf '%s\n' "$local_build_output" | /usr/bin/grep -F '"selector":"cxx-prettyprint"' >/dev/null

echo "+ accept signed channel sequence 2"
/usr/bin/python3 -c \
    'import os,sys; next_root,link=sys.argv[1:]; os.symlink(next_root, link+".next"); os.replace(link+".next", link)' \
    "$bundle/publication-2" "$work/release"
channel_output=$(/usr/local/bin/pkg --json update)
printf '%s\n' "$channel_output" | /usr/bin/grep -F '"channelSequence":2' >/dev/null
printf '%s\n' "$channel_output" | /usr/bin/grep -F '"updated":true' >/dev/null
printf '%s\n' "$channel_output" | /usr/bin/grep -F '"stateUpdated":true' >/dev/null

echo "+ upgrade and rollback"
upgrade_output=$(/usr/local/bin/pkg --yes --json upgrade ripgrep --no-build)
printf '%s\n' "$upgrade_output" | /usr/bin/grep -F '"upgraded":["ripgrep"]' >/dev/null
"$state_root/current/bin/rg" --version | /usr/bin/grep -F 'ripgrep 15.1.0' >/dev/null
rollback_output=$(/usr/local/bin/pkg --json rollback)
printf '%s\n' "$rollback_output" | /usr/bin/grep -F '"sourceGeneration"' >/dev/null
printf '%s\n' "$rollback_output" | /usr/bin/grep -F '"targetGeneration"' >/dev/null
"$state_root/current/bin/rg" --version | /usr/bin/grep -F 'ripgrep 13.0.0' >/dev/null

echo "+ damage and repair the cached hello package"
hello_path=$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' \
    "$state_root/current/bin/hello")
case "$hello_path" in /nix/store/*/bin/hello) ;; *) fail "hello escaped the managed store" ;; esac
/usr/bin/sudo /bin/chmod u+w "$hello_path"
printf 'damaged\n' | /usr/bin/sudo /usr/bin/tee "$hello_path" >/dev/null
/usr/bin/sudo /bin/chmod a-w "$hello_path"
if verify_output=$(/usr/local/bin/pkg --json repair --verify-only 2>&1); then
    fail "repair verification did not detect damage"
fi
printf '%s\n' "$verify_output" | /usr/bin/grep -F '"symbol":"VERIFY_FAIL"' >/dev/null
repair_output=$(/usr/local/bin/pkg --yes --json repair)
printf '%s\n' "$repair_output" | /usr/bin/grep -F '"status":"repaired-from-cache"' >/dev/null
"$state_root/current/bin/hello" | /usr/bin/grep -F 'Hello, world!' >/dev/null

echo "+ refuse ownership drift without mutation"
/usr/bin/sudo /bin/chmod 0600 /Library/LaunchDaemons/org.pkg.nix-broker.plist
if /usr/bin/sudo "$bundle/pkg-install" >"$work/drift.log" 2>&1; then
    fail "pkg-install accepted changed launchd ownership state"
fi
[ "$(/usr/bin/stat -f '%Lp' /Library/LaunchDaemons/org.pkg.nix-broker.plist)" = 600 ] \
    || fail "the refusal mutated the changed plist"
/usr/bin/sudo /bin/chmod 0644 /Library/LaunchDaemons/org.pkg.nix-broker.plist
/usr/bin/sudo "$bundle/pkg-install"

echo "+ interrupt uninstall after APFS removal"
/usr/bin/sudo /bin/sh -c 'echo $$ > "$1"; shift; exec "$@"' sh \
    "$work/uninstall.pid" /usr/local/bin/pkg --yes uninstall \
    >"$work/interrupted-uninstall.log" 2>&1 &
uninstall_launcher=$!
uninstall_checkpoint=false
attempt=0
while [ "$attempt" -lt 600 ]; do
    if ! product_volume_present \
        && /usr/bin/sudo /usr/bin/test -f \
            /private/var/db/pkg-install/macos-transaction-v1.json; then
        uninstall_checkpoint=true
        break
    fi
    if ! /bin/kill -0 "$uninstall_launcher" >/dev/null 2>&1; then
        break
    fi
    attempt=$((attempt + 1))
    /bin/sleep 0.05
done
[ "$uninstall_checkpoint" = true ] || fail "the uninstall recovery checkpoint was not observed"
uninstall_pid=$(/bin/cat "$work/uninstall.pid")
[ -n "$uninstall_pid" ] || fail "the public pkg uninstall process was not found"
/usr/bin/sudo /bin/kill -KILL "$uninstall_pid"
wait "$uninstall_launcher" >/dev/null 2>&1 || true

echo "+ recover uninstall and prove idempotent absence"
/usr/bin/sudo "$work/pkg-after-uninstall" --yes uninstall
/usr/bin/sudo "$work/pkg-after-uninstall" --yes uninstall
assert_services_absent
for path in \
    /opt/pkg \
    /usr/local/bin/pkg \
    '/Library/Application Support/pkg' \
    /Library/LaunchDaemons/org.pkg.store-volume.plist \
    /Library/LaunchDaemons/org.pkg.nix-daemon.plist \
    /Library/LaunchDaemons/org.pkg.root-helper.plist \
    /Library/LaunchDaemons/org.pkg.nix-broker.plist \
    /private/etc/paths.d/pkg \
    /private/var/db/pkg-install \
    /private/var/db/pkg-install-auth \
    /private/var/db/pkg-install-accounts.lock \
    "$state_root"; do
    [ ! -e "$path" ] || fail "pkg residue remains"
done
for record in /Users/pkg-nix-broker /Groups/pkg-nix-broker /Groups/nixbld; do
    if /usr/bin/dscl . -read "$record" >/dev/null 2>&1; then
        fail "a pkg account remains"
    fi
done
if /usr/bin/dscl . -list /Users UniqueID | /usr/bin/grep -E '^_nixbld([1-9]|[12][0-9]|3[0-2])[[:space:]]' >/dev/null; then
    fail "a pkg build user remains"
fi
[ ! -e /private/etc/synthetic.conf ] || fail "synthetic.conf remains"
if /usr/bin/sudo /usr/bin/security find-generic-password \
    -s org.pkg.store-volume -a 'pkg Nix Store' >/dev/null 2>&1; then
    fail "the pkg Keychain item remains"
fi
product_volume_present && fail "the pkg APFS volume remains"
if /sbin/mount | /usr/bin/grep -F ' on /nix ' >/dev/null; then
    fail "a filesystem remains mounted at /nix"
fi
[ -d /nix ] || fail "the expected transient synthetic mount point is absent"
[ -z "$(/bin/ls -A /nix)" ] || fail "the transient synthetic mount point is not empty"

echo "+ verify foreign-Nix refusal without mutation"
/bin/mkdir -m 0700 "$work/foreign-bin"
/usr/bin/printf '%s\n' '#!/bin/sh' 'exit 0' >"$work/foreign-bin/nix"
/bin/chmod 0700 "$work/foreign-bin/nix"
if /usr/bin/sudo /usr/bin/env -i \
    PATH="$work/foreign-bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    "$bundle/pkg-install" >"$work/foreign.log" 2>&1; then
    fail "pkg-install accepted the unowned /nix path"
fi
[ ! -e /opt/pkg ] || fail "foreign-Nix refusal mutated /opt/pkg"
[ ! -e '/Library/Application Support/pkg' ] || fail "foreign-Nix refusal mutated services"

echo "macOS alpha clean-host proof passed"
