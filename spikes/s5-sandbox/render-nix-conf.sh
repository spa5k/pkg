#!/bin/sh
set -eu

case "${1:-}" in
    linux)
        printf '%s\n' \
            'experimental-features = nix-command flakes cgroups' \
            'sandbox = true' \
            'sandbox-fallback = false' \
            'build-users-group = nixbld' \
            'use-cgroups = true' \
            'max-jobs = 1' \
            'cores = 1' \
            'timeout = 60' \
            'max-silent-time = 30' \
            'max-build-log-size = 1048576'
        ;;
    macos)
        printf '%s\n' \
            'experimental-features = nix-command flakes' \
            'sandbox = true' \
            'sandbox-fallback = false' \
            'build-users-group = nixbld' \
            'max-jobs = 1' \
            'cores = 1' \
            'timeout = 60' \
            'max-silent-time = 30' \
            'max-build-log-size = 1048576'
        ;;
    *)
        printf '%s\n' 'usage: render-nix-conf.sh linux|macos' >&2
        exit 64
        ;;
esac
