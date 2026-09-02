#!/usr/bin/env bash
# DN-1 task 1.5: hermetic workspace test run.
#
# The wrapper does three things:
#
# 1. Ambient-time audit. It scans every product crate's production source for
#    ALL wall-clock families (SystemTime, jiff Timestamp, time, chrono). A hit
#    is allowed only at the one seam implementation (pkg-core/src/clock.rs) or
#    at a record-only site from the grounding audit of 2026-09-03. Any other
#    hit fails the run, so a new decision site cannot hide from the audit.
#    (tools/ is release authoring, not product runtime; it is out of scope.)
# 2. Hermetic tripwire. It exports PKG_HERMETIC=1, which makes every ambient
#    SystemClock read panic (pkg_core::SystemClock). Tests that reach a
#    freshness decision must inject a fixed clock.
# 3. Network denial. Tests run with networking disabled: `unshare -n` on
#    Linux, `sandbox-exec` with a deny-network profile on macOS. Set
#    STRICT=1 to fail when no wrapper is available (macOS CI sets this).
#
# Usage: tools/verify/run-hermetic.sh [-- test-args...]
#        Everything after `--` is passed to cargo test.
#        AUDIT_ONLY=1 runs only the static ambient-time audit and the
#        temp-root audit (the fast ci-fast gates); no tests execute and no
#        network denial is needed.

set -euo pipefail
cd "$(dirname "$0")/../.."

FAMILIES=(
	'SystemTime::now('
	'Timestamp::now('
	'Zoned::now('
	'Utc::now('
	'OffsetDateTime::now_utc('
)

# Sanctioned ambient reads, as "path family" pairs (design.md, Amendment 2):
# - pkg-core/src/clock.rs is the one seam implementation; it owns the reads.
# - The record-only sites decide nothing and stay ambient:
#     pkg-cli/src/commands/local.rs      CLI lease identities and gc stamps
#     pkg-cli/src/log.rs                 log-row timestamps
#     pkg-installer/src/broker.rs        approval journal timestamp
#     pkg-installer/src/production_repair.rs  repair report timestamp
ALLOWED=(
	'crates/pkg-core/src/clock.rs SystemTime::now('
	'crates/pkg-core/src/clock.rs Timestamp::now('
	'crates/pkg-cli/src/commands/local.rs SystemTime::now('
	'crates/pkg-cli/src/log.rs SystemTime::now('
	'crates/pkg-installer/src/broker.rs SystemTime::now('
	'crates/pkg-installer/src/production_repair.rs SystemTime::now('
)

echo "==> ambient-time audit (all wall-clock families)"
violations=0
for family in "${FAMILIES[@]}"; do
	while IFS= read -r hit; do
		[ -z "$hit" ] && continue
		path="${hit%%:*}"
		allowed=""
		for entry in "${ALLOWED[@]}"; do
			if [ "$entry" = "$path $family" ]; then
				allowed=1
				break
			fi
		done
		if [ -z "$allowed" ]; then
			echo "AMBIENT-TIME VIOLATION ($family): $hit" >&2
			violations=1
		fi
	done < <(
		grep -RIFn "$family" crates --include='*.rs' 2>/dev/null \
			| grep -v '/tests/' | grep -v 'tests\.rs$' \
			| grep -v 'test_support' | grep -v '/pkg-testkit/' \
			|| true
	)
done

if [ "$violations" -ne 0 ]; then
	echo "==> ambient-time audit FAILED" >&2
	echo "    route the read through pkg_core::Clock, or record the" >&2
	echo "    record-only site in ALLOWED with the owner's justification." >&2
	exit 1
fi
echo "    clean"

# ---------------------------------------------------------------------------
# Temp-root audit (DN-1 PR-2).
#
# Product runtime code must use explicit roots, never the ambient temp dir.
# Tests may use temp roots (that is what they are for); this codebase keeps
# every test module either inline behind a `#[cfg(test)]` marker or in a
# sibling `tests.rs` / `tests/` path (skipped by find). pkg-testkit is the
# allowed everywhere. A hit outside both allowances fails the run: a new
# production temp-dir dependency cannot hide.
#
# Sanctioned production temp-dir uses (documented baseline):
# - pkg-nix/src/managed/installer_bundle.rs  spools streamed TUF targets to
#   an anonymous tempfile while digesting; deliberate, disk-bounded design
# - pkg-nix/src/managed/provision.rs          extraction TempPath plumbing
#   for the same bundle path
# Both stay until an explicit-root spool design lands (IMPL-NOTES-PR2).
TEMP_ALLOWED=(
	'crates/pkg-nix/src/managed/installer_bundle.rs'
	'crates/pkg-nix/src/managed/provision.rs'
)
# ---------------------------------------------------------------------------
echo "==> temp-root audit (production code must not use the ambient temp dir)"

violations=0
while IFS= read -r hit; do
	[ -z "$hit" ] && continue
	path="${hit%%:*}"
	skip=""
	for entry in "${TEMP_ALLOWED[@]}"; do
		if [ "$entry" = "$path" ]; then
			skip=1
			break
		fi
	done
	[ -n "$skip" ] && continue
	echo "    temp-root violation: $hit" >&2
	violations=$((violations + 1))
done < <(find crates -name '*.rs' -not -path '*/target/*' \
	-not -name 'tests.rs' -not -path '*/tests/*' -print0 \
	| xargs -0 awk '
		FNR == 1 { in_test = 0 }
		/#\[cfg\(test\)\]/ { in_test = 1 }
		in_test { next }
		/std::env::temp_dir\(|tempfile::|tempdir::|TempDir::new\(/ {
			print FILENAME ":" FNR
		}' \
	| grep -v '^crates/pkg-testkit/' || true)

if [ "$violations" -ne 0 ]; then
	echo "==> temp-root audit FAILED" >&2
	echo "    use an explicit product root, or the pkg-testkit harness" >&2
	exit 1
fi
echo "    clean"

if [ "${AUDIT_ONLY:-0}" = "1" ]; then
	echo "==> audit-only mode: skipping tripwire and test run"
	exit 0
fi

# Private per-invocation TMPDIR: tests that honor TMPDIR get a fresh root,
# removed on exit. The path is deliberately SHORT (/tmp/hm.XXXXXXXX):
# broker transport tests bind unix sockets whose paths extend TMPDIR, and
# macOS SUN_LEN is 104 bytes — a longer hermetic TMPDIR overflowed it on
# the CI probe (run 33676912359). Per-TEST isolation is enforced by the
# audit above plus the pkg-testkit harness, not by this wrapper.
HERMETIC_TMP="$(mktemp -d /tmp/hm.XXXXXXXX)"
export TMPDIR="$HERMETIC_TMP"
cleanup_tmpdir() { rm -rf "$HERMETIC_TMP"; }
trap cleanup_tmpdir EXIT

# Prefetch pinned dependencies while the network is still up: the hermetic
# run itself is offline (CARGO_NET_OFFLINE), but a fresh CI runner has an
# empty cargo cache, and fetching the --locked dependency set is not the
# network the tripwire is hunting. This must happen BEFORE the offline
# export.
cargo_bin="$(command -v cargo)"
"$cargo_bin" fetch --locked

export PKG_HERMETIC=1
export CARGO_NET_OFFLINE=true

cargo_args=("--workspace" "--locked")
if [ "${1:-}" = "--" ]; then
	shift
	cargo_args+=("$@")
fi

run_wrapped() {
	# $1 = label, rest = command
	local label="$1"
	shift
	echo "==> network denial: $label"
	"$@"
}

case "$(uname -s)" in
Linux)
	network_mode="none (no usable namespace)"
	if unshare -rn /usr/bin/true >/dev/null 2>&1; then
		# Unprivileged user namespace with an isolated network. Loopback
		# must come up because broker and channel tests bind 127.0.0.1.
		network_mode="unshare -rn (unprivileged userns)"
		run_wrapped "$network_mode" unshare -rn \
			bash -c 'ip link set lo up && exec "$0" test "${@}"' \
			"$cargo_bin" "${cargo_args[@]}"
	elif sudo -n unshare -n /usr/bin/true >/dev/null 2>&1; then
		# GitHub ubuntu runners block unprivileged uid_map writes (spike
		# verdict, run 33674556472). Passwordless sudo can still create a
		# network namespace as real root; setpriv then drops back to the
		# invoking user so cargo execs from $HOME/.cargo/bin with normal
		# permissions and owns its target/ files. sudo resets the
		# environment, so everything cargo needs is re-exported inside —
		# conditionally, because cargo rejects EMPTY CARGO_TARGET_DIR
		# (run 33676912359) and empty RUSTUP_TOOLCHAIN confuses rustup.
		network_mode="sudo unshare -n (root netns, setpriv back to invoking uid)"
		inner_env="export PATH='$PATH' HOME='$HOME' TMPDIR='$HERMETIC_TMP' PKG_HERMETIC=1 CARGO_NET_OFFLINE=true"
		[ -n "${RUSTUP_TOOLCHAIN:-}" ] \
			&& inner_env="$inner_env RUSTUP_TOOLCHAIN='$RUSTUP_TOOLCHAIN'"
		[ -n "${CARGO_TARGET_DIR:-}" ] \
			&& inner_env="$inner_env CARGO_TARGET_DIR='$CARGO_TARGET_DIR'"
		run_wrapped "$network_mode" sudo -n unshare -n \
			bash -c "$inner_env; ip link set lo up && exec setpriv --reuid=$(id -u) --regid=$(id -g) --clear-groups \"\$0\" test \"\${@}\"" \
			"$cargo_bin" "${cargo_args[@]}"
	elif command -v unshare >/dev/null 2>&1; then
		echo "error: no usable network namespace (unprivileged uid_map and sudo both refused)" >&2
		[ "${STRICT:-0}" = "1" ] && exit 1
		echo "warning: running WITHOUT network denial (set STRICT=1 in CI)" >&2
		run_wrapped "unwrapped (denial refused)" "$cargo_bin" test "${cargo_args[@]}"
	else
		echo "error: unshare is unavailable; cannot deny networking" >&2
		[ "${STRICT:-0}" = "1" ] && exit 1
		echo "warning: running WITHOUT network denial (set STRICT=1 in CI)" >&2
		run_wrapped "unwrapped (unshare missing)" "$cargo_bin" test "${cargo_args[@]}"
	fi
	;;
Darwin)
	profile="$(mktemp "${TMPDIR:-/tmp}/pkg-hermetic.XXXXXX.sb")"
	trap 'rm -f "$profile"' EXIT
	cat >"$profile" <<'EOF'
(version 1)
(deny network*)
(allow network-bind (local ip "*:*"))
(allow network-outbound (remote unix-socket))
(allow network-outbound (remote tcp4 "localhost:*"))
(allow network-outbound (remote tcp6 "localhost:*"))
EOF
	network_mode="sandbox-exec"
	# Newer macOS builds refuse every sandbox-exec profile, so probe
	# before relying on it. STRICT=1 (macOS CI) requires a working
	# wrapper; a developer laptop gets a loud unwrapped fallback.
	if command -v sandbox-exec >/dev/null 2>&1 \
		&& sandbox-exec -f "$profile" /usr/bin/true 2>/dev/null; then
		# Loopback stays reachable for local test servers; every remote
		# socket is denied. sandbox-exec prints a deprecation notice on
		# stderr; that is expected and not a failure.
		run_wrapped "$network_mode (remote network denied)" \
			sandbox-exec -f "$profile" "$cargo_bin" test "${cargo_args[@]}"
	else
		network_mode="none (sandbox-exec refused by this OS build)"
		echo "error: sandbox-exec cannot run on this host; cannot deny networking" >&2
		if [ "${STRICT:-0}" = "1" ]; then
			exit 1
		fi
		echo "warning: running WITHOUT network denial (set STRICT=1 in CI)" >&2
		run_wrapped "none" "$cargo_bin" test "${cargo_args[@]}"
	fi
	;;
*)
	echo "error: unsupported platform $(uname -s)" >&2
	exit 1
	;;
esac

echo "==> hermetic run passed: ambient-time audit clean, PKG_HERMETIC armed"
echo "    network denial: $network_mode"
