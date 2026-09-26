#!/bin/sh
set -eu
eval "$(/tmp/pkg shellenv)"
pkg_user_bin="$HOME/Library/Application Support/pkg/current/bin"
pkg_check_path=$PATH
case ":$pkg_check_path:" in
    *:/usr/local/bin:*) ;;
    *) pkg_check_path="/usr/local/bin:$pkg_check_path" ;;
esac
case ":$pkg_check_path:" in
    *:"$pkg_user_bin":*) ;;
    *) pkg_check_path="$pkg_user_bin:$pkg_check_path" ;;
esac

PATH="$pkg_check_path" /tmp/pkg doctor --json
printf '%s\n' "$pkg_check_path" | /usr/bin/awk -F: '{ n=0; for (i=1;i<=NF;i++) if ($i ~ /pkg\/current\/bin$/) n++; if (n != 1) exit 1; print "managed PATH entries:", n }'
probe_dir=$(mktemp -d)
trap 'rm -rf "$probe_dir"' EXIT
printf '#!/bin/sh\nprintf "export PKG_SHELL_TEST=ready\\n"\n' > "$probe_dir/pkg"
chmod 700 "$probe_dir/pkg"
printf '[ ! -x "%s/pkg" ] || eval "$("%s/pkg" shellenv)"\n' "$probe_dir" "$probe_dir" > "$probe_dir/startup"
for shell in /bin/bash /bin/zsh; do
  "$shell" -c '. "$1"; test "$PKG_SHELL_TEST" = ready' shell "$probe_dir/startup"
done
rm "$probe_dir/pkg"
for shell in /bin/bash /bin/zsh; do
  "$shell" -c '. "$1"' shell "$probe_dir/startup"
done
printf '%s\n' 'PASS: Bash and zsh startup before and after binary removal'
