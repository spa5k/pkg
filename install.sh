#!/bin/sh
# Install pkg for the current user:
#
#   curl -fsSL https://github.com/spa5k/pkg/releases/latest/download/install.sh | sh
#
# The installer takes no options. It:
#   - downloads the pkg client of one exact release and verifies its checksum;
#   - installs ~/.local/bin/pkg and shell completions under ~/.local/share/pkg;
#   - adds pkg, Nix, and installed packages to PATH in Bash, Zsh, and Fish,
#     and turns on tab completion;
#   - turns on a daily user job that updates pkg and its search data, and
#     starts it once now.
#
# pkg needs Nix. When Nix is missing and a terminal is attached, the
# installer asks once, then runs the Determinate Nix installer, which asks
# for your password. pkg does not update, repair, or remove Nix. Without a
# terminal, the installer prints the Nix command and installs only pkg.
#
# The installer itself never uses sudo, never changes Nix settings, and
# never removes old installations.
#
# pkg updates itself later: `pkg update` and the daily job download and
# verify newer releases. Tests and benchmarks set PKG_INSTALL_MODE=test to
# skip the user job, because the job is one per user and a test home must
# not replace the real one.
#
# The managed shell blocks contain literal $PATH text on purpose.
# shellcheck disable=SC2016
set -eu

# The version must agree with the release built by
# tools/release/package_client.sh, which refuses a mismatch.
repo="spa5k/pkg"
version="0.2.0-alpha.18"
tag="v${version}"
nix_installer_url="https://install.determinate.systems/nix"
nix_command="curl -fsSL ${nix_installer_url} | sh -s -- install"

begin_marker="# >>> pkg paths >>>"
end_marker="# <<< pkg paths <<<"
fish_marker="# Managed by the pkg installer."

start_job=1
tmp=""
pending_new=""

# ---------------------------------------------------------------------------
# Output: a few short lines, and errors as one sentence plus one action.
# ---------------------------------------------------------------------------
say() {
  printf '%s\n' "$*"
}

done_step() {
  say "✓ $*"
}

fail() {
  printf 'error: %s\n' "$1" >&2
  shift
  for line do
    if [ -n "$line" ]; then
      printf '  %s\n' "$line" >&2
    fi
  done
  exit 1
}

cleanup() {
  if [ -n "$pending_new" ]; then
    rm -f -- "$pending_new"
  fi
  if [ -n "$tmp" ]; then
    rm -rf -- "$tmp"
  fi
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "pkg needs the $1 command. Install $1, then run this installer again."
}

# sq VALUE: VALUE single-quoted for POSIX shells. Spaces, quotes, dollars,
# backticks, and glob characters stay literal.
sq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# fq VALUE: VALUE single-quoted for Fish, which treats \\ and \' as escapes
# inside single quotes.
fq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/[\\\\']/\\\\&/g")"
}

# ---------------------------------------------------------------------------
# Nix detection. The inherited PATH is not trusted alone: the stable
# profile paths that pkg itself searches are checked next, in the same
# order. A candidate counts only when it runs and answers `nix --version`.
# ---------------------------------------------------------------------------
find_nix() {
  nix_bin=""
  nix_rejected=""
  for candidate in \
    "$(command -v nix 2>/dev/null || true)" \
    "/nix/var/nix/profiles/default/bin/nix" \
    "$state_home/nix/profile/bin/nix" \
    "$HOME/.nix-profile/bin/nix"
  do
    [ -n "$candidate" ] || continue
    # dash reports a match in a relative PATH entry as a relative path.
    # Pin it to the current directory, as pkg itself does.
    case "$candidate" in
      /*) ;;
      *) candidate="$(pwd)/$candidate" ;;
    esac
    if [ -f "$candidate" ] && [ -x "$candidate" ] \
      && "$candidate" --version >/dev/null 2>&1; then
      nix_bin="$candidate"
      return 0
    fi
    if [ -e "$candidate" ] && [ -z "$nix_rejected" ]; then
      nix_rejected="$candidate"
    fi
  done
}

# has_terminal: succeed when the user can answer a question.
has_terminal() {
  (exec </dev/tty >/dev/tty) 2>/dev/null
}

# offer_nix: ask once, then run the vendor installer. Succeed only when the
# user agreed and the vendor installer succeeded.
offer_nix() {
  if ! has_terminal; then
    return 1
  fi
  printf 'pkg needs Nix. Install Nix now? [Y/n] ' >/dev/tty
  answer=""
  read -r answer </dev/tty || answer="n"
  case "$answer" in
    "" | y | Y | yes | Yes | YES) ;;
    *) return 1 ;;
  esac
  say "Installing Nix with the Determinate Nix installer. It asks for your password."
  if ! curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/nix-installer.sh" "$nix_installer_url"; then
    printf 'Nix was not installed: the Nix installer could not be downloaded.\n' >&2
    return 1
  fi
  if ! sh "$tmp/nix-installer.sh" install --no-confirm </dev/tty; then
    printf 'Nix was not installed. The Nix installer stopped with an error.\n' >&2
    return 1
  fi
  return 0
}

# ---------------------------------------------------------------------------
# Archive members. Only exact members are read, through `tar -xO`, into
# files named here. The archive tree is never extracted, so absolute
# paths, `..`, and links in the archive cannot write anywhere.
# ---------------------------------------------------------------------------

# exact_regular MEMBER: succeed only when MEMBER is stored exactly once, as
# one regular file, with nothing below it.
exact_regular() {
  awk -v m="$1" '
    {
      name = $NF
      if ($0 ~ / -> /) {
        line = $0
        sub(/ -> .*/, "", line)
        n = split(line, fields, " ")
        name = fields[n]
      }
      if (name == m) {
        named++
        if (substr($1, 1, 1) == "-") regular++
      }
      if (index($0, m "/")) below = 1
    }
    END { exit !(regular == 1 && named == 1 && !below) }
  ' "$tmp/listing"
}

# read_member MEMBER DEST: write MEMBER's bytes to DEST, or fail.
read_member() {
  tar -xOf "$tmp/archive.tar" -- "$1" > "$2" \
    || fail "the pkg download is damaged: tar cannot read $1." "Run this installer again."
}

download_client() {
  archive="pkg-${version}-${system}.tar.gz"
  root="pkg-${version}-${system}"
  base="https://github.com/${repo}/releases/download/${tag}"
  if ! curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/$archive" "${base}/${archive}" \
    || ! curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/SHA256SUMS" "${base}/SHA256SUMS"; then
    fail "pkg ${version} could not be downloaded." "Check your internet connection, then run this installer again."
  fi

  want="$(grep " ${archive}\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
  [ -n "$want" ] || fail "the release has no checksum for ${archive}."
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
  else
    got="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
  fi
  # Both tools mark an escaped file name with a leading backslash.
  got="${got#\\}"
  [ "$got" = "$want" ] \
    || fail "the pkg download does not match its published checksum. Nothing was installed." "Run this installer again."

  gzip -dc "$tmp/$archive" > "$tmp/archive.tar" \
    || fail "the pkg download is damaged. Nothing was installed." "Run this installer again."
  tar -tvf "$tmp/archive.tar" > "$tmp/listing" \
    || fail "the pkg download is damaged. Nothing was installed." "Run this installer again."

  client="${root}/bin/pkg"
  exact_regular "$client" || fail "the pkg download has no single client file. Nothing was installed."
  read_member "$client" "$tmp/pkg"
  [ -s "$tmp/pkg" ] || fail "the pkg download has an empty client file. Nothing was installed."
  for shell in bash zsh fish; do
    if exact_regular "${root}/completions/${shell}.txt"; then
      read_member "${root}/completions/${shell}.txt" "$tmp/${shell}.completion"
    fi
  done
}

# install_client: start the new client from its final directory, then
# replace the old client with one rename. A running pkg keeps its file.
install_client() {
  mkdir -p "$bindir" "$compldir"
  previous=""
  if [ -x "$bindir/pkg" ]; then
    previous="$("$bindir/pkg" --version 2>/dev/null | awk '{print $2}' || true)"
  fi
  pending_new="$(mktemp "$bindir/.pkg-new.XXXXXX")"
  cat "$tmp/pkg" > "$pending_new"
  chmod 0755 "$pending_new"
  if ! "$pending_new" --version > "$tmp/version.out" 2>&1; then
    detail="$(head -n 1 "$tmp/version.out")"
    if [ "$system" = "x86_64-linux" ]; then
      fail "pkg cannot run on this computer. pkg needs glibc 2.35 or newer." "$detail"
    fi
    fail "pkg cannot run on this computer." "$detail"
  fi
  mv -f "$pending_new" "$bindir/pkg"
  pending_new=""

  # Completions go where each shell loads them through the managed setup.
  rm -f "$compldir/bash.txt" "$compldir/zsh.txt" "$compldir/fish.txt"
  for pair in bash:pkg.bash zsh:_pkg fish:pkg.fish; do
    shell="${pair%%:*}"
    if [ -f "$tmp/${shell}.completion" ]; then
      cp "$tmp/${shell}.completion" "$compldir/${pair#*:}"
    fi
  done

  if [ -n "$previous" ] && [ "$previous" != "$version" ]; then
    done_step "Updated pkg ${previous} to ${version}"
  else
    done_step "Installed pkg ${version}"
  fi
}

# ---------------------------------------------------------------------------
# Shell setup. Bash and Zsh get one managed block between two marker lines.
# Fish gets one pkg-owned file. PATH entries are guarded, so repeated shell
# starts never duplicate them. The final order is: pkg client, Nix,
# installed packages, then the rest of PATH.
# ---------------------------------------------------------------------------

# path_lines DIR...: guarded PATH lines. Each line prepends, so the
# directories are written in reverse to keep the given order.
path_lines() {
  reversed=""
  for dir do
    reversed="$(sq "$dir")${reversed:+ $reversed}"
  done
  eval "set -- $reversed"
  for dir do
    qdir="$(sq "$dir")"
    printf 'case ":$PATH:" in\n'
    printf '  *:%s:*) ;;\n' "$qdir"
    printf '  *) export PATH=%s"${PATH:+:$PATH}" ;;\n' "$qdir"
    printf 'esac\n'
  done
}

write_bash_block() {
  printf '%s\n' "$begin_marker"
  printf '%s\n' "# Managed by the pkg installer. Run the installer again to update this block."
  path_lines "$@"
  qfile="$(sq "$compldir/pkg.bash")"
  printf 'if [ -n "${BASH_VERSION:-}" ] && [ -r %s ]; then\n' "$qfile"
  printf '  case $- in *i*) . %s ;; esac\n' "$qfile"
  printf 'fi\n'
  printf '%s\n' "$end_marker"
}

write_zsh_block() {
  printf '%s\n' "$begin_marker"
  printf '%s\n' "# Managed by the pkg installer. Run the installer again to update this block."
  path_lines "$@"
  qdir="$(sq "$compldir")"
  printf 'fpath=(%s ${fpath:#%s})\n' "$qdir" "$qdir"
  printf 'if (( $+functions[compdef] )); then\n'
  printf '  autoload -Uz _pkg && compdef _pkg pkg\n'
  printf 'else\n'
  printf '  autoload -Uz compinit && compinit -i\n'
  printf 'fi\n'
  printf '%s\n' "$end_marker"
}

write_fish_file() {
  printf '%s\n' "$fish_marker"
  printf '%s\n' "# Run the installer again to update this file. pkg self uninstall removes it."
  reversed=""
  for dir do
    reversed="$(fq "$dir")${reversed:+ $reversed}"
  done
  printf 'for dir in %s\n' "$reversed"
  printf '    contains -- $dir $PATH; or set -gx PATH $dir $PATH\n'
  printf 'end\n'
  qdir="$(fq "$compldir")"
  printf 'contains -- %s $fish_complete_path; or set -g fish_complete_path %s $fish_complete_path\n' "$qdir" "$qdir"
}

# upsert_block FILE BLOCK ACTION: make FILE hold exactly one current copy
# of BLOCK. ACTION "add" appends a block when none exists; "refresh" only
# replaces an existing block. Unrelated lines, a missing final newline, and
# the file mode stay intact (sibling copy plus rename). Links, special
# files, and broken marker lines are reported and never rewritten.
upsert_block() {
  up_file="$1"
  up_block="$2"
  up_action="$3"
  if [ ! -e "$up_file" ] && [ ! -L "$up_file" ]; then
    [ "$up_action" = "add" ] || return 0
    if install -m 0644 "$up_block" "$up_file"; then
      return 0
    fi
    printf 'Shell setup: cannot create %s.\n' "$up_file" >&2
    shell_failed=1
    return 0
  fi
  if [ -L "$up_file" ] || [ ! -f "$up_file" ]; then
    [ "$up_action" = "add" ] || return 0
    say "Shell setup skipped $up_file: it is a link or a special file."
    shell_manual=1
    return 0
  fi
  up_verdict="$(awk -v b="$begin_marker" -v e="$end_marker" '
    $0 == b { nb++; if (!fb) fb = NR }
    $0 == e { ne++; if (!fe) fe = NR }
    END {
      if (nb == 0 && ne == 0) print "APPEND"
      else if (nb == 1 && ne == 1 && fb < fe) print "REPLACE"
      else print "MALFORMED"
    }
  ' "$up_file" || echo MALFORMED)"
  case "$up_verdict" in
    APPEND)
      [ "$up_action" = "add" ] || return 0
      {
        cat "$up_file"
        if [ -s "$up_file" ] && [ -n "$(tail -c 1 "$up_file")" ]; then
          printf '\n'
        fi
        if [ -s "$up_file" ]; then
          printf '\n'
        fi
        cat "$up_block"
      } > "$tmp/pkg-new-content"
      ;;
    REPLACE)
      awk -v b="$begin_marker" -v e="$end_marker" '
        $0 == b { on = 1 }
        on { print }
        $0 == e && on { exit }
      ' "$up_file" > "$tmp/pkg-current-block"
      if cmp -s "$tmp/pkg-current-block" "$up_block"; then
        return 0
      fi
      # The block path is read through ARGV in BEGIN: awk applies escape
      # processing to -v values only, so a TMPDIR with a backslash keeps
      # its path. Lines after the block keep their exact bytes, also a
      # last line without a final newline.
      up_lastnl=1
      if [ -n "$(tail -c 1 "$up_file")" ]; then
        up_lastnl=0
      fi
      awk -v b="$begin_marker" -v e="$end_marker" -v lastnl="$up_lastnl" '
        BEGIN {
          new = ARGV[2]
          ARGV[2] = ""
          while ((getline line < new) > 0) blk = blk line ORS
        }
        $0 == b && !done { done = 1; skip = 1; next }
        skip {
          if ($0 == e) { skip = 0 }
          next
        }
        done { post[++np] = $0; next }
        { print }
        END {
          if (!done) exit
          out = blk
          for (i = 1; i <= np; i++)
            out = out post[i] ((i < np || lastnl) ? ORS : "")
          if (np == 0 && !lastnl)
            out = substr(out, 1, length(out) - 1)
          printf "%s", out
        }
      ' "$up_file" "$up_block" > "$tmp/pkg-new-content"
      ;;
    *)
      say "Shell setup skipped $up_file: its pkg marker lines are broken."
      say "  Remove the lines from $begin_marker to $end_marker, then run this installer again."
      shell_manual=1
      return 0
      ;;
  esac
  # The new content must hold exactly one marker pair.
  if [ "$(grep -c -x -F "$begin_marker" "$tmp/pkg-new-content" || true)" != 1 ] \
    || [ "$(grep -c -x -F "$end_marker" "$tmp/pkg-new-content" || true)" != 1 ]; then
    printf 'Shell setup: refusing an unsafe update of %s.\n' "$up_file" >&2
    shell_failed=1
    return 0
  fi
  # A random sibling name cannot follow or replace an unrelated file, and
  # the rename within one directory is atomic.
  pending_new="$(mktemp "${up_file%/*}/pkg-new.XXXXXX")" || {
    pending_new=""
    printf 'Shell setup: cannot update %s safely.\n' "$up_file" >&2
    shell_failed=1
    return 0
  }
  if cp -p "$up_file" "$pending_new" 2>/dev/null \
    && cat "$tmp/pkg-new-content" > "$pending_new" \
    && mv -f "$pending_new" "$up_file"; then
    pending_new=""
    return 0
  fi
  rm -f -- "$pending_new"
  pending_new=""
  printf 'Shell setup: cannot update %s safely.\n' "$up_file" >&2
  shell_failed=1
  return 0
}

# upsert_fish FILE ACTION: write the pkg-owned Fish file. A file without
# the pkg marker belongs to the user and stays unchanged.
upsert_fish() {
  up_file="$1"
  if [ -L "$up_file" ] || { [ -e "$up_file" ] && [ ! -f "$up_file" ]; }; then
    [ "$2" = "add" ] || return 0
    say "Shell setup skipped $up_file: it is a link or a special file."
    shell_manual=1
    return 0
  fi
  if [ -f "$up_file" ]; then
    if [ "$(head -n 1 "$up_file")" != "$fish_marker" ]; then
      [ "$2" = "add" ] || return 0
      say "Shell setup skipped $up_file: the file is not managed by pkg."
      shell_manual=1
      return 0
    fi
    if cmp -s "$up_file" "$tmp/pkg.fish"; then
      return 0
    fi
  elif [ "$2" != "add" ]; then
    return 0
  fi
  if mkdir -p "${up_file%/*}" \
    && pending_new="$(mktemp "${up_file%/*}/pkg-new.XXXXXX")" \
    && cat "$tmp/pkg.fish" > "$pending_new" \
    && chmod 0644 "$pending_new" \
    && mv -f "$pending_new" "$up_file"; then
    pending_new=""
    return 0
  fi
  if [ -n "$pending_new" ]; then
    rm -f -- "$pending_new"
  fi
  pending_new=""
  printf 'Shell setup: cannot write %s.\n' "$up_file" >&2
  shell_failed=1
}

# bash_login_file: the first existing Bash login file, or .profile. A new
# .bash_profile or .bash_login would hide an existing .profile.
bash_login_file() {
  for name in .bash_profile .bash_login .profile; do
    if [ -e "$HOME/$name" ] || [ -L "$HOME/$name" ]; then
      printf '%s\n' "$HOME/$name"
      return 0
    fi
  done
  printf '%s\n' "$HOME/.profile"
}

setup_shells() {
  shell_failed=0
  shell_manual=0
  set --
  for dir in "$bindir" "$nix_dir" "$profile_bin"; do
    [ -n "$dir" ] || continue
    case "$dir" in
      /*) ;;
      *) fail "the directory $dir is not an absolute path." ;;
    esac
    case "$dir" in
      *:* | *"$newline"*)
        say "Shell setup skipped: PATH cannot hold the directory $dir."
        shell_manual=1
        return 0
        ;;
    esac
    set -- "$@" "$dir"
  done
  write_bash_block "$@" > "$tmp/pkg-bash-block"
  write_zsh_block "$@" > "$tmp/pkg-zsh-block"
  write_fish_file "$@" > "$tmp/pkg.fish"

  zsh_file="${zdotdir:-$HOME}/.zshrc"
  fish_file="$config_home/fish/conf.d/pkg.fish"
  bash_add="refresh"
  zsh_add="refresh"
  fish_add="refresh"
  case "${SHELL:-}" in
    */bash | bash) bash_add="add" ;;
    */zsh | zsh) zsh_add="add" ;;
    */fish | fish) fish_add="add" ;;
    *) shell_manual=1 ;;
  esac
  if [ "$zsh_add" = "add" ] && [ -n "$zdotdir" ] && [ ! -d "$zdotdir" ]; then
    # A failure here is reported when the startup file cannot be created.
    mkdir -p "$zdotdir" 2>/dev/null || true
  fi

  # Existing managed setups are refreshed in every shell, so a user who
  # changes shells keeps one current setup everywhere.
  upsert_block "$HOME/.bashrc" "$tmp/pkg-bash-block" "$bash_add"
  login_file="$(bash_login_file)"
  upsert_block "$login_file" "$tmp/pkg-bash-block" "$bash_add"
  for name in .bash_profile .bash_login .profile; do
    if [ "$HOME/$name" != "$login_file" ]; then
      upsert_block "$HOME/$name" "$tmp/pkg-bash-block" refresh
    fi
  done
  upsert_block "$zsh_file" "$tmp/pkg-zsh-block" "$zsh_add"
  upsert_fish "$fish_file" "$fish_add"
}

# ---------------------------------------------------------------------------
# Main.
# ---------------------------------------------------------------------------
main() {
  if [ "$#" -ne 0 ]; then
    printf 'Usage: curl -fsSL https://github.com/%s/releases/latest/download/install.sh | sh\n' "$repo" >&2
    printf '  The installer takes no options.\n' >&2
    exit 2
  fi
  case "${PKG_INSTALL_MODE:-}" in
    "") ;;
    test) start_job=0 ;;
    *) fail "PKG_INSTALL_MODE must be test or unset." ;;
  esac

  case "$(uname -s):$(uname -m)" in
    Linux:x86_64) system="x86_64-linux" ;;
    Darwin:arm64) system="aarch64-darwin" ;;
    *) fail "pkg runs on x86_64 Linux and Apple silicon macOS only." ;;
  esac

  for tool in curl tar gzip awk grep sed cmp tail head mktemp install; do
    need "$tool"
  done
  if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
    fail "pkg needs the sha256sum or shasum command. Install one of them, then run this installer again."
  fi

  case "${HOME:-}" in
    /*) ;;
    *) fail "HOME must be an absolute path. Start the installer from a normal terminal." ;;
  esac
  for variable in XDG_STATE_HOME XDG_CONFIG_HOME ZDOTDIR; do
    value=""
    eval "value=\${$variable:-}"
    case "$value" in
      "" | /*) ;;
      *) fail "$variable must be an absolute path. Fix or unset it, then run this installer again." ;;
    esac
  done

  newline="$(printf '\nx')"
  newline="${newline%x}"
  bindir="$HOME/.local/bin"
  compldir="$HOME/.local/share/pkg/completions"
  state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
  config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
  zdotdir="${ZDOTDIR:-}"
  profile_bin="$state_home/nix/profiles/pkg/bin"

  tmp="$(mktemp -d)"
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  say "Installing pkg ${version}…"

  find_nix
  nix_offered=0
  if [ -z "$nix_bin" ] && [ -z "$nix_rejected" ]; then
    if offer_nix; then
      nix_offered=1
      find_nix
    fi
  fi
  nix_dir=""
  if [ -n "$nix_bin" ]; then
    nix_dir="${nix_bin%/*}"
    if [ "$nix_offered" -eq 1 ]; then
      done_step "Installed Nix"
    fi
  fi

  download_client
  install_client

  setup_shells
  if [ "$shell_failed" -eq 0 ] && [ "$shell_manual" -eq 0 ]; then
    done_step "Set up your shell"
  fi

  if [ -n "$nix_bin" ] && [ "$start_job" -eq 1 ]; then
    if PATH="$nix_dir:$PATH" "$bindir/pkg" background enable > "$tmp/background.out" 2>&1; then
      done_step "Search is getting ready in the background"
    else
      say "Daily updates are off. Search data will be prepared the first time you search."
    fi
  fi

  if [ "$shell_manual" -eq 1 ] || [ "$shell_failed" -eq 1 ]; then
    say ""
    say "Add these directories to PATH in your shell's startup file:"
    for dir in "$bindir" "$nix_dir" "$profile_bin"; do
      if [ -n "$dir" ]; then
        say "  $dir"
      fi
    done
  fi

  say ""
  if [ -z "$nix_bin" ]; then
    if [ -n "$nix_rejected" ]; then
      say "pkg found $nix_rejected, but it does not work. Repair Nix, then run this installer again."
    elif [ "$nix_offered" -eq 1 ]; then
      say "Nix was installed, but pkg cannot find it yet."
      say "Open a new terminal window, then run this installer again."
    else
      say "pkg needs Nix to install packages. Install Nix:"
      say "  $nix_command"
      say "Then open a new terminal window and run: pkg doctor"
    fi
  else
    say "Open a new terminal window to start using pkg."
  fi
  [ "$shell_failed" -eq 0 ] || exit 1
}

main "$@"
