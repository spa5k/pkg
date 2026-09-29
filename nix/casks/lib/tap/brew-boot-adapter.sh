#!/usr/bin/env bash
# pkg CONTROLLED BOOT ADAPTER over the pinned Homebrew tree.
#
# Why this exists (parent-approved startup adaptation, nothing else):
# upstream `bin/brew` hardcodes PATH="/usr/bin:/bin:/usr/sbin:/sbin",
# then `exec /usr/bin/env -i "${FILTERED_ENV[@]}" /bin/bash -p brew.sh`,
# which (a) does not exist on Nix Linux (no /usr/bin/env, no /bin/bash)
# and (b) strips every PKG_EXPORT_* contract variable. This adapter
# performs ONLY the environment startup that upstream bin/brew performs
# before that broken exec (prefix/repository/library resolution and the
# HOMEBREW_* bookkeeping variables brew.sh expects) and then hands
# control to the UNMODIFIED upstream loader
# ${HOMEBREW_LIBRARY}/Homebrew/brew.sh with the full environment kept.
# All actual Homebrew loader logic, cask DSL, and vendored bundle stay
# upstream. The only other adaptation (parent-approved) is applied by
# raw-export.nix: utils/ruby.sh keeps a pre-supplied valid
# HOMEBREW_RUBY_PATH (the Nix Ruby) instead of unsetting it.
set -eu

fail() {
  echo "pkg-boot: FAIL: $1" >&2
  exit 1
}

[ -n "${HOMEBREW_LIBRARY:-}" ] || fail "HOMEBREW_LIBRARY is not set"
brew_sh="${HOMEBREW_LIBRARY}/Homebrew/brew.sh"
[ -f "${brew_sh}" ] || fail "upstream brew.sh missing at ${brew_sh}"
[ -n "${HOME:-}" ] || fail "HOME is not set"

# Same values upstream bin/brew derives from bin/brew's own location.
# HOMEBREW_LIBRARY is <brew-root>/Library: ONE dirname yields the brew
# root (two dirnames would climb to the staging parent).
brew_root="$(dirname "${HOMEBREW_LIBRARY}")"
brew_root="$(cd "${brew_root}" && pwd -P)"
export HOMEBREW_BREW_FILE="${brew_root}/bin/brew"
export HOMEBREW_REPOSITORY="${brew_root}"
export HOMEBREW_PREFIX="${HOMEBREW_PREFIX:-${brew_root}}"
export HOMEBREW_USER_CONFIG_HOME="${HOME}/.homebrew"

# Record the caller-set HOMEBREW_* variables exactly as bin/brew does,
# so upstream env-config bookkeeping sees a consistent picture.
if [ -z "${HOMEBREW_USER_SET_VARS+set}" ]; then
  user_set=""
  for var in $(env | sed -n 's/^\(HOMEBREW_[A-Za-z0-9_]*\)=.*/\1/p' | sort); do
    user_set="${user_set} ${var}"
  done
  export HOMEBREW_USER_SET_VARS="${user_set# }"
fi

# Keep the original PATH for sub-brew invocations, like bin/brew does.
export HOMEBREW_PATH="${HOMEBREW_PATH:-${PATH}}"

bash_bin="$(command -v bash)" || fail "bash is missing from PATH"
exec "${bash_bin}" -p "${brew_sh}" "$@"
