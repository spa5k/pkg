set -eu
pkg_test_cli=$1
pkg_test_man="$HOME/Library/Application Support/pkg/current/share/man"
for pkg_test_mode in unset empty custom defaults present; do
    unset MANPATH
    case "$pkg_test_mode" in
        unset) pkg_test_expected="$pkg_test_man:" ;;
        empty) MANPATH=''; export MANPATH; pkg_test_expected="$pkg_test_man:" ;;
        custom) MANPATH='/custom/man'; export MANPATH; pkg_test_expected="$pkg_test_man:/custom/man" ;;
        defaults) MANPATH=':/custom/man:'; export MANPATH; pkg_test_expected="$pkg_test_man::/custom/man:" ;;
        present) MANPATH="$pkg_test_man"; export MANPATH; pkg_test_expected="$pkg_test_man" ;;
    esac
    eval "$("$pkg_test_cli" shellenv)"
    eval "$("$pkg_test_cli" shellenv)"
    eval "$("$pkg_test_cli" shellenv)"
    printf '%s: %s\n' "$pkg_test_mode" "$MANPATH"
    test "$MANPATH" = "$pkg_test_expected"
done
