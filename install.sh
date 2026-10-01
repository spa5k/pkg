#!/bin/sh
# pkg client downloader for one exact release.
#
# Downloads and verifies the standalone pkg client archive for this system,
# reads bin/pkg (plus completions) out of the archive, and installs those
# files under ~/.local (or PKG_INSTALL_BIN / PKG_INSTALL_COMPLETIONS).
#
# The downloader also sets up PATH for subsequent shells. It writes one
# small managed block into Bash and Zsh startup files. The block covers
# the pkg client bin, the detected Nix bin, and the pkg package profile
# bin. Set PKG_INSTALL_SHELL_SETUP=0 to skip all startup file changes.
#
# The downloader detects an existing Nix runtime without trusting the
# inherited PATH. The detected Nix bin directory is always written into
# the managed block, because a PATH entry can be a one-time export.
#
# This script NEVER:
#   - installs, updates, configures, or removes Nix or any other runtime;
#   - uses sudo or installs privileged helpers or services;
#   - changes Nix trust settings or accepts new substituters or keys;
#   - removes or migrates old pkg installations;
#   - writes any file outside the install targets and the user's own
#     Bash/Zsh startup files described above.
#
# Nix must be installed separately, by you, from the vendor:
#   https://docs.determinate.systems/determinate-nix/
#
# The managed block and the printed one-time command must contain literal
# $PATH text on purpose.
# shellcheck disable=SC2016
set -eu

# The version, tag, archive name, and checksum file must all agree with the
# release built by tools/release/package_client.sh. package_client.sh takes
# the version from pkg-cli Cargo metadata, and the client-release workflow
# rejects a tag that disagrees with that version.
repo="spa5k/pkg"
version="0.2.0-alpha.10"
tag="v${version}"

if [ "$#" -ne 0 ]; then
  echo "Usage: sh pkg-install.sh" >&2
  echo "  This downloader takes no arguments. It installs release ${tag}." >&2
  exit 2
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) system="x86_64-linux" ;;
  Darwin:arm64) system="aarch64-darwin" ;;
  *)
    echo "error: unsupported system. Supported: x86_64 Linux, Apple silicon macOS." >&2
    exit 1
    ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: missing tool: $1" >&2; exit 1; }; }
need curl
need tar
need install
need awk
need grep
need sed
need cmp
need tail
need mktemp

if [ -z "${HOME:-}" ]; then
  echo "error: HOME is not set; the downloader installs under HOME." >&2
  exit 1
fi
case "$HOME" in
  /*) ;;
  *)
    echo "error: HOME is not an absolute path: $HOME" >&2
    echo "  Start the downloader from a normal login shell." >&2
    exit 1
    ;;
esac

# Every base directory the setup uses must be absolute. A relative value
# cannot give working paths in later shells, and the pkg client rejects a
# relative XDG_STATE_HOME itself, so refuse it here instead of pretending.
require_absolute() {
  if [ "$2" = "" ]; then
    return 0
  fi
  case "$2" in
    /*) ;;
    *)
      echo "error: $1 is not an absolute path: $2" >&2
      echo "  Set $1 to an absolute path, or unset it." >&2
      exit 1
      ;;
  esac
}
require_absolute PKG_INSTALL_BIN "${PKG_INSTALL_BIN:-}"
require_absolute PKG_INSTALL_COMPLETIONS "${PKG_INSTALL_COMPLETIONS:-}"
require_absolute XDG_STATE_HOME "${XDG_STATE_HOME:-}"

# One checksum tool is enough. Check both before use: "need shasum ||
# need sha256sum" cannot express this, because need exits the script when
# the first tool is missing.
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  echo "error: missing tool: shasum or sha256sum" >&2
  exit 1
fi

archive="pkg-${version}-${system}.tar.gz"
root="pkg-${version}-${system}"
base="https://github.com/${repo}/releases/download/${tag}"

tmp="$(mktemp -d)"
pending_new=""
trap 'rm -rf "$tmp"; if [ -n "$pending_new" ]; then rm -f -- "$pending_new"; fi' EXIT

echo "Downloading ${archive} from release ${tag}..."
curl -fsSL -o "$tmp/$archive" "${base}/${archive}"
curl -fsSL -o "$tmp/SHA256SUMS" "${base}/SHA256SUMS"

# Verify the archive against its published checksum.
want="$(grep " ${archive}\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
if [ -z "$want" ]; then
  echo "error: no checksum entry for ${archive} in SHA256SUMS." >&2
  exit 1
fi
if command -v shasum >/dev/null 2>&1; then
  got="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
  got="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
fi
# Both tools mark an escaped filename with a leading backslash on the
# whole line. The temporary path may contain literal backslashes, so
# strip that one marker before the digest comparison.
case "$got" in
  \\*) got="${got#\\}" ;;
esac
if [ "$got" != "$want" ]; then
  echo "error: checksum mismatch for ${archive}." >&2
  echo "  expected $want" >&2
  echo "  got      $got" >&2
  exit 1
fi

# Read only the exact members this downloader installs. Every member's
# bytes go to stdout (`tar -xO`) and into a temporary file named below.
# The archive's directory tree is never extracted to disk, so archive
# paths (absolute, `..`, deep) and links cannot write outside these files.
tar -tvf "$tmp/$archive" > "$tmp/listing" || {
  echo "error: cannot list members of ${archive}." >&2
  exit 1
}

# exact_regular MEMBER: succeed only when MEMBER is stored exactly once,
# as one regular file, with nothing below MEMBER. Duplicate names, mixed
# member types, links, and directories are refused.
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

# read_member MEMBER DEST: write MEMBER's bytes to DEST, or fail clearly.
read_member() {
  tar -xOzf "$tmp/$archive" -- "$1" > "$2" || {
    echo "error: tar failed while reading ${1} from ${archive}." >&2
    exit 1
  }
}

client="${root}/bin/pkg"
if ! exact_regular "$client"; then
  echo "error: ${archive} stores no single regular file ${client}." >&2
  exit 1
fi
read_member "$client" "$tmp/pkg"
if [ ! -s "$tmp/pkg" ]; then
  echo "error: ${client} from ${archive} is empty." >&2
  exit 1
fi

for shell in bash zsh fish; do
  member="${root}/completions/${shell}.txt"
  if exact_regular "$member"; then
    read_member "$member" "$tmp/${shell}.completion"
  fi
done

bindir="${PKG_INSTALL_BIN:-$HOME/.local/bin}"
compldir="${PKG_INSTALL_COMPLETIONS:-$HOME/.local/share/pkg/completions}"
mkdir -p "$bindir" "$compldir"
install -m 0755 "$tmp/pkg" "$bindir/pkg"
for shell in bash zsh fish; do
  if [ -f "$tmp/${shell}.completion" ]; then
    cp "$tmp/${shell}.completion" "$compldir/${shell}.txt"
  fi
done

echo "Installed $bindir/pkg and completions under $compldir."

# ---------------------------------------------------------------------------
# Detect an existing Nix runtime. The inherited PATH is not trusted alone:
# the same stable profile paths the pkg client searches are checked next,
# in the same order. A candidate counts only when it is an executable
# file for this user AND answers `nix --version`. Nothing here installs,
# configures, or repairs Nix.
# ---------------------------------------------------------------------------
state_home="$HOME/.local/state"
if [ -n "${XDG_STATE_HOME:-}" ]; then
  state_home="$XDG_STATE_HOME"
fi
profile_bin="$state_home/nix/profiles/pkg/bin"

nix_bin=""
nix_rejected=""
nix_from_path_candidate="$(command -v nix 2>/dev/null || true)"
for candidate in \
  "$nix_from_path_candidate" \
  "/nix/var/nix/profiles/default/bin/nix" \
  "$state_home/nix/profile/bin/nix" \
  "$HOME/.nix-profile/bin/nix"
do
  [ -n "$candidate" ] || continue
  if [ -f "$candidate" ] && [ -x "$candidate" ] \
    && "$candidate" --version >/dev/null 2>&1; then
    nix_bin="$candidate"
    break
  fi
  if [ -e "$candidate" ] && [ -z "$nix_rejected" ]; then
    nix_rejected="$candidate"
  fi
done

# The Nix bin directory always enters the shell setup, also when Nix was
# found through PATH. A PATH entry can be a one-time export; new shells
# must keep a working Nix without it.
nix_dir=""
if [ -n "$nix_bin" ]; then
  nix_dir="${nix_bin%/*}"
fi

# ---------------------------------------------------------------------------
# Managed shell setup. One block between two version-independent marker
# lines per startup file. Each directory is added to PATH once, guarded,
# single-quoted, and never executed as anything but a PATH assignment.
# Directories travel as positional parameters only, so spaces and shell
# metacharacters stay intact.
# ---------------------------------------------------------------------------
begin_marker="# >>> pkg paths >>>"
end_marker="# <<< pkg paths <<<"
block="$tmp/pkg-paths-block"

# sq VALUE: print VALUE single-quoted for shell text. Spaces, quotes,
# dollars, backticks, and glob characters stay literal; no path becomes
# executed shell text.
sq() {
  printf "'%s'" "$(printf "%s" "$1" | sed "s/'/'\\\\''/g")"
}

# write_block DIR...: print the managed block for the directories given.
write_block() {
  printf '%s\n' "$begin_marker"
  printf '%s\n' "# Managed by the pkg downloader. Run the downloader again to update this block."
  for dir do
    qdir="$(sq "$dir")"
    printf 'case ":$PATH:" in\n'
    printf '  *:%s:*) ;;\n' "$qdir"
    printf '  *) export PATH=%s"${PATH:+:$PATH}" ;;\n' "$qdir"
    printf 'esac\n'
  done
  printf '%s\n' "$end_marker"
}

# Collect the final directory list as positional parameters.
set --
for candidate in "$bindir" "$nix_dir" "$profile_bin"; do
  [ -n "$candidate" ] || continue
  set -- "$@" "$candidate"
done

# A PATH directory cannot contain a colon or a newline: PATH itself
# cannot represent them. Refuse shell setup before any startup file is
# changed; the client stays installed.
nl_char="$(printf '\nx')"
nl_char="${nl_char%x}"
setup_dirs_ok=1
for dir do
  case "$dir" in
    /*) ;;
    *)
      echo "error: directory for the PATH setup is not an absolute path: $dir" >&2
      setup_dirs_ok=0
      ;;
  esac
  case "$dir" in
    *:*)
      echo "error: PATH directory contains a colon and cannot be set up: $dir" >&2
      setup_dirs_ok=0
      ;;
  esac
  case "$dir" in
    *"$nl_char"*)
      echo "error: PATH directory contains a newline and cannot be set up: $dir" >&2
      setup_dirs_ok=0
      ;;
  esac
done

write_block "$@" > "$block"

setup_state="not-run"
file_failed=0
setup_hard_error=0

# upsert_block FILE: make FILE hold exactly one current managed block.
# Unrelated lines, a missing final newline, the file's mode, and its
# directory entry stay intact (sibling copy plus rename, never a
# truncate-in-place). Symlinks, non-regular files, and marker lines in
# an unexpected form are reported and skipped, never rewritten.
upsert_block() {
  up_file="$1"
  if [ -L "$up_file" ]; then
    echo "Shell setup: $up_file is a symlink. The downloader did not change it." >&2
    echo "  Add the pkg PATH block to the file the link points to, or remove the link and run again." >&2
    file_failed=1
    return 0
  fi
  if [ -e "$up_file" ] && [ ! -f "$up_file" ]; then
    echo "Shell setup: $up_file is not a regular file. The downloader did not change it." >&2
    file_failed=1
    return 0
  fi
  if [ ! -e "$up_file" ]; then
    if install -m 0644 "$block" "$up_file"; then
      echo "Shell setup: created $up_file."
      return 0
    fi
    echo "Shell setup: cannot create $up_file." >&2
    file_failed=1
    setup_hard_error=1
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
      {
        cat "$up_file"
        if [ -s "$up_file" ] && [ -n "$(tail -c 1 "$up_file")" ]; then
          printf '\n'
        fi
        if [ -s "$up_file" ]; then
          printf '\n'
        fi
        cat "$block"
      } > "$tmp/pkg-new-content"
      ;;
    REPLACE)
      # With an identical current block, change nothing at all.
      awk -v b="$begin_marker" -v e="$end_marker" '
        $0 == b { on = 1 }
        on { print }
        $0 == e && on { exit }
      ' "$up_file" > "$tmp/pkg-current-block"
      if cmp -s "$tmp/pkg-current-block" "$block"; then
        echo "Shell setup: $up_file already holds the current pkg PATH block."
        return 0
      fi
      # The block file is passed as a file argument and read through
      # ARGV in BEGIN: awk applies backslash escape processing to -v
      # values only, never to file arguments, so a TMPDIR with a literal
      # backslash (for example "tmp\n...") keeps the block path intact.
      # Clear ARGV[2] so awk does not read the block again as input.
      # Lines after the block are buffered and reprinted without a final
      # ORS when the file itself ends without one, so a trailing partial
      # line keeps its exact bytes.
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
      ' "$up_file" "$block" > "$tmp/pkg-new-content"
      ;;
    *)
      echo "Shell setup: $up_file holds pkg marker lines in an unexpected form." >&2
      echo "  The downloader did not change the file and removed nothing from it." >&2
      echo "  Fix or remove the marker lines yourself, then run the downloader again." >&2
      echo "  Marker lines: $begin_marker / $end_marker" >&2
      file_failed=1
      return 0
      ;;
  esac
  # One last guard before writing: the new content must hold exactly one
  # well-formed marker pair, even if something above went wrong.
  if [ "$(grep -c -x -F "$begin_marker" "$tmp/pkg-new-content" || true)" != 1 ] \
    || [ "$(grep -c -x -F "$end_marker" "$tmp/pkg-new-content" || true)" != 1 ]; then
    echo "Shell setup: refusing an unsafe update of $up_file." >&2
    file_failed=1
    setup_hard_error=1
    return 0
  fi
  # Create the pending file with mktemp in the same directory: the
  # name is not predictable, so an unrelated file with a guessed name
  # cannot be overwritten or followed, and the final rename is atomic
  # within one directory. mktemp succeeds before pending_new is set, so
  # the EXIT trap can clean the file up.
  pending_new="$(mktemp "${up_file%/*}/pkg-new.XXXXXX")" || {
    pending_new=""
    echo "Shell setup: cannot update $up_file safely." >&2
    file_failed=1
    setup_hard_error=1
    return 0
  }
  if cp -p "$up_file" "$pending_new" 2>/dev/null \
    && cat "$tmp/pkg-new-content" > "$pending_new" \
    && mv -f "$pending_new" "$up_file"; then
    pending_new=""
    echo "Shell setup: updated $up_file."
    return 0
  fi
  rm -f -- "$pending_new"
  pending_new=""
  echo "Shell setup: cannot update $up_file safely." >&2
  file_failed=1
  setup_hard_error=1
  return 0
}

# bash_login_file: the first existing Bash login file, or .profile when
# none exists. A new .bash_profile or .bash_login would hide an existing
# .profile, so those two are never created.
bash_login_file() {
  for name in .bash_profile .bash_login .profile; do
    if [ -e "$HOME/$name" ] || [ -L "$HOME/$name" ]; then
      printf '%s\n' "$HOME/$name"
      return 0
    fi
  done
  printf '%s\n' "$HOME/.profile"
}

# print_dirs DIR...: the manual instruction line for unsupported cases.
print_dirs() {
  echo "Add these directories to your PATH, in your shell's own syntax:"
  for dir do
    printf '  %s\n' "$dir"
  done
}

shell_name="${SHELL:-}"
shell_name="${shell_name##*/}"

if [ "${PKG_INSTALL_SHELL_SETUP:-1}" = "0" ]; then
  setup_state="skipped"
  echo "Shell setup skipped: PKG_INSTALL_SHELL_SETUP=0 is set."
  echo "No shell startup file was changed."
elif [ "$setup_dirs_ok" -eq 0 ]; then
  setup_state="failed"
  setup_hard_error=1
  echo "Shell setup was not performed. Fix the errors above and run again."
else
  case "$shell_name" in
    bash)
      # Interactive Bash reads .bashrc; login Bash reads the first login
      # file. Both shells must work, so the block goes into both files.
      upsert_block "$HOME/.bashrc"
      upsert_block "$(bash_login_file)"
      if [ "$file_failed" -eq 0 ]; then
        setup_state="done"
      fi
      ;;
    zsh)
      zdotdir="${ZDOTDIR:-}"
      if [ -n "$zdotdir" ]; then
        case "$zdotdir" in
          /*) ;;
          *)
            echo "error: ZDOTDIR is not an absolute path: $zdotdir" >&2
            echo "  Set ZDOTDIR to an absolute path, or unset it." >&2
            setup_state="failed"
            setup_hard_error=1
            ;;
        esac
        if [ "$setup_state" != "failed" ] && [ ! -d "$zdotdir" ]; then
          if mkdir -p "$zdotdir"; then
            echo "Shell setup: created ZDOTDIR $zdotdir."
          else
            echo "Shell setup: cannot create ZDOTDIR $zdotdir." >&2
            setup_state="failed"
            setup_hard_error=1
          fi
        fi
        if [ "$setup_state" != "failed" ]; then
          upsert_block "$zdotdir/.zshrc"
          if [ "$file_failed" -eq 0 ]; then
            setup_state="done"
          fi
        fi
      else
        upsert_block "$HOME/.zshrc"
        if [ "$file_failed" -eq 0 ]; then
          setup_state="done"
        fi
      fi
      ;;
    "")
      setup_state="manual"
      echo "SHELL is empty or not set, so the downloader does not know your shell."
      echo "No shell startup file was changed."
      print_dirs "$@"
      echo "Or run the downloader again from your normal login shell."
      ;;
    *)
      setup_state="manual"
      echo "Automatic shell setup is not available for the shell: $shell_name"
      echo "No shell startup file was changed."
      print_dirs "$@"
      ;;
  esac
fi

# ---------------------------------------------------------------------------
# Report. The downloader cannot change the shell that ran it. Say plainly
# what worked, what did not, and what remains.
# ---------------------------------------------------------------------------
echo ""
echo "Client: $bindir/pkg"
if [ -n "$nix_bin" ]; then
  echo "Detected Nix: $nix_bin"
else
  echo "No usable Nix installation was found."
  if [ -n "$nix_rejected" ]; then
    echo "Found $nix_rejected, but it did not answer 'nix --version'."
    echo "Repair Nix, or fix PATH, then run this downloader again."
  fi
  echo "pkg needs an external Nix runtime. pkg does not install Nix."
  echo "Install Determinate Nix from https://docs.determinate.systems/determinate-nix/"
  echo "Then open a new terminal and run: pkg doctor"
  echo "Until then, use the client by its full path: $bindir/pkg doctor"
fi

echo ""
case "$setup_state" in
  done)
    if [ -n "$nix_dir" ]; then
      echo "Shell setup: complete. New terminals get pkg, nix, and pkg packages on PATH."
    else
      echo "Shell setup: complete. New terminals get pkg and pkg packages on PATH."
    fi
    ;;
  skipped)
    echo "Shell setup: skipped (PKG_INSTALL_SHELL_SETUP=0). New terminals do NOT get pkg on PATH."
    print_dirs "$@"
    ;;
  manual)
    echo "Shell setup: not complete. Add the paths shown above to your shell yourself."
    ;;
  failed | not-run | *)
    echo "Shell setup: not complete. Fix the errors above and run the downloader again."
    ;;
esac

echo ""
echo "This downloader cannot change the shell that ran it."
case "$shell_name" in
  bash | zsh)
    echo "For this terminal only, run:"
    printf 'export PATH='
    first=1
    for dir do
      if [ "$first" -eq 1 ]; then
        printf '%s' "$(sq "$dir")"
        first=0
      else
        printf ':%s' "$(sq "$dir")"
      fi
    done
    printf ':"$PATH"\n'
    ;;
  *)
    echo "For this terminal only, use the directories above in your shell's own PATH syntax."
    ;;
esac
if [ "$setup_state" = "done" ]; then
  echo "New terminals pick the setup up automatically."
fi
echo "Docs: https://github.com/${repo}/blob/${tag}/docs/install.md"

if [ "$setup_hard_error" -eq 1 ]; then
  exit 1
fi
exit 0
