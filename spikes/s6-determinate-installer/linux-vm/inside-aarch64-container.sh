#!/bin/sh
set -eu

evidence=/evidence
endpoint=http://127.0.0.1:18080
status=0
canary_pid=

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
record() { printf '%s\n' "$1" | tee -a "$evidence/results"; }
sha256() { sha256sum -- "$1" | awk '{print $1}'; }
write_argv() { file=$1; shift; : >"$file"; for arg in "$@"; do printf '%s\n' "$arg" >>"$file"; done; }
stop_canary() {
    [ -n "$canary_pid" ] || return 0
    kill "$canary_pid" 2>/dev/null || :
    wait "$canary_pid" 2>/dev/null || :
    canary_pid=
}
capture_sentry() {
    stage=$1
    sentry=/etc/nix/sentry-endpoint
    prefix=$evidence/sentry-$stage
    if [ -L "$sentry" ]; then
        printf '%s\n' symlink >"$prefix.kind"
        stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$sentry" >"$prefix.stat"
    elif [ -f "$sentry" ]; then
        printf '%s\n' regular-file >"$prefix.kind"
        stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$sentry" >"$prefix.stat"
        sha256 "$sentry" >"$prefix.sha256"
    elif [ ! -e "$sentry" ]; then
        printf '%s\n' absent >"$prefix.kind"
    else
        printf '%s\n' other >"$prefix.kind"
        stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$sentry" >"$prefix.stat"
    fi
}

[ "$#" -eq 2 ] && [ "$1" = --approve-destructive-container ] || die "usage: inside-aarch64-container.sh --approve-destructive-container TARGET"
target=$2
case $target in
    aarch64-linux) machine=aarch64; expected_installer_sha=9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179 ;;
    x86_64-linux) machine=x86_64; expected_installer_sha=9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c ;;
    *) die "unsupported container target: $target" ;;
esac
installer=/input/nix-installer-$target
[ "$(id -u)" -eq 0 ] || die "container probe requires EUID 0"
[ "$(uname -s)" = Linux ] || die "container probe requires Linux"
[ -d "$evidence" ] && [ ! -L "$evidence" ] || die "evidence path is unsafe"
[ ! -e "$evidence/results" ] && [ ! -L "$evidence/results" ] || die "evidence results already exist"
for existing_path in /nix /etc/nix /usr/local/bin/determinate-nixd; do
    [ ! -e "$existing_path" ] && [ ! -L "$existing_path" ] || die "pre-existing Nix state: $existing_path"
done
trap stop_canary EXIT HUP INT TERM
umask 077
: >"$evidence/results"
uname -m >"$evidence/uname-machine"
uname -sr >"$evidence/uname-kernel"
[ "$(cat "$evidence/uname-machine")" = "$machine" ] || { record "FAIL: container is not $machine"; exit 1; }
record "PASS: container reports $machine"

[ -f "$installer" ] && [ ! -L "$installer" ] || { record 'FAIL: pinned installer input is unsafe'; exit 1; }
actual_installer_sha=$(sha256 "$installer")
printf '%s\n' "$expected_installer_sha" >"$evidence/installer.expected.sha256"
printf '%s\n' "$actual_installer_sha" >"$evidence/installer.actual.sha256"
[ "$actual_installer_sha" = "$expected_installer_sha" ] || { record 'FAIL: pinned installer digest mismatch'; exit 1; }
record "PASS: pinned $target installer digest matches"

"$installer" --version >"$evidence/installer-version.output" 2>&1
installer_version_status=$?
printf '%s\n' "$installer_version_status" >"$evidence/installer-version.status"
if [ "$installer_version_status" -eq 0 ] && grep -F -x 'nix-installer 3.22.1' "$evidence/installer-version.output" >/dev/null; then
    record 'PASS: pinned installer executes and reports version 3.22.1'
else
    record 'FAIL: pinned installer version contract'
    exit 1
fi

printf '%s\n' '0' >"$evidence/diagnostic-requests"
cat >/tmp/pkg-s6-diagnostic-canary.pl <<'PERL'
use strict;
use warnings;
use IO::Socket::INET;
my ($ready, $count) = @ARGV;
my $server = IO::Socket::INET->new(
    LocalAddr => '127.0.0.1', LocalPort => 18080, Listen => 5,
    ReuseAddr => 1, Proto => 'tcp'
) or die "loopback bind failed: $!";
open my $ready_fh, '>', $ready or die "ready file: $!";
print {$ready_fh} "ready\n";
close $ready_fh;
while (my $client = $server->accept()) {
    open my $in, '<', $count or die "count read: $!";
    my $value = <$in>;
    close $in;
    open my $out, '>', $count or die "count write: $!";
    print {$out} int($value || 0) + 1, "\n";
    close $out;
    print {$client} "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    close $client;
}
PERL
perl /tmp/pkg-s6-diagnostic-canary.pl "$evidence/diagnostic-canary.ready" "$evidence/diagnostic-requests" >"$evidence/diagnostic-canary.output" 2>&1 &
canary_pid=$!
i=0
while [ "$i" -lt 50 ] && [ ! -s "$evidence/diagnostic-canary.ready" ]; do i=$((i + 1)); sleep 0.1; done
[ -s "$evidence/diagnostic-canary.ready" ] && kill -0 "$canary_pid" 2>/dev/null || { record 'FAIL: loopback diagnostics canary did not start'; exit 1; }
record 'PASS: loopback diagnostics canary is active'

printf '%s\n' 'DETSYS_IDS_TELEMETRY=disabled' >"$evidence/install.env"
write_argv "$evidence/install.argv" "$installer" --diagnostic-endpoint "$endpoint" install linux --determinate --no-confirm --no-modify-profile --init none --extra-conf 'sandbox = false' 'filter-syscalls = false'
set +e
DETSYS_IDS_TELEMETRY=disabled "$installer" --diagnostic-endpoint "$endpoint" install linux --determinate --no-confirm --no-modify-profile --init none --extra-conf 'sandbox = false' 'filter-syscalls = false' >"$evidence/install.output" 2>&1
install_status=$?
set -e
printf '%s\n' "$install_status" >"$evidence/install.status"
sleep 2
cp "$evidence/diagnostic-requests" "$evidence/diagnostic-install.requests"
if [ "$install_status" -eq 0 ] && kill -0 "$canary_pid" 2>/dev/null && [ "$(cat "$evidence/diagnostic-install.requests")" -eq 0 ]; then
    record 'PASS: install returned zero and telemetry-disabled sent no request to the loopback endpoint'
else
    record 'FAIL: install or diagnostics-control contract'
    exit 1
fi

receipt=/nix/receipt.json
if [ -f "$receipt" ] && [ ! -L "$receipt" ] && [ -s "$receipt" ]; then
    stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$receipt" >"$evidence/receipt.stat"
    sha256 "$receipt" >"$evidence/receipt.sha256"
    record 'PASS: opaque receipt metadata and private SHA-256 recorded'
else
    record 'FAIL: opaque receipt is missing or unsafe'
    exit 1
fi

installed=/nix/nix-installer
if [ -x "$installed" ] && [ ! -L "$installed" ] && [ "$(sha256 "$installed")" = "$expected_installer_sha" ]; then
    stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$installed" >"$evidence/installed-copy.stat"
    sha256 "$installed" >"$evidence/installed-copy.sha256"
    record 'PASS: installed vendor copy matches the pinned asset'
else
    record 'FAIL: installed vendor copy contract'
    exit 1
fi

nix=/nix/var/nix/profiles/default/bin/nix
set +e
"$nix" --version >"$evidence/nix-version.output" 2>&1
nix_status=$?
set -e
printf '%s\n' "$nix_status" >"$evidence/nix-version.status"
if [ "$nix_status" -eq 0 ] && grep -F -x 'nix (Determinate Nix 3.22.1) 2.35.2' "$evidence/nix-version.output" >/dev/null; then
    record "PASS: installed $machine Nix executable runs"
else
    record "FAIL: installed $machine Nix executable"
    exit 1
fi

capture_sentry after-install

printf '%s\n' 'DETSYS_IDS_TELEMETRY=disabled' >"$evidence/uninstall.env"
write_argv "$evidence/uninstall.argv" "$installed" --diagnostic-endpoint "$endpoint" uninstall --no-confirm "$receipt"
set +e
DETSYS_IDS_TELEMETRY=disabled "$installed" --diagnostic-endpoint "$endpoint" uninstall --no-confirm "$receipt" >"$evidence/uninstall.output" 2>&1
uninstall_status=$?
set -e
printf '%s\n' "$uninstall_status" >"$evidence/uninstall.status"
sleep 2
cp "$evidence/diagnostic-requests" "$evidence/diagnostic-total.requests"
if [ "$uninstall_status" -eq 0 ] && kill -0 "$canary_pid" 2>/dev/null && [ "$(cat "$evidence/diagnostic-total.requests")" -eq 0 ]; then
    record 'PASS: uninstall returned zero and telemetry-disabled sent no request to the loopback endpoint'
else
    record 'FAIL: uninstall or diagnostics-control contract'
    status=1
fi

capture_sentry after-uninstall
if [ -f /etc/nix/sentry-endpoint ] && [ ! -L /etc/nix/sentry-endpoint ] && cmp -s "$evidence/sentry-after-install.sha256" "$evidence/sentry-after-uninstall.sha256"; then
    record 'PASS: sentry endpoint private identity is unchanged after uninstall'
else
    record 'FAIL: sentry endpoint identity after uninstall'
    status=1
fi

: >"$evidence/residue.txt"
for path in /nix/receipt.json /nix /usr/local/bin/determinate-nixd; do
    [ ! -e "$path" ] && [ ! -L "$path" ] || printf '%s\n' "$path" >>"$evidence/residue.txt"
done
find /etc/systemd/system /usr/lib/systemd/system /lib/systemd/system \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print >>"$evidence/residue.txt" 2>/dev/null || :
find /usr/local/bin -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print >>"$evidence/residue.txt" 2>/dev/null || :
getent passwd | cut -d: -f1 | grep -E '^nixbld[0-9]+$' >>"$evidence/residue.txt" || :

etc_nix_ok=1
: >"$evidence/etc-nix.entries"
if [ -L /etc/nix ]; then
    stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- /etc/nix >"$evidence/etc-nix.stat" 2>&1 || :
    etc_nix_ok=0
elif [ -e /etc/nix ]; then
    stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- /etc/nix >"$evidence/etc-nix.stat" 2>&1 || :
    find /etc/nix -mindepth 1 -print | LC_ALL=C sort >"$evidence/etc-nix.entries"
    etc_nix_mode_owner=$(stat -c '%a %U:%G' /etc/nix)
    [ -d /etc/nix ] && [ "$etc_nix_mode_owner" = '755 root:root' ] && [ ! -s "$evidence/etc-nix.entries" ] || etc_nix_ok=0
else
    printf '%s\n' absent >"$evidence/etc-nix.stat"
fi

if [ "$(wc -l <"$evidence/etc-nix.entries" | tr -d ' ')" -eq 1 ] && grep -F -x '/etc/nix/sentry-endpoint' "$evidence/etc-nix.entries" >/dev/null; then
    record 'PASS: sentry endpoint is the only /etc/nix entry'
else
    record 'FAIL: exact /etc/nix entry inventory'
    status=1
fi

if [ "$etc_nix_ok" -eq 1 ] && [ ! -s "$evidence/residue.txt" ]; then
    record 'PASS: strict clean-uninstall residue contract'
else
    record 'FAIL: strict clean-uninstall residue contract'
    status=1
fi

exit "$status"
