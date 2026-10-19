#!/usr/bin/env python3
"""Offline behavioral checks for the pkg installer, install.sh.

Every test runs the real script with /bin/sh inside a private sandbox:
HOME, TMPDIR, and every XDG path live in a temporary directory, and fake
`curl` and `uname` commands on a private PATH entry serve fixture
archives. The pkg client inside the fixtures is a small shell script. It
answers `--version` and records its arguments, so the default mode, which
runs `pkg background enable`, never reaches launchctl or systemctl, and no
test touches the real HOME.

Every installer runs in a new session without a controlling terminal, so
the Nix question can never wait for input. Only the pseudo-terminal tests
attach a terminal on purpose.

Tests that must not see a Nix installation of the host run the installer
in a private mount namespace (unshare) with a tmpfs over /nix, or directly
on a Linux host that has no /nix at all. When neither is possible, for
example on macOS, these tests skip themselves. They never fake a pass.
"""

from __future__ import annotations

import atexit
import collections
import functools
import hashlib
import io
import os
import pathlib
import pty
import re
import select
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "install.sh"
SCRIPT_TEXT = SCRIPT.read_text(encoding="utf-8")
_VERSION_MATCH = re.search(r'^version="([^"]+)"$', SCRIPT_TEXT, re.MULTILINE)
assert _VERSION_MATCH, "install.sh must pin one release version"
VERSION = _VERSION_MATCH.group(1)

REPO = "spa5k/pkg"
RELEASE_BASE = f"https://github.com/{REPO}/releases/download/v{VERSION}"
SYSTEM = "x86_64-linux"
MEMBER_ROOT = f"pkg-{VERSION}-{SYSTEM}"
CLIENT = f"{MEMBER_ROOT}/bin/pkg"
UNAME = {"x86_64-linux": ("Linux", "x86_64"), "aarch64-darwin": ("Darwin", "arm64")}

USAGE = (
    f"Usage: curl -fsSL https://github.com/{REPO}/releases/latest/download/install.sh | sh\n"
    "  The installer takes no options.\n"
)
NIX_INSTALLER_URL = "https://install.determinate.systems/nix"
NIX_QUESTION = "pkg needs Nix. Install Nix now? [Y/n]"
NIX_GUIDANCE = (
    "pkg needs Nix to install packages. Install Nix:\n"
    f"  curl -fsSL {NIX_INSTALLER_URL} | sh -s -- install\n"
    "Then open a new terminal window and run: pkg doctor\n"
)
BEGIN_MARKER = "# >>> pkg paths >>>"
END_MARKER = "# <<< pkg paths <<<"
FISH_MARKER = "# Managed by the pkg installer."
INSTALLING = f"Installing pkg {VERSION}…\n"
INSTALLED = f"✓ Installed pkg {VERSION}\n"
SHELL_DONE = "✓ Set up your shell\n"
SEARCH_DONE = "✓ Search is getting ready in the background\n"
SEARCH_LATER = "Search data will be prepared the first time you search.\n"
ADD_DIRECTORIES = "Add these directories to PATH in your shell's startup file:\n"
OPEN_TERMINAL = "Open a new terminal window to start using pkg.\n"
TEST_MODE = {"PKG_INSTALL_MODE": "test"}
REMOVED_OPTIONS = (
    "PKG_INSTALL_BIN",
    "PKG_INSTALL_COMPLETIONS",
    "PKG_INSTALL_SHELL_SETUP",
    "PKG_INSTALL_PREPARE",
    "PKG_INSTALL_BACKGROUND",
)

# Archive members: completions/<shell>.txt. Installed names differ per shell.
COMPLETIONS = {
    "bash": "_pkg_test_complete() { COMPREPLY=(alpha-sub beta-sub); }\n"
    "complete -F _pkg_test_complete pkg\n",
    "zsh": "#compdef pkg\ncompadd alpha-sub beta-sub\n",
    "fish": "complete -c pkg -f -a 'alpha-sub beta-sub'\n",
}
INSTALLED_NAMES = {"bash": "pkg.bash", "zsh": "_pkg", "fish": "pkg.fish"}
FAKE_NIX = '#!/bin/sh\necho "nix (Nix) 2.99.0-fake"\n'
BROKEN_NIX = "#!/bin/sh\necho 'nix: broken store' >&2\nexit 3\n"
OLD_BLOCK = (
    f"{BEGIN_MARKER}\n"
    "# An older pkg block.\n"
    "export PATH='/old/pkg/bin'\"${PATH:+:$PATH}\"\n"
    f"{END_MARKER}\n"
)
HOSTILE_NAME = "we'ird \"dq\" $HOME `tick` *?[ \\ dir"

UNSHARE_FLAGS = ["--user", "--map-root-user", "--mount", "--propagation", "private"]
PtyResult = collections.namedtuple("PtyResult", "returncode output asked")


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------


def sq(value: str) -> str:
    """VALUE single-quoted the way the installer quotes for POSIX shells."""
    return "'" + value.replace("'", "'\\''") + "'"


def fq(value: str) -> str:
    """VALUE single-quoted the way the installer quotes for Fish."""
    return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"


def make_executable(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


def fake_client(version: str = VERSION) -> bytes:
    """A pkg client script that records each call in $PKG_TEST_CLIENT_LOG.

    `background enable` also records PATH and the nix it resolves, and
    fails when PKG_TEST_BACKGROUND_FAIL=1. The bytes do not depend on the
    sandbox: macOS checks every new script content once, which is slow.
    """
    return (
        "#!/bin/sh\n"
        'log="${PKG_TEST_CLIENT_LOG:-/dev/null}"\n'
        "printf 'ARGS %s\\n' \"$*\" >> \"$log\"\n"
        'case "$1" in\n'
        f"  --version) echo 'pkg {version}' ;;\n"
        "  background)\n"
        "    printf 'PATH %s\\n' \"$PATH\" >> \"$log\"\n"
        "    printf 'NIX %s\\n' \"$(command -v nix)\" >> \"$log\"\n"
        '    if [ "${PKG_TEST_BACKGROUND_FAIL:-0}" = 1 ]; then\n'
        "      echo 'fake pkg: cannot enable the job' >&2\n"
        "      exit 1\n"
        "    fi\n"
        "    echo 'On: pkg and its search data update daily.' ;;\n"
        "  *) echo pkg-ok ;;\n"
        "esac\n"
    ).encode()


FAKE_CURL = (
    "#!/bin/sh\n"
    "# Serve release fixtures from $PKG_TEST_FIXTURES; refuse other URLs.\n"
    "out=\n"
    "prev=\n"
    "url=\n"
    'for arg in "$@"; do\n'
    '  if [ "$prev" = "-o" ]; then out=$arg; fi\n'
    "  prev=$arg\n"
    "  url=$arg\n"
    "done\n"
    "printf '%s\\n' \"$url\" >> \"$PKG_TEST_CURL_LOG\"\n"
    'case "$url" in\n'
    f"  '{RELEASE_BASE}/pkg-{VERSION}-'*.tar.gz | '{RELEASE_BASE}/SHA256SUMS')\n"
    '    src="$PKG_TEST_FIXTURES/${url##*/}" ;;\n'
    f"  '{NIX_INSTALLER_URL}') src=\"$PKG_TEST_FIXTURES/nix-installer.sh\" ;;\n"
    '  *) echo "fake curl: refused $url" >&2; exit 22 ;;\n'
    "esac\n"
    'if [ ! -f "$src" ]; then echo "fake curl: 404 $url" >&2; exit 22; fi\n'
    'if [ -z "$out" ]; then echo "fake curl: no -o" >&2; exit 2; fi\n'
    'cat "$src" > "$out"\n'
)


def fake_uname(kernel: str, machine: str) -> str:
    return f'#!/bin/sh\ncase "$1" in\n  -m) echo {machine} ;;\n  *) echo {kernel} ;;\nesac\n'


@functools.lru_cache(maxsize=None)
def shared_tools(system: str) -> pathlib.Path:
    """Fake curl and uname for SYSTEM, shared by every sandbox."""
    tools = pathlib.Path(tempfile.mkdtemp(prefix=f"pkg-installer-tools-{system}-"))
    atexit.register(shutil.rmtree, tools, True)
    make_executable(tools / "uname", fake_uname(*UNAME[system]))
    make_executable(tools / "curl", FAKE_CURL)
    return tools


def add_file(tf: tarfile.TarFile, name: str, data: bytes, mode: int = 0o755) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    tf.addfile(info, io.BytesIO(data))


def add_link(tf: tarfile.TarFile, name: str, kind: bytes, target: str) -> None:
    info = tarfile.TarInfo(name)
    info.type = kind
    info.linkname = target
    tf.addfile(info)


def execute(argv, env, *, input_text=None, timeout=120, cwd=None):
    """Run one command in a new session (no controlling terminal)."""
    options = {"input": input_text} if input_text is not None else {"stdin": subprocess.DEVNULL}
    result = subprocess.run(
        [str(part) for part in argv],
        env=env,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        check=False,
        timeout=timeout,
        start_new_session=True,
        cwd=cwd,
        **options,
    )
    result.combined = result.stdout + result.stderr  # type: ignore[attr-defined]
    return result


def marker_pairs(text: str) -> int:
    """Well-formed marker pairs in TEXT, or -1 for a broken layout."""
    pairs = 0
    inside = False
    for line in text.splitlines():
        if line == BEGIN_MARKER and not inside:
            inside = True
        elif line == END_MARKER and inside:
            inside = False
            pairs += 1
        elif line in (BEGIN_MARKER, END_MARKER):
            return -1
    return -1 if inside else pairs


def block_of(text: str) -> str:
    """The managed block in TEXT, marker lines included."""
    lines = text.splitlines(keepends=True)
    start = [line.rstrip("\n") for line in lines].index(BEGIN_MARKER)
    for end in range(start, len(lines)):
        if lines[end].rstrip("\n") == END_MARKER:
            return "".join(lines[start : end + 1])
    raise AssertionError("the managed block has no end marker")


def path_order(text: str) -> list[str]:
    """The PATH order the managed block in TEXT produces.

    Each guarded line prepends one directory, so the last written
    directory ends up first.
    """
    written = []
    for line in block_of(text).splitlines():
        if line.startswith("  *:") and line.endswith(":*) ;;"):
            written.extend(shlex.split(line[len("  *:") : -len(":*) ;;")]))
    return written[::-1]


def fish_words(text: str) -> list[str]:
    """Decode a run of Fish single-quoted words: only \\\\ and \\' escape."""
    words = []
    index = 0
    while index < len(text):
        if text[index].isspace():
            index += 1
            continue
        if text[index] != "'":
            raise AssertionError(f"unquoted Fish word in {text!r}")
        index += 1
        word = []
        while text[index] != "'":
            if text[index] == "\\" and text[index + 1] in "\\'":
                word.append(text[index + 1])
                index += 2
            else:
                word.append(text[index])
                index += 1
        index += 1
        words.append("".join(word))
    return words


def fish_path_order(text: str) -> list[str]:
    for line in text.splitlines():
        if line.startswith("for dir in "):
            return fish_words(line[len("for dir in ") :])[::-1]
    raise AssertionError("the Fish file has no PATH loop")


def fish_completion_dir(text: str) -> str:
    for line in text.splitlines():
        if line.startswith("contains -- ") and "fish_complete_path" in line:
            quoted = line[len("contains -- ") :].split(" $fish_complete_path", 1)[0]
            return fish_words(quoted)[0]
    raise AssertionError("the Fish file does not add a completion directory")


def directory_list(dirs) -> str:
    return ADD_DIRECTORIES + "".join(f"  {d}\n" for d in dirs)


@functools.lru_cache(maxsize=None)
def isolation() -> tuple[str | None, str]:
    """How tests can hide a host Nix: ("unshare" | "absent" | None, reason)."""
    if not sys.platform.startswith("linux"):
        return None, "hiding the host Nix needs Linux mount namespaces"
    for path in ("/usr/bin/nix", "/bin/nix"):
        if os.path.lexists(path):
            return None, f"{path} exists, and every test PATH holds its directory"
    unshare = shutil.which("unshare")
    if unshare:
        probe = "if [ -e /nix ]; then mount -t tmpfs tmpfs /nix || exit 1; fi"
        try:
            result = execute([unshare, *UNSHARE_FLAGS, "/bin/sh", "-c", probe], None, timeout=30)
            if result.returncode == 0:
                return "unshare", "private mount namespace"
        except (OSError, subprocess.SubprocessError):
            pass
    if not os.path.lexists("/nix"):
        return "absent", "this host has no /nix"
    return None, "unshare is unavailable here and the host has /nix"


def run_shell(argv, home: pathlib.Path, extra_env=None):
    """Run one real shell with a minimal environment and the sandbox HOME."""
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "TERM": "dumb"}
    if extra_env:
        env.update(extra_env)
    return execute(argv, env, timeout=60)


def values(output: str, key: str) -> list[str]:
    prefix = f"{key}="
    return [line[len(prefix) :] for line in output.splitlines() if line.startswith(prefix)]


class Sandbox:
    """One disposable install target: HOME, TMPDIR, fake tools, fixtures."""

    def __init__(self, test: unittest.TestCase, *, system=SYSTEM, home_name="home", **release):
        self.path = pathlib.Path(tempfile.mkdtemp(prefix="pkg-installer-test-"))
        test.addCleanup(shutil.rmtree, self.path, True)
        self.system = system
        self.archive = f"pkg-{VERSION}-{system}.tar.gz"
        self.member_root = f"pkg-{VERSION}-{system}"
        self.fixtures = self.path / "fixtures"
        # Per-test overrides of the shared fake tools come first on PATH.
        self.fake_bin = self.path / "fake-bin"
        self.tools = shared_tools(system)
        self.nix_dir = self.path / "nix bin"
        self.home = self.path / home_name
        self.tmp = self.path / "tmp"
        self.client_log = self.path / "client.log"
        self.curl_log = self.path / "curl.log"
        self.nix_installer_log = self.path / "nix-installer.log"
        for directory in (self.fixtures, self.fake_bin, self.home, self.tmp):
            directory.mkdir(parents=True)
        self.client_bytes = fake_client()
        make_executable(self.nix_dir / "nix", FAKE_NIX)
        self.build(**release)

    # -- fixtures -----------------------------------------------------------

    def build(
        self,
        *,
        client_bytes: bytes | None = None,
        omit_client: bool = False,
        completions=("bash", "zsh", "fish"),
        extra_members=(),
        sums_hash: str | None = None,
        sums_name: str | None = None,
        archive_bytes: bytes | None = None,
    ) -> None:
        """Write the release archive and SHA256SUMS fixtures."""
        path = self.fixtures / self.archive
        if archive_bytes is not None:
            path.write_bytes(archive_bytes)
        else:
            with tarfile.open(path, "w:gz") as tf:
                if not omit_client:
                    data = self.client_bytes if client_bytes is None else client_bytes
                    add_file(tf, f"{self.member_root}/bin/pkg", data)
                for shell in completions:
                    add_file(
                        tf,
                        f"{self.member_root}/completions/{shell}.txt",
                        COMPLETIONS[shell].encode(),
                        0o644,
                    )
                for name, kind, value in extra_members:
                    if kind == tarfile.REGTYPE:
                        add_file(tf, name, value)
                    else:
                        add_link(tf, name, kind, value)
        digest = sums_hash or hashlib.sha256(path.read_bytes()).hexdigest()
        (self.fixtures / "SHA256SUMS").write_text(f"{digest}  {sums_name or self.archive}\n")

    def write_nix_installer(self, *, exit_code: int = 0, create_nix: bool = True) -> None:
        """A fake Determinate installer that records its arguments."""
        body = (
            "#!/bin/sh\n"
            "printf 'ARGS %s\\n' \"$*\" >> \"$PKG_TEST_NIX_LOG\"\n"
            "if [ -t 0 ]; then echo 'STDIN tty' >> \"$PKG_TEST_NIX_LOG\"; fi\n"
        )
        if create_nix:
            body += (
                'mkdir -p "$HOME/.nix-profile/bin"\n'
                "cat > \"$HOME/.nix-profile/bin/nix\" <<'EOF'\n"
                f"{FAKE_NIX}"
                "EOF\n"
                'chmod 755 "$HOME/.nix-profile/bin/nix"\n'
            )
        body += f"exit {exit_code}\n"
        (self.fixtures / "nix-installer.sh").write_text(body)

    # -- running ------------------------------------------------------------

    def env(self, *, shell="", extra=None, nix=True, path_prepend=()):
        entries = [*map(str, path_prepend), str(self.fake_bin), str(self.tools)]
        if nix:
            entries.append(str(self.nix_dir))
        entries += ["/usr/bin", "/bin"]
        env = {
            "PATH": ":".join(entries),
            "HOME": str(self.home),
            "TMPDIR": str(self.tmp),
            # An empty SHELL stays empty everywhere. An unset SHELL does
            # not: Bash, /bin/sh on macOS, fills it in from the account.
            "SHELL": shell,
            "PKG_TEST_CLIENT_LOG": str(self.client_log),
            "PKG_TEST_CURL_LOG": str(self.curl_log),
            "PKG_TEST_FIXTURES": str(self.fixtures),
            "PKG_TEST_NIX_LOG": str(self.nix_installer_log),
        }
        if extra:
            env.update(extra)
        return env

    def run(self, *args, stdin_script=False, cwd=None, **env_options):
        """Run the installer as a file, or piped into sh like `curl | sh`."""
        if stdin_script:
            return execute(
                ["/bin/sh"], self.env(**env_options), input_text=SCRIPT_TEXT, cwd=cwd
            )
        return execute(["/bin/sh", SCRIPT, *args], self.env(**env_options), cwd=cwd)

    def isolated_argv(self, inside_setup: str = "") -> list[str]:
        """The installer command line that cannot see a host Nix.

        INSIDE_SETUP runs first, after the host /nix is hidden, so it can
        plant fake system-profile Nix files safely.
        """
        mode, _ = isolation()
        lines = ["#!/bin/sh"]
        if mode == "unshare":
            lines.append("if [ -e /nix ]; then mount -t tmpfs tmpfs /nix || exit 90; fi")
        else:
            lines.append("if [ -e /nix ]; then echo 'isolation: the host has /nix' >&2; exit 91; fi")
        if inside_setup:
            lines.append(f"{{ {inside_setup}; }} || exit 92")
        lines.append(f"exec /bin/sh {shlex.quote(str(SCRIPT))}")
        runner = self.path / "isolated-runner.sh"
        runner.write_text("\n".join(lines) + "\n")
        prefix = [shutil.which("unshare"), *UNSHARE_FLAGS] if mode == "unshare" else []
        return [*prefix, "/bin/sh", str(runner)]

    def run_isolated(self, *, inside_setup="", **env_options):
        env_options.setdefault("nix", False)
        return execute(self.isolated_argv(inside_setup), self.env(**env_options))

    def run_pty(self, answer: str | None, *, inside_setup="", timeout=60, **env_options):
        """Run the isolated installer on a pseudo-terminal.

        When the Nix question appears, ANSWER is typed (`n` when ANSWER is
        None). Output holds stdout, stderr, and the terminal echo.
        """
        env_options.setdefault("nix", False)
        argv = self.isolated_argv(inside_setup)
        env = self.env(**env_options)
        pid, fd = pty.fork()
        if pid == 0:
            try:
                os.execve(argv[0], argv, env)
            finally:
                os._exit(127)
        output = bytearray()
        asked = False
        deadline = time.monotonic() + timeout
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    os.kill(pid, signal.SIGKILL)
                    raise AssertionError(
                        "the installer did not finish on the terminal:\n"
                        + output.decode("utf-8", errors="replace")
                    )
                ready, _, _ = select.select([fd], [], [], min(remaining, 1.0))
                if not ready:
                    continue
                try:
                    data = os.read(fd, 65536)
                except OSError:  # EIO: the terminal closed.
                    break
                if not data:
                    break
                output += data
                if not asked and NIX_QUESTION.encode() in output:
                    asked = True
                    os.write(fd, ("n" if answer is None else answer).encode() + b"\n")
        finally:
            os.close(fd)
            _, status = os.waitpid(pid, 0)
        text = output.decode("utf-8", errors="replace").replace("\r\n", "\n")
        return PtyResult(os.waitstatus_to_exitcode(status), text, asked)

    # -- inspection ---------------------------------------------------------

    @property
    def bindir(self) -> pathlib.Path:
        return self.home / ".local" / "bin"

    @property
    def compldir(self) -> pathlib.Path:
        return self.home / ".local" / "share" / "pkg" / "completions"

    def profile_bin(self, state: pathlib.Path | None = None) -> pathlib.Path:
        return (state or self.home / ".local" / "state") / "nix" / "profiles" / "pkg" / "bin"

    def fish_file(self, config: pathlib.Path | None = None) -> pathlib.Path:
        return (config or self.home / ".config") / "fish" / "conf.d" / "pkg.fish"

    def dirs(self, *, nix: pathlib.Path | str | None = "default", state=None) -> list[str]:
        """The expected PATH order: client, Nix (when found), profile."""
        found = [str(self.bindir)]
        if nix is not None:
            found.append(str(self.nix_dir if nix == "default" else nix))
        found.append(str(self.profile_bin(state)))
        return found

    def log(self, kind: str) -> list[str]:
        if not self.client_log.exists():
            return []
        prefix = f"{kind} "
        return [
            line[len(prefix) :]
            for line in self.client_log.read_text().splitlines()
            if line.startswith(prefix)
        ]

    def background_calls(self) -> list[str]:
        return [args for args in self.log("ARGS") if args.startswith("background")]

    def curl_urls(self) -> list[str]:
        return self.curl_log.read_text().splitlines() if self.curl_log.exists() else []

    def leftovers(self) -> list[str]:
        """Temporary files the installer must not leave behind."""
        found = [str(p) for p in self.tmp.iterdir()]
        found += [
            str(p)
            for p in self.home.rglob("*")
            if p.name.startswith((".pkg-new.", "pkg-new."))
        ]
        return found


class InstallerTestCase(unittest.TestCase):
    maxDiff = None

    def make_sandbox(self, **kwargs) -> Sandbox:
        return Sandbox(self, **kwargs)

    def assert_ok(self, result) -> None:
        self.assertEqual(result.returncode, 0, result.combined)

    def assert_fatal(self, result, sentence: str) -> None:
        self.assertEqual(result.returncode, 1, result.combined)
        # A failing tool (gzip, curl) can print its own line first.
        self.assertIn(f"\nerror: {sentence}\n", "\n" + result.stderr)
        self.assertEqual(result.stderr.count("error: "), 1, result.stderr)

    def assert_clean(self, sandbox: Sandbox) -> None:
        self.assertEqual(sandbox.leftovers(), [])

    def assert_block(self, path: pathlib.Path, dirs: list[str], kind: str = "bash") -> None:
        """The file at PATH holds exactly one current managed block of KIND."""
        text = path.read_text()
        self.assertEqual(marker_pairs(text), 1, text)
        self.assertEqual(path_order(text), dirs, text)
        block = block_of(text)
        if kind == "bash":
            self.assertIn("BASH_VERSION", block)
            self.assertNotIn("fpath", block)
        else:
            self.assertIn("fpath=(", block)
            self.assertNotIn("BASH_VERSION", block)
        self.assertNotIn("/old/pkg/bin", text)


# ---------------------------------------------------------------------------
# Arguments, preflight checks, and the script contract
# ---------------------------------------------------------------------------


class UsageTests(InstallerTestCase):
    def test_any_argument_prints_usage_and_exits_2(self) -> None:
        sandbox = self.make_sandbox()
        for args in (["--help"], ["-y"], [""], ["install", "now"]):
            with self.subTest(args=args):
                result = sandbox.run(*args, shell="/bin/bash")
                self.assertEqual(result.returncode, 2, result.combined)
                self.assertEqual(result.stdout, "")
                self.assertEqual(result.stderr, USAGE)
                self.assertEqual(sandbox.curl_urls(), [])
                self.assertEqual(list(sandbox.home.iterdir()), [])


class PreflightTests(InstallerTestCase):
    def assert_refused_before_network(self, sandbox: Sandbox, result, sentence: str) -> None:
        self.assert_fatal(result, sentence)
        self.assertEqual(result.stdout, "")
        self.assertEqual(sandbox.curl_urls(), [])
        self.assertEqual(sandbox.log("ARGS"), [])
        if sandbox.home.is_dir():
            self.assertEqual(list(sandbox.home.iterdir()), [])
        self.assertEqual(list(sandbox.tmp.iterdir()), [])

    def test_unsupported_system_fails_before_network(self) -> None:
        for kernel, machine in (("SunOS", "i86pc"), ("Linux", "aarch64"), ("Darwin", "x86_64")):
            with self.subTest(system=f"{kernel} {machine}"):
                sandbox = self.make_sandbox()
                make_executable(sandbox.fake_bin / "uname", fake_uname(kernel, machine))
                result = sandbox.run(shell="/bin/bash")
                self.assert_refused_before_network(
                    sandbox, result, "pkg runs on x86_64 Linux and Apple silicon macOS only."
                )

    def tool_stubs(self, sandbox: Sandbox, names) -> pathlib.Path:
        tools = sandbox.path / "only-tools"
        for name in names:
            make_executable(tools / name, "#!/bin/sh\nexit 99\n")
        shutil.copy2(sandbox.tools / "uname", tools / "uname")
        shutil.copy2(sandbox.tools / "curl", tools / "curl")
        return tools

    def test_missing_checksum_tool_is_a_clean_error(self) -> None:
        sandbox = self.make_sandbox()
        tools = self.tool_stubs(
            sandbox,
            ("tar", "gzip", "awk", "grep", "sed", "cmp", "tail", "head", "mktemp", "install"),
        )
        env = sandbox.env(shell="/bin/bash")
        env["PATH"] = str(tools)
        result = execute(["/bin/sh", SCRIPT], env)
        self.assert_refused_before_network(
            sandbox,
            result,
            "pkg needs the sha256sum or shasum command. "
            "Install one of them, then run this installer again.",
        )

    def test_missing_required_tool_is_named(self) -> None:
        sandbox = self.make_sandbox()
        tools = self.tool_stubs(
            sandbox,
            ("tar", "gzip", "awk", "grep", "sed", "tail", "head", "mktemp", "install", "shasum"),
        )
        env = sandbox.env(shell="/bin/bash")
        env["PATH"] = str(tools)
        result = execute(["/bin/sh", SCRIPT], env)
        self.assert_refused_before_network(
            sandbox, result, "pkg needs the cmp command. Install cmp, then run this installer again."
        )

    def test_invalid_install_mode_fails_before_network(self) -> None:
        for value in ("1", "yes", "Test", "prepare"):
            with self.subTest(value=value):
                sandbox = self.make_sandbox()
                result = sandbox.run(shell="/bin/bash", extra={"PKG_INSTALL_MODE": value})
                self.assert_refused_before_network(
                    sandbox, result, "PKG_INSTALL_MODE must be test or unset."
                )

    def test_home_must_be_absolute(self) -> None:
        for home in ("relative/home", ""):
            with self.subTest(home=home):
                sandbox = self.make_sandbox()
                result = sandbox.run(shell="/bin/bash", extra={"HOME": home})
                self.assert_refused_before_network(
                    sandbox,
                    result,
                    "HOME must be an absolute path. Start the installer from a normal terminal.",
                )

    def test_relative_xdg_and_zdotdir_rejected_before_download(self) -> None:
        for variable in ("XDG_STATE_HOME", "XDG_CONFIG_HOME", "ZDOTDIR"):
            with self.subTest(variable=variable):
                sandbox = self.make_sandbox()
                result = sandbox.run(shell="/bin/zsh", extra={variable: "relative/dir"})
                self.assert_refused_before_network(
                    sandbox,
                    result,
                    f"{variable} must be an absolute path. "
                    "Fix or unset it, then run this installer again.",
                )


class ScriptContractTests(unittest.TestCase):
    @staticmethod
    def code() -> str:
        return "\n".join(
            line for line in SCRIPT_TEXT.splitlines() if not line.lstrip().startswith("#")
        )

    def test_pins_one_release(self) -> None:
        self.assertIn(f'repo="{REPO}"', SCRIPT_TEXT)
        self.assertIn('tag="v${version}"', SCRIPT_TEXT)
        self.assertIn('archive="pkg-${version}-${system}.tar.gz"', SCRIPT_TEXT)
        self.assertIn('base="https://github.com/${repo}/releases/download/${tag}"', SCRIPT_TEXT)

    def test_reads_exact_members_without_extracting(self) -> None:
        code = self.code()
        self.assertIn('tar -xOf "$tmp/archive.tar" -- "$1"', code)
        for command in re.findall(r"\btar -x\w*", code):
            self.assertIn("O", command)
        self.assertNotRegex(code, r"\btar\b[^\n]*\s-C\s")

    def test_never_uses_sudo(self) -> None:
        self.assertNotRegex(self.code(), r"\bsudo\b")

    def test_markers_are_version_independent(self) -> None:
        self.assertIn(f'begin_marker="{BEGIN_MARKER}"', SCRIPT_TEXT)
        self.assertIn(f'end_marker="{END_MARKER}"', SCRIPT_TEXT)
        self.assertIn(f'fish_marker="{FISH_MARKER}"', SCRIPT_TEXT)

    def test_removed_options_are_gone(self) -> None:
        for name in REMOVED_OPTIONS:
            self.assertNotIn(name, SCRIPT_TEXT)


# ---------------------------------------------------------------------------
# Release archive handling
# ---------------------------------------------------------------------------


class ArchiveTests(InstallerTestCase):
    def install(self, sandbox: Sandbox):
        return sandbox.run(extra=TEST_MODE)

    def assert_refused(self, sandbox: Sandbox, result, sentence: str) -> None:
        self.assert_fatal(result, sentence)
        self.assertEqual(result.stdout, INSTALLING)
        self.assertFalse((sandbox.home / ".local").exists())
        self.assertEqual(sandbox.log("ARGS"), [])
        self.assertFalse(list(sandbox.path.rglob(sandbox.member_root)))
        self.assert_clean(sandbox)

    def test_installs_client_and_completions_under_new_names(self) -> None:
        sandbox = self.make_sandbox()
        result = self.install(sandbox)
        self.assert_ok(result)
        pkg = sandbox.bindir / "pkg"
        self.assertEqual(pkg.read_bytes(), sandbox.client_bytes)
        self.assertEqual(stat.S_IMODE(pkg.stat().st_mode), 0o755)
        self.assertEqual(sorted(p.name for p in sandbox.bindir.iterdir()), ["pkg"])
        self.assertEqual(
            {p.name: p.read_text() for p in sandbox.compldir.iterdir()},
            {INSTALLED_NAMES[shell]: text for shell, text in COMPLETIONS.items()},
        )
        self.assertEqual(
            sandbox.curl_urls(),
            [f"{RELEASE_BASE}/{sandbox.archive}", f"{RELEASE_BASE}/SHA256SUMS"],
        )
        # The archive tree is never extracted.
        self.assertFalse(list(sandbox.path.rglob(sandbox.member_root)))
        self.assert_clean(sandbox)

    def test_old_txt_completions_are_removed(self) -> None:
        sandbox = self.make_sandbox()
        sandbox.compldir.mkdir(parents=True)
        for shell in ("bash", "zsh", "fish"):
            (sandbox.compldir / f"{shell}.txt").write_text("old\n")
        (sandbox.compldir / "notes").write_text("keep\n")
        self.assert_ok(self.install(sandbox))
        self.assertEqual(
            sorted(p.name for p in sandbox.compldir.iterdir()),
            sorted(["_pkg", "notes", "pkg.bash", "pkg.fish"]),
        )
        self.assertEqual((sandbox.compldir / "notes").read_text(), "keep\n")

    def test_missing_or_linked_completion_is_skipped(self) -> None:
        sandbox = self.make_sandbox(
            completions=("bash",),
            extra_members=((f"{MEMBER_ROOT}/completions/zsh.txt", tarfile.SYMTYPE, "/etc/passwd"),),
        )
        self.assert_ok(self.install(sandbox))
        self.assertEqual(sorted(p.name for p in sandbox.compldir.iterdir()), ["pkg.bash"])

    def test_checksum_mismatch_installs_nothing(self) -> None:
        sandbox = self.make_sandbox(sums_hash="0" * 64)
        result = self.install(sandbox)
        self.assert_refused(
            sandbox,
            result,
            "the pkg download does not match its published checksum. Nothing was installed.",
        )
        self.assertIn("\n  Run this installer again.\n", result.stderr)

    def test_missing_checksum_entry_installs_nothing(self) -> None:
        sandbox = self.make_sandbox(sums_name="pkg-other.tar.gz")
        self.assert_refused(
            sandbox, self.install(sandbox), f"the release has no checksum for {sandbox.archive}."
        )

    def test_failed_download_installs_nothing(self) -> None:
        sandbox = self.make_sandbox()
        (sandbox.fixtures / sandbox.archive).unlink()
        result = self.install(sandbox)
        self.assert_refused(sandbox, result, f"pkg {VERSION} could not be downloaded.")
        self.assertIn("  Check your internet connection", result.stderr)

    def test_damaged_archive_installs_nothing(self) -> None:
        sandbox = self.make_sandbox(archive_bytes=b"not a gzip archive\n")
        self.assert_refused(
            sandbox, self.install(sandbox), "the pkg download is damaged. Nothing was installed."
        )

    def test_empty_client_is_refused(self) -> None:
        sandbox = self.make_sandbox(client_bytes=b"")
        self.assert_refused(
            sandbox,
            self.install(sandbox),
            "the pkg download has an empty client file. Nothing was installed.",
        )

    def test_client_that_is_not_one_regular_file_is_refused(self) -> None:
        cases = {
            "missing": dict(omit_client=True),
            "symlink": dict(
                omit_client=True, extra_members=((CLIENT, tarfile.SYMTYPE, "/etc/passwd"),)
            ),
            "hard link": dict(
                omit_client=True,
                extra_members=((CLIENT, tarfile.LNKTYPE, f"{MEMBER_ROOT}/completions/bash.txt"),),
            ),
            "duplicate regular": dict(extra_members=((CLIENT, tarfile.REGTYPE, b"evil\n"),)),
            "duplicate symlink": dict(
                extra_members=((CLIENT, tarfile.SYMTYPE, "/etc/passwd"),)
            ),
            "member below it": dict(
                extra_members=((f"{CLIENT}/inner", tarfile.REGTYPE, b"evil\n"),)
            ),
        }
        for name, release in cases.items():
            with self.subTest(case=name):
                sandbox = self.make_sandbox(**release)
                self.assert_refused(
                    sandbox,
                    self.install(sandbox),
                    "the pkg download has no single client file. Nothing was installed.",
                )

    def test_traversal_members_write_nothing_outside(self) -> None:
        sandbox = self.make_sandbox(
            extra_members=(
                ("../../outside.txt", tarfile.REGTYPE, b"evil\n"),
                (f"{MEMBER_ROOT}/../../inside-outside.txt", tarfile.REGTYPE, b"evil\n"),
                ("/pkg-test-absolute.txt", tarfile.REGTYPE, b"evil\n"),
            )
        )
        self.assert_ok(self.install(sandbox))
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertFalse(list(sandbox.path.parent.glob("outside.txt")))
        self.assertFalse(list(sandbox.path.rglob("outside.txt")))
        self.assertFalse(list(sandbox.path.rglob("inside-outside.txt")))
        self.assertFalse(pathlib.Path("/pkg-test-absolute.txt").exists())
        self.assertFalse(list(sandbox.path.rglob(sandbox.member_root)))
        self.assert_clean(sandbox)

    def test_parent_symlink_writes_nothing_through_it(self) -> None:
        sandbox = self.make_sandbox()
        sandbox.build(
            omit_client=True,
            extra_members=(
                (f"{MEMBER_ROOT}/bin", tarfile.SYMTYPE, "evil-target"),
                (CLIENT, tarfile.REGTYPE, sandbox.client_bytes),
            ),
        )
        self.assert_ok(self.install(sandbox))
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertFalse(list(sandbox.path.rglob("evil-target")))
        self.assertFalse(list(sandbox.path.rglob(sandbox.member_root)))
        self.assert_clean(sandbox)


# ---------------------------------------------------------------------------
# Client installation and the background step
# ---------------------------------------------------------------------------


class ClientInstallTests(InstallerTestCase):
    def place_old_client(self, sandbox: Sandbox, data: bytes, mode: int = 0o755) -> None:
        sandbox.bindir.mkdir(parents=True)
        old = sandbox.bindir / "pkg"
        old.write_bytes(data)
        old.chmod(mode)

    def test_first_install_prints_a_short_summary(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run(shell="/bin/bash")
        self.assert_ok(result)
        self.assertEqual(
            result.stdout,
            INSTALLING + INSTALLED + SHELL_DONE + SEARCH_DONE + "\n" + OPEN_TERMINAL,
        )
        self.assertEqual(result.stderr, "")
        self.assert_clean(sandbox)

    def test_piped_into_sh_installs(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run(stdin_script=True, shell="/bin/bash")
        self.assert_ok(result)
        self.assertEqual(
            result.stdout,
            INSTALLING + INSTALLED + SHELL_DONE + SEARCH_DONE + "\n" + OPEN_TERMINAL,
        )
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs())

    def test_installs_on_apple_silicon(self) -> None:
        sandbox = self.make_sandbox(system="aarch64-darwin")
        result = sandbox.run(extra=TEST_MODE)
        self.assert_ok(result)
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertIn(f"{RELEASE_BASE}/pkg-{VERSION}-aarch64-darwin.tar.gz", sandbox.curl_urls())

    def test_update_line_names_the_old_and_new_version(self) -> None:
        sandbox = self.make_sandbox()
        self.place_old_client(sandbox, fake_client("0.1.0"))
        result = sandbox.run(extra=TEST_MODE)
        self.assert_ok(result)
        self.assertIn(f"✓ Updated pkg 0.1.0 to {VERSION}\n", result.stdout)
        self.assertNotIn("✓ Installed pkg", result.stdout)
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertEqual(sorted(p.name for p in sandbox.bindir.iterdir()), ["pkg"])
        self.assert_clean(sandbox)

    def test_same_or_unusable_previous_client_says_installed(self) -> None:
        cases = {
            "same version": (lambda s: fake_client(VERSION), 0o755),
            "not executable": (lambda s: fake_client("0.1.0"), 0o644),
            "fails": (lambda s: b"#!/bin/sh\nexit 1\n", 0o755),
        }
        for name, (data, mode) in cases.items():
            with self.subTest(case=name):
                sandbox = self.make_sandbox()
                self.place_old_client(sandbox, data(sandbox), mode)
                result = sandbox.run(extra=TEST_MODE)
                self.assert_ok(result)
                self.assertIn(INSTALLED, result.stdout)
                self.assertNotIn("Updated", result.stdout)
                pkg = sandbox.bindir / "pkg"
                self.assertEqual(pkg.read_bytes(), sandbox.client_bytes)
                self.assertEqual(stat.S_IMODE(pkg.stat().st_mode), 0o755)

    def test_client_that_cannot_run_installs_nothing(self) -> None:
        sandbox = self.make_sandbox()
        detail = "/lib/x86_64-linux-gnu/libc.so.6: version GLIBC_2.35 not found (required by pkg)"
        sandbox.build(client_bytes=f"#!/bin/sh\necho {shlex.quote(detail)} >&2\nexit 1\n".encode())
        old = fake_client("0.1.0")
        self.place_old_client(sandbox, old)
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_text("alias ll='ls -l'\n")
        result = sandbox.run(shell="/bin/bash")
        self.assertEqual(result.returncode, 1, result.combined)
        self.assertEqual(
            result.stderr,
            "error: pkg cannot run on this computer. pkg needs glibc 2.35 or newer.\n"
            f"  {detail}\n",
        )
        self.assertEqual(result.stdout, INSTALLING)
        # The old client stays, and nothing else changed.
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), old)
        self.assertEqual(sorted(p.name for p in sandbox.bindir.iterdir()), ["pkg"])
        self.assertEqual(list(sandbox.compldir.iterdir()), [])
        self.assertEqual(bashrc.read_text(), "alias ll='ls -l'\n")
        self.assertEqual(sorted(p.name for p in sandbox.home.iterdir()), [".bashrc", ".local"])
        self.assertEqual(sandbox.background_calls(), [])
        self.assert_clean(sandbox)

    def test_client_that_cannot_run_on_macos_does_not_mention_glibc(self) -> None:
        sandbox = self.make_sandbox(system="aarch64-darwin")
        sandbox.build(client_bytes=b"#!/bin/sh\necho 'Bad CPU type in executable' >&2\nexit 86\n")
        result = sandbox.run(shell="/bin/zsh")
        self.assertEqual(result.returncode, 1, result.combined)
        self.assertEqual(
            result.stderr,
            "error: pkg cannot run on this computer.\n  Bad CPU type in executable\n",
        )
        self.assertFalse((sandbox.bindir / "pkg").exists())
        self.assertEqual(list(sandbox.bindir.iterdir()), [])
        self.assertFalse((sandbox.home / ".zshrc").exists())
        self.assert_clean(sandbox)

    def test_removed_options_have_no_effect(self) -> None:
        sandbox = self.make_sandbox()
        elsewhere = sandbox.path / "elsewhere"
        result = sandbox.run(
            shell="/bin/bash",
            extra={
                "PKG_INSTALL_BIN": str(elsewhere / "bin"),
                "PKG_INSTALL_COMPLETIONS": str(elsewhere / "completions"),
                "PKG_INSTALL_SHELL_SETUP": "0",
                "PKG_INSTALL_PREPARE": "0",
                "PKG_INSTALL_BACKGROUND": "0",
            },
        )
        self.assert_ok(result)
        self.assertFalse(elsewhere.exists())
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertTrue((sandbox.compldir / "pkg.bash").is_file())
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs())
        self.assertEqual(sandbox.background_calls(), ["background enable"])


class BackgroundStepTests(InstallerTestCase):
    def test_default_mode_enables_the_job_once_with_nix_first_on_path(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run()
        self.assert_ok(result)
        self.assertIn(SEARCH_DONE, result.stdout)
        self.assertEqual(sandbox.background_calls(), ["background enable"])
        self.assertEqual(sandbox.log("PATH"), [f"{sandbox.nix_dir}:{sandbox.env()['PATH']}"])
        self.assertEqual(sandbox.log("NIX"), [str(sandbox.nix_dir / "nix")])

    def test_failed_background_step_still_succeeds(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run(shell="/bin/bash", extra={"PKG_TEST_BACKGROUND_FAIL": "1"})
        self.assert_ok(result)
        self.assertEqual(
            result.stdout,
            INSTALLING + INSTALLED + SHELL_DONE + SEARCH_LATER + "\n" + OPEN_TERMINAL,
        )
        self.assertEqual(result.stderr, "")
        self.assertEqual(sandbox.background_calls(), ["background enable"])
        self.assert_clean(sandbox)

    def test_test_mode_skips_the_job(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run(shell="/bin/bash", extra=TEST_MODE)
        self.assert_ok(result)
        self.assertEqual(result.stdout, INSTALLING + INSTALLED + SHELL_DONE + "\n" + OPEN_TERMINAL)
        self.assertEqual(sandbox.log("ARGS"), ["--version"])

    def test_empty_install_mode_is_the_default_mode(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run(extra={"PKG_INSTALL_MODE": ""})
        self.assert_ok(result)
        self.assertEqual(sandbox.background_calls(), ["background enable"])


class NixOnPathTests(InstallerTestCase):
    """Nix found through PATH. The host Nix cannot win here."""

    def test_nix_in_a_relative_path_entry_is_pinned_to_the_directory(self) -> None:
        # dash answers `command -v nix` with a relative path here. The
        # installer, like pkg, pins it to the current directory.
        sandbox = self.make_sandbox()
        make_executable(sandbox.path / "relative-nix" / "nix", FAKE_NIX)
        result = sandbox.run(
            shell="/bin/bash", nix=False, path_prepend=("relative-nix",), cwd=sandbox.path
        )
        self.assert_ok(result)
        self.assertIn(SHELL_DONE + SEARCH_DONE, result.stdout)
        expected = os.path.realpath(sandbox.path / "relative-nix")
        [client, nix_dir, profile] = path_order((sandbox.home / ".bashrc").read_text())
        self.assertEqual([client, profile], [str(sandbox.bindir), str(sandbox.profile_bin())])
        self.assertEqual(os.path.realpath(nix_dir), expected)
        [nix] = sandbox.log("NIX")
        self.assertEqual(os.path.realpath(nix), os.path.join(expected, "nix"))


# ---------------------------------------------------------------------------
# Shell setup
# ---------------------------------------------------------------------------


class ShellSetupTests(InstallerTestCase):
    def install(self, sandbox: Sandbox, shell: str, extra=None):
        return sandbox.run(shell=shell, extra={**TEST_MODE, **(extra or {})})

    # -- Bash ----------------------------------------------------------------

    def test_bash_first_install_writes_bashrc_and_profile(self) -> None:
        sandbox = self.make_sandbox()
        result = self.install(sandbox, "/bin/bash")
        self.assert_ok(result)
        self.assertIn(SHELL_DONE, result.stdout)
        self.assertNotIn(ADD_DIRECTORIES, result.stdout)
        home = sandbox.home
        bashrc, profile = home / ".bashrc", home / ".profile"
        for path in (bashrc, profile):
            self.assert_block(path, sandbox.dirs())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o644)
        text = bashrc.read_text()
        self.assertEqual(profile.read_text(), text)
        self.assertTrue(text.startswith(BEGIN_MARKER + "\n"))
        self.assertTrue(text.endswith(END_MARKER + "\n"))
        completion = sq(str(sandbox.compldir / "pkg.bash"))
        self.assertIn(
            f'if [ -n "${{BASH_VERSION:-}}" ] && [ -r {completion} ]; then\n'
            f"  case $- in *i*) . {completion} ;; esac\n"
            "fi\n",
            text,
        )
        for name in (".bash_profile", ".bash_login", ".zshrc", ".config"):
            self.assertFalse((home / name).exists(), name)
        self.assert_clean(sandbox)

    def test_bash_login_file_choice(self) -> None:
        cases = [
            ({".bash_profile": "export EDITOR=vi\n", ".profile": "keep me\n"}, ".bash_profile"),
            ({".bash_login": "# custom\n", ".profile": "keep me\n"}, ".bash_login"),
            ({".profile": "P=1\n"}, ".profile"),
            ({}, ".profile"),
        ]
        for files, chosen in cases:
            with self.subTest(files=sorted(files)):
                sandbox = self.make_sandbox()
                for name, text in files.items():
                    (sandbox.home / name).write_text(text)
                self.assert_ok(self.install(sandbox, "/bin/bash"))
                login = sandbox.home / chosen
                self.assert_block(login, sandbox.dirs())
                self.assertTrue(login.read_text().startswith(files.get(chosen, "")))
                for name in (".bash_profile", ".bash_login", ".profile"):
                    if name == chosen:
                        continue
                    path = sandbox.home / name
                    if name in files:
                        self.assertEqual(path.read_text(), files[name])
                    else:
                        self.assertFalse(path.exists(), name)

    def test_old_block_in_another_login_file_is_refreshed(self) -> None:
        sandbox = self.make_sandbox()
        (sandbox.home / ".bash_profile").write_text("plain\n")
        (sandbox.home / ".profile").write_text("keep\n" + OLD_BLOCK)
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        self.assert_block(sandbox.home / ".bash_profile", sandbox.dirs())
        profile = sandbox.home / ".profile"
        self.assert_block(profile, sandbox.dirs())
        self.assertTrue(profile.read_text().startswith("keep\n" + BEGIN_MARKER + "\n"))
        self.assertFalse((sandbox.home / ".bash_login").exists())

    def test_rerun_is_idempotent(self) -> None:
        sandbox = self.make_sandbox()
        (sandbox.home / ".bashrc").write_text("alias ll='ls -l'\n")
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        files = [sandbox.home / ".bashrc", sandbox.home / ".profile"]
        before = {p: (p.read_bytes(), p.stat().st_ino) for p in files}
        second = self.install(sandbox, "/bin/bash")
        self.assert_ok(second)
        self.assertIn(SHELL_DONE, second.stdout)
        for path in files:
            self.assertEqual((path.read_bytes(), path.stat().st_ino), before[path], path)
            self.assertEqual(marker_pairs(path.read_text()), 1)
        self.assert_clean(sandbox)

    def test_rerun_replaces_only_the_old_block(self) -> None:
        sandbox = self.make_sandbox()
        before = "alias ll='ls -l'\n# user line before\n"
        after = "export AFTER=1\n# user line after\n"
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_text(before + OLD_BLOCK + after)
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        text = bashrc.read_text()
        self.assertTrue(text.startswith(before + BEGIN_MARKER + "\n"), text)
        self.assertTrue(text.endswith(END_MARKER + "\n" + after), text)
        self.assert_block(bashrc, sandbox.dirs())

    def test_backslash_in_tmpdir_still_replaces_the_block(self) -> None:
        # awk applies escapes to -v values, so the block file must reach
        # awk as a file argument. A literal backslash-n proves it does.
        sandbox = self.make_sandbox()
        back_tmp = sandbox.path / "tmp\\nweird"
        back_tmp.mkdir()
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_text("alias ll='ls -l'\n" + OLD_BLOCK)
        result = self.install(sandbox, "/bin/bash", {"TMPDIR": str(back_tmp)})
        self.assert_ok(result)
        self.assertTrue(bashrc.read_text().startswith("alias ll='ls -l'\n" + BEGIN_MARKER))
        self.assert_block(bashrc, sandbox.dirs())
        self.assertEqual(list(back_tmp.iterdir()), [])

    def test_trailing_line_without_final_newline_is_kept(self) -> None:
        sandbox = self.make_sandbox()
        zshrc = sandbox.home / ".zshrc"
        zshrc.write_bytes(b"setopt autocd\n" + OLD_BLOCK.encode() + b"export USER_SENTINEL=ok")
        self.assert_ok(self.install(sandbox, "/bin/zsh"))
        data = zshrc.read_bytes()
        self.assertTrue(data.startswith(b"setopt autocd\n" + BEGIN_MARKER.encode()), data)
        self.assertTrue(data.endswith(END_MARKER.encode() + b"\nexport USER_SENTINEL=ok"), data)
        self.assert_block(zshrc, sandbox.dirs(), "zsh")

    def test_block_at_end_without_final_newline_keeps_that_ending(self) -> None:
        sandbox = self.make_sandbox()
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_text("x=1\n" + OLD_BLOCK.rstrip("\n"))
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        text = bashrc.read_text()
        self.assertTrue(text.startswith("x=1\n" + BEGIN_MARKER + "\n"), text)
        self.assertTrue(text.endswith("\n" + END_MARKER), text)
        self.assert_block(bashrc, sandbox.dirs())

    def test_append_keeps_unrelated_content_and_mode(self) -> None:
        sandbox = self.make_sandbox()
        original = b"alias ll='ls -l'\nexport A='x y'\nno newline at end"
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_bytes(original)
        bashrc.chmod(0o600)
        profile = sandbox.home / ".profile"
        profile.write_bytes(b"P=1\n")
        profile.chmod(0o640)
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        self.assertEqual(stat.S_IMODE(bashrc.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(profile.stat().st_mode), 0o640)
        block = block_of(bashrc.read_text()).encode()
        self.assertEqual(bashrc.read_bytes(), original + b"\n\n" + block)
        self.assertEqual(profile.read_bytes(), b"P=1\n\n" + block)
        self.assert_block(bashrc, sandbox.dirs())
        self.assert_clean(sandbox)

    def test_update_uses_a_random_sibling_and_leaves_no_leftover(self) -> None:
        sandbox = self.make_sandbox()
        bashrc = sandbox.home / ".bashrc"
        bashrc.write_text("keep\n" + OLD_BLOCK)
        bashrc.chmod(0o600)
        decoy = sandbox.home / ".bashrc.pkg-new.999999"
        decoy.write_text("decoy\n")
        self.assert_ok(self.install(sandbox, "/bin/bash"))
        self.assert_block(bashrc, sandbox.dirs())
        self.assertEqual(decoy.read_text(), "decoy\n")
        self.assertEqual(stat.S_IMODE(bashrc.stat().st_mode), 0o600)
        self.assert_clean(sandbox)

    # -- files that are not changed ------------------------------------------

    def test_link_or_special_startup_file_is_skipped(self) -> None:
        with self.subTest(case="symlink .bashrc"):
            sandbox = self.make_sandbox()
            target = sandbox.path / "real-bashrc"
            target.write_text("# mine\n")
            bashrc = sandbox.home / ".bashrc"
            bashrc.symlink_to(target)
            result = self.install(sandbox, "/bin/bash")
            self.assert_ok(result)
            self.assertTrue(bashrc.is_symlink())
            self.assertEqual(target.read_text(), "# mine\n")
            self.assertIn(
                f"Shell setup skipped {bashrc}: it is a link or a special file.\n", result.stdout
            )
            self.assertNotIn(SHELL_DONE, result.stdout)
            self.assertIn(directory_list(sandbox.dirs()), result.stdout)
            # The independent login file still gets its block.
            self.assert_block(sandbox.home / ".profile", sandbox.dirs())
        with self.subTest(case="directory .zshrc"):
            sandbox = self.make_sandbox()
            zdotdir = sandbox.path / "zdot"
            (zdotdir / ".zshrc").mkdir(parents=True)
            result = self.install(sandbox, "/bin/zsh", {"ZDOTDIR": str(zdotdir)})
            self.assert_ok(result)
            self.assertTrue((zdotdir / ".zshrc").is_dir())
            self.assertIn(
                f"Shell setup skipped {zdotdir / '.zshrc'}: it is a link or a special file.\n",
                result.stdout,
            )
            self.assertIn(directory_list(sandbox.dirs()), result.stdout)

    def test_broken_markers_leave_the_file_untouched(self) -> None:
        for content in (
            f"{BEGIN_MARKER}\nuser line\n{BEGIN_MARKER}\n{END_MARKER}\n",
            f"{BEGIN_MARKER}\n{END_MARKER}\nuser line\n{BEGIN_MARKER}\n{END_MARKER}\n",
            f"{END_MARKER}\nuser line\n{BEGIN_MARKER}\n",
            f"user line\n{BEGIN_MARKER}\nno end\n",
        ):
            with self.subTest(content=content):
                sandbox = self.make_sandbox()
                bashrc = sandbox.home / ".bashrc"
                bashrc.write_text(content)
                bashrc.chmod(0o600)
                result = self.install(sandbox, "/bin/bash")
                self.assert_ok(result)
                self.assertEqual(bashrc.read_text(), content)
                self.assertEqual(stat.S_IMODE(bashrc.stat().st_mode), 0o600)
                self.assertIn(
                    f"Shell setup skipped {bashrc}: its pkg marker lines are broken.\n"
                    f"  Remove the lines from {BEGIN_MARKER} to {END_MARKER}, "
                    "then run this installer again.\n",
                    result.stdout,
                )
                self.assertNotIn(SHELL_DONE, result.stdout)
                self.assertIn(directory_list(sandbox.dirs()), result.stdout)
                self.assert_block(sandbox.home / ".profile", sandbox.dirs())

    def test_unsupported_or_empty_shell_writes_no_startup_file(self) -> None:
        for shell in ("/bin/ksh", "/usr/local/bin/tcsh", "/bin/dash", "sh", ""):
            with self.subTest(shell=shell):
                sandbox = self.make_sandbox()
                result = self.install(sandbox, shell)
                self.assert_ok(result)
                self.assertEqual(
                    result.stdout,
                    INSTALLING
                    + INSTALLED
                    + "\n"
                    + directory_list(sandbox.dirs())
                    + "\n"
                    + OPEN_TERMINAL,
                )
                self.assertEqual(result.stderr, "")
                self.assertEqual([p.name for p in sandbox.home.iterdir()], [".local"])

    def test_existing_setups_are_refreshed_for_any_shell(self) -> None:
        sandbox = self.make_sandbox()
        home = sandbox.home
        (home / ".bashrc").write_text("b=1\n" + OLD_BLOCK)
        (home / ".zshrc").write_text("z=1\n" + OLD_BLOCK)
        fish = sandbox.fish_file()
        fish.parent.mkdir(parents=True)
        fish.write_text(FISH_MARKER + "\n# stale\n")
        result = self.install(sandbox, "/bin/ksh")
        self.assert_ok(result)
        self.assert_block(home / ".bashrc", sandbox.dirs())
        self.assert_block(home / ".zshrc", sandbox.dirs(), "zsh")
        self.assertEqual(fish_path_order(fish.read_text()), sandbox.dirs())
        self.assertFalse((home / ".profile").exists())
        self.assertIn(directory_list(sandbox.dirs()), result.stdout)

    def test_existing_zsh_block_is_refreshed_when_shell_is_bash(self) -> None:
        sandbox = self.make_sandbox()
        zshrc = sandbox.home / ".zshrc"
        zshrc.write_text("setopt autocd\n" + OLD_BLOCK + "alias z=zoxide\n")
        result = self.install(sandbox, "/bin/bash")
        self.assert_ok(result)
        self.assertIn(SHELL_DONE, result.stdout)
        text = zshrc.read_text()
        self.assertTrue(text.startswith("setopt autocd\n" + BEGIN_MARKER), text)
        self.assertTrue(text.endswith(END_MARKER + "\nalias z=zoxide\n"), text)
        self.assert_block(zshrc, sandbox.dirs(), "zsh")
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs())

    def test_existing_fish_file_is_refreshed_when_shell_is_zsh(self) -> None:
        sandbox = self.make_sandbox()
        fish = sandbox.fish_file()
        fish.parent.mkdir(parents=True)
        fish.write_text(FISH_MARKER + "\n# stale\n")
        self.assert_ok(self.install(sandbox, "/bin/zsh"))
        text = fish.read_text()
        self.assertNotIn("# stale", text)
        self.assertEqual(fish_path_order(text), sandbox.dirs())

    def test_files_without_a_block_stay_untouched_for_other_shells(self) -> None:
        with self.subTest(shell="bash"):
            sandbox = self.make_sandbox()
            (sandbox.home / ".zshrc").write_text("setopt autocd\n")
            self.assert_ok(self.install(sandbox, "/bin/bash"))
            self.assertEqual((sandbox.home / ".zshrc").read_text(), "setopt autocd\n")
            self.assertFalse((sandbox.home / ".config").exists())
        with self.subTest(shell="zsh"):
            sandbox = self.make_sandbox()
            (sandbox.home / ".bashrc").write_text("plain\n")
            self.assert_ok(self.install(sandbox, "/bin/zsh"))
            self.assertEqual((sandbox.home / ".bashrc").read_text(), "plain\n")
            for name in (".profile", ".bash_profile", ".bash_login", ".config"):
                self.assertFalse((sandbox.home / name).exists(), name)

    def test_checksum_failure_makes_no_startup_edits(self) -> None:
        sandbox = self.make_sandbox(sums_hash="0" * 64)
        (sandbox.home / ".bashrc").write_text("keep\n")
        result = self.install(sandbox, "/bin/bash")
        self.assertEqual(result.returncode, 1, result.combined)
        self.assertEqual((sandbox.home / ".bashrc").read_text(), "keep\n")
        self.assertEqual([p.name for p in sandbox.home.iterdir()], [".bashrc"])

    # -- paths -----------------------------------------------------------------

    def test_home_with_spaces_quotes_and_dollar_signs(self) -> None:
        sandbox = self.make_sandbox(home_name=HOSTILE_NAME)
        for shell in ("/bin/bash", "/bin/zsh", "/usr/bin/fish"):
            self.assert_ok(self.install(sandbox, shell))
        dirs = sandbox.dirs()
        self.assert_block(sandbox.home / ".bashrc", dirs)
        self.assert_block(sandbox.home / ".profile", dirs)
        self.assert_block(sandbox.home / ".zshrc", dirs, "zsh")
        fish = sandbox.fish_file().read_text()
        self.assertEqual(fish_path_order(fish), dirs)
        self.assertEqual(fish_completion_dir(fish), str(sandbox.compldir))
        # A POSIX shell keeps every directory literal, in order, once.
        script = '. "$1"; . "$1"; printf "%s\\n" "$PATH"'
        result = run_shell(["/bin/sh", "-c", script, "sh", sandbox.home / ".profile"], sandbox.home)
        self.assertEqual(result.stdout.rstrip("\n").split(":"), [*dirs, "/usr/bin", "/bin"])

    def test_colon_or_newline_in_a_directory_skips_shell_setup(self) -> None:
        with self.subTest(case="colon in HOME"):
            sandbox = self.make_sandbox(home_name="bad:home")
            result = self.install(sandbox, "/bin/bash")
            self.assert_ok(result)
            self.assertIn(
                f"Shell setup skipped: PATH cannot hold the directory {sandbox.bindir}.\n",
                result.stdout,
            )
            self.assertNotIn(SHELL_DONE, result.stdout)
            self.assertIn(directory_list(sandbox.dirs()), result.stdout)
            self.assertTrue((sandbox.bindir / "pkg").is_file())
            self.assertEqual([p.name for p in sandbox.home.iterdir()], [".local"])
        with self.subTest(case="newline in XDG_STATE_HOME"):
            sandbox = self.make_sandbox()
            state = sandbox.path / "state\nline"
            result = self.install(sandbox, "/bin/bash", {"XDG_STATE_HOME": str(state)})
            self.assert_ok(result)
            self.assertIn(
                f"Shell setup skipped: PATH cannot hold the directory {sandbox.profile_bin(state)}.\n",
                result.stdout,
            )
            self.assertIn(directory_list(sandbox.dirs(state=state)), result.stdout)
            self.assertEqual([p.name for p in sandbox.home.iterdir()], [".local"])

    def test_xdg_state_home_moves_the_profile_bin(self) -> None:
        sandbox = self.make_sandbox()
        state = sandbox.path / "xdg state"
        self.assert_ok(self.install(sandbox, "/bin/bash", {"XDG_STATE_HOME": str(state)}))
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(state=state))
        self.assertNotIn(".local/state", (sandbox.home / ".bashrc").read_text())

    # -- Zsh -------------------------------------------------------------------

    def test_zsh_block_in_home_zshrc(self) -> None:
        sandbox = self.make_sandbox()
        result = self.install(sandbox, "/bin/zsh")
        self.assert_ok(result)
        self.assertIn(SHELL_DONE, result.stdout)
        zshrc = sandbox.home / ".zshrc"
        self.assert_block(zshrc, sandbox.dirs(), "zsh")
        self.assertEqual(stat.S_IMODE(zshrc.stat().st_mode), 0o644)
        completions = sq(str(sandbox.compldir))
        self.assertIn(
            f"fpath=({completions} ${{fpath:#{completions}}})\n"
            "if (( $+functions[compdef] )); then\n"
            "  autoload -Uz _pkg && compdef _pkg pkg\n"
            "else\n"
            "  autoload -Uz compinit && compinit -i\n"
            "fi\n"
            f"{END_MARKER}\n",
            zshrc.read_text(),
        )
        self.assertEqual(sorted(p.name for p in sandbox.home.iterdir()), [".local", ".zshrc"])

    def test_zdotdir_is_created_and_used(self) -> None:
        sandbox = self.make_sandbox()
        zdotdir = sandbox.path / "fresh zdot"
        self.assert_ok(self.install(sandbox, "/bin/zsh", {"ZDOTDIR": str(zdotdir)}))
        self.assertTrue(zdotdir.is_dir())
        self.assert_block(zdotdir / ".zshrc", sandbox.dirs(), "zsh")
        self.assertEqual([p.name for p in sandbox.home.iterdir()], [".local"])

    # -- Fish ------------------------------------------------------------------

    def test_fish_file_is_written(self) -> None:
        for config in (None, "xdg config"):
            with self.subTest(XDG_CONFIG_HOME=config):
                sandbox = self.make_sandbox()
                extra = {}
                config_home = None
                if config:
                    config_home = sandbox.path / config
                    extra["XDG_CONFIG_HOME"] = str(config_home)
                result = self.install(sandbox, "/usr/bin/fish", extra)
                self.assert_ok(result)
                self.assertIn(SHELL_DONE, result.stdout)
                fish = sandbox.fish_file(config_home)
                text = fish.read_text()
                self.assertEqual(text.splitlines()[0], FISH_MARKER)
                self.assertEqual(stat.S_IMODE(fish.stat().st_mode), 0o644)
                self.assertIn("    contains -- $dir $PATH; or set -gx PATH $dir $PATH\n", text)
                self.assertEqual(fish_path_order(text), sandbox.dirs())
                self.assertEqual(fish_completion_dir(text), str(sandbox.compldir))
                for name in (".bashrc", ".profile", ".zshrc"):
                    self.assertFalse((sandbox.home / name).exists(), name)
                inode = fish.stat().st_ino
                self.assert_ok(self.install(sandbox, "/usr/bin/fish", extra))
                self.assertEqual(fish.stat().st_ino, inode)
                self.assertEqual(fish.read_text(), text)
                self.assert_clean(sandbox)

    def test_fish_file_of_the_user_is_untouched(self) -> None:
        sandbox = self.make_sandbox()
        fish = sandbox.fish_file()
        fish.parent.mkdir(parents=True)
        fish.write_text("set -gx FOO bar\n")
        result = self.install(sandbox, "/usr/bin/fish")
        self.assert_ok(result)
        self.assertEqual(fish.read_text(), "set -gx FOO bar\n")
        self.assertIn(
            f"Shell setup skipped {fish}: the file is not managed by pkg.\n", result.stdout
        )
        self.assertNotIn(SHELL_DONE, result.stdout)
        self.assertIn(directory_list(sandbox.dirs()), result.stdout)

    def test_fish_link_is_skipped(self) -> None:
        sandbox = self.make_sandbox()
        target = sandbox.path / "real.fish"
        target.write_text(FISH_MARKER + "\n# mine\n")
        fish = sandbox.fish_file()
        fish.parent.mkdir(parents=True)
        fish.symlink_to(target)
        result = self.install(sandbox, "/usr/bin/fish")
        self.assert_ok(result)
        self.assertTrue(fish.is_symlink())
        self.assertEqual(target.read_text(), FISH_MARKER + "\n# mine\n")
        self.assertIn(f"Shell setup skipped {fish}: it is a link or a special file.\n", result.stdout)
        self.assertIn(directory_list(sandbox.dirs()), result.stdout)

    def test_unwritable_startup_location_fails_after_the_client(self) -> None:
        sandbox = self.make_sandbox()
        (sandbox.home / ".config").write_text("a file, not a directory\n")
        result = self.install(sandbox, "/usr/bin/fish")
        self.assertEqual(result.returncode, 1, result.combined)
        self.assertIn(f"Shell setup: cannot write {sandbox.fish_file()}.\n", result.stderr)
        self.assertNotIn(SHELL_DONE, result.stdout)
        self.assertIn(directory_list(sandbox.dirs()), result.stdout)
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertEqual((sandbox.home / ".config").read_text(), "a file, not a directory\n")
        self.assert_clean(sandbox)

    def test_fish_quoting_escapes_backslash_and_quote(self) -> None:
        sandbox = self.make_sandbox()
        state = sandbox.path / "st'ate\\dir"
        self.assert_ok(self.install(sandbox, "/usr/bin/fish", {"XDG_STATE_HOME": str(state)}))
        text = sandbox.fish_file().read_text()
        profile_bin = str(sandbox.profile_bin(state))
        self.assertIn(fq(profile_bin), text)
        self.assertIn("st\\'ate\\\\dir", text)
        self.assertEqual(fish_path_order(text), sandbox.dirs(state=state))


# ---------------------------------------------------------------------------
# New shells started after the installer
# ---------------------------------------------------------------------------


class RealShellTests(InstallerTestCase):
    def require(self, name: str) -> str:
        path = shutil.which(name)
        if not path:
            self.skipTest(f"{name} is not installed")
        return path

    def assert_path_prefix(self, output: str, dirs: list[str]) -> None:
        [path] = values(output, "PATH")
        entries = path.split(":")
        self.assertEqual(entries[: len(dirs)], dirs, path)
        for directory in dirs:
            self.assertEqual(entries.count(directory), 1, path)

    def check_bash(self, sandbox: Sandbox) -> None:
        bash = self.require("bash")
        result = run_shell(
            [
                bash,
                "-ic",
                'printf "PATH=%s\\n" "$PATH"; printf "WHICH=%s\\n" "$(command -v pkg)"; '
                'printf "COMP=%s\\n" "$(complete -p pkg)"; printf "RUN=%s\\n" "$(pkg)"',
            ],
            sandbox.home,
        )
        self.assert_path_prefix(result.stdout, sandbox.dirs())
        self.assertEqual(values(result.stdout, "WHICH"), [str(sandbox.bindir / "pkg")], result.combined)
        self.assertEqual(
            values(result.stdout, "COMP"), ["complete -F _pkg_test_complete pkg"], result.combined
        )
        self.assertEqual(values(result.stdout, "RUN"), ["pkg-ok"])

    def check_zsh(self, sandbox: Sandbox, extra_env=None) -> None:
        zsh = self.require("zsh")
        result = run_shell(
            [
                zsh,
                "-ic",
                'print -r -- "PATH=$PATH"; print -r -- "WHICH=$(whence -p pkg)"; '
                'print -r -- "COMP=${_comps[pkg]}"; print -r -- "FUNC=$(whence -w _pkg)"; '
                'print -r -- "RUN=$(pkg)"',
            ],
            sandbox.home,
            extra_env,
        )
        self.assert_path_prefix(result.stdout, sandbox.dirs())
        self.assertEqual(values(result.stdout, "WHICH"), [str(sandbox.bindir / "pkg")], result.combined)
        self.assertEqual(values(result.stdout, "COMP"), ["_pkg"], result.combined)
        # The completion function must be loadable, not only registered.
        self.assertEqual(values(result.stdout, "FUNC"), ["_pkg: function"], result.combined)
        self.assertEqual(values(result.stdout, "RUN"), ["pkg-ok"])

    def check_fish(self, sandbox: Sandbox) -> None:
        fish = self.require("fish")
        result = run_shell(
            [
                fish,
                "-c",
                'printf "PATH=%s\\n" (string join : $PATH); '
                'printf "WHICH=%s\\n" (command -s pkg); '
                'printf "COMP=%s\\n" (complete -C "pkg ")',
            ],
            sandbox.home,
        )
        [path] = values(result.stdout, "PATH")
        entries = path.split(":")
        positions = [entries.index(d) for d in sandbox.dirs()]
        self.assertEqual(positions, sorted(positions), path)
        self.assertLess(positions[-1], entries.index("/usr/bin"), path)
        self.assertEqual(values(result.stdout, "WHICH"), [str(sandbox.bindir / "pkg")], result.combined)
        self.assertEqual(
            sorted(c.split("\t")[0] for c in values(result.stdout, "COMP")),
            ["alpha-sub", "beta-sub"],
            result.combined,
        )

    def test_posix_sh_reads_the_profile_block_in_order(self) -> None:
        sandbox = self.make_sandbox()
        self.assert_ok(sandbox.run(shell="/bin/bash", extra=TEST_MODE))
        script = '. "$1"; . "$1"; printf "%s\\n" "$PATH"'
        result = run_shell(["/bin/sh", "-c", script, "sh", sandbox.home / ".profile"], sandbox.home)
        self.assertEqual(
            result.stdout.rstrip("\n").split(":"), [*sandbox.dirs(), "/usr/bin", "/bin"]
        )

    def test_new_interactive_bash(self) -> None:
        sandbox = self.make_sandbox()
        self.assert_ok(sandbox.run(shell="/bin/bash", extra=TEST_MODE))
        self.check_bash(sandbox)

    def test_new_login_bash(self) -> None:
        bash = self.require("bash")
        sandbox = self.make_sandbox()
        self.assert_ok(sandbox.run(shell="/bin/bash", extra=TEST_MODE))
        result = run_shell(
            [bash, "-lc", 'printf "PATH=%s\\n" "$PATH"; printf "WHICH=%s\\n" "$(command -v pkg)"'],
            sandbox.home,
        )
        self.assert_path_prefix(result.stdout, sandbox.dirs())
        self.assertEqual(values(result.stdout, "WHICH"), [str(sandbox.bindir / "pkg")])

    def test_new_interactive_zsh(self) -> None:
        sandbox = self.make_sandbox()
        self.assert_ok(sandbox.run(shell="/bin/zsh", extra=TEST_MODE))
        self.check_zsh(sandbox)

    def test_new_zsh_after_the_users_own_compinit(self) -> None:
        sandbox = self.make_sandbox()
        (sandbox.home / ".zshrc").write_text("autoload -Uz compinit && compinit -i\n")
        self.assert_ok(sandbox.run(shell="/bin/zsh", extra=TEST_MODE))
        self.check_zsh(sandbox)

    def test_new_zsh_with_zdotdir(self) -> None:
        sandbox = self.make_sandbox()
        zdotdir = sandbox.path / "zdot"
        self.assert_ok(
            sandbox.run(shell="/bin/zsh", extra={**TEST_MODE, "ZDOTDIR": str(zdotdir)})
        )
        self.check_zsh(sandbox, {"ZDOTDIR": str(zdotdir)})

    def test_new_fish(self) -> None:
        sandbox = self.make_sandbox()
        self.assert_ok(sandbox.run(shell="/usr/bin/fish", extra=TEST_MODE))
        self.check_fish(sandbox)

    def test_hostile_home_in_new_bash(self) -> None:
        sandbox = self.make_sandbox(home_name=HOSTILE_NAME)
        self.assert_ok(sandbox.run(shell="/bin/bash", extra=TEST_MODE))
        self.check_bash(sandbox)

    def test_hostile_home_in_new_zsh(self) -> None:
        sandbox = self.make_sandbox(home_name=HOSTILE_NAME)
        self.assert_ok(sandbox.run(shell="/bin/zsh", extra=TEST_MODE))
        self.check_zsh(sandbox)

    def test_hostile_home_in_new_fish(self) -> None:
        sandbox = self.make_sandbox(home_name=HOSTILE_NAME)
        self.assert_ok(sandbox.run(shell="/usr/bin/fish", extra=TEST_MODE))
        self.check_fish(sandbox)


# ---------------------------------------------------------------------------
# Nix detection and the Nix question, without a host Nix in view
# ---------------------------------------------------------------------------


class IsolatedTestCase(InstallerTestCase):
    def setUp(self) -> None:
        mode, reason = isolation()
        if mode is None:
            self.skipTest(f"cannot hide a host Nix: {reason}")

    def require_nix_mount(self) -> None:
        if isolation()[0] != "unshare" or not os.path.isdir("/nix"):
            self.skipTest("planting /nix/var/nix/profiles needs a private tmpfs over /nix")

    def plant(self, directory: pathlib.Path, text: str = FAKE_NIX, mode: int = 0o755) -> pathlib.Path:
        nix = directory / "nix"
        make_executable(nix, text)
        nix.chmod(mode)
        return nix

    def assert_uses_nix(self, sandbox: Sandbox, result, nix: pathlib.Path | str, *, shell=True) -> None:
        self.assert_ok(result)
        nix_dir = str(pathlib.PurePath(nix).parent)
        self.assertNotIn("does not work", result.stdout)
        self.assertNotIn(NIX_GUIDANCE, result.stdout)
        self.assertTrue(result.stdout.endswith(OPEN_TERMINAL), result.combined)
        self.assertEqual(sandbox.background_calls(), ["background enable"])
        self.assertEqual(sandbox.log("NIX"), [str(nix)])
        [path] = sandbox.log("PATH")
        self.assertTrue(path.startswith(nix_dir + ":"), path)
        if shell:
            self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(nix=nix_dir))


class NixDetectionTests(IsolatedTestCase):
    def test_no_nix_without_a_terminal_installs_only_the_client(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run_isolated(shell="/bin/bash")
        self.assert_ok(result)
        self.assertEqual(result.stdout, INSTALLING + INSTALLED + SHELL_DONE + "\n" + NIX_GUIDANCE)
        self.assertEqual(result.stderr, "")
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertNotIn(NIX_INSTALLER_URL, sandbox.curl_urls())
        self.assertEqual(sandbox.background_calls(), [])
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(nix=None))
        self.assert_clean(sandbox)

    def test_nix_on_path_comes_first(self) -> None:
        sandbox = self.make_sandbox()
        self.plant(sandbox.home / ".nix-profile" / "bin")
        result = sandbox.run_isolated(shell="/bin/bash", nix=True)
        self.assert_uses_nix(sandbox, result, sandbox.nix_dir / "nix")

    def test_system_profile_is_found_outside_path(self) -> None:
        self.require_nix_mount()
        sandbox = self.make_sandbox()
        system = "/nix/var/nix/profiles/default/bin"
        result = sandbox.run_isolated(
            shell="/bin/bash",
            inside_setup=f"mkdir -p {system} && cp {shlex.quote(str(sandbox.nix_dir / 'nix'))} {system}/nix",
        )
        self.assert_uses_nix(sandbox, result, f"{system}/nix")

    def test_state_profile_is_preferred_over_the_legacy_profile(self) -> None:
        sandbox = self.make_sandbox()
        modern = self.plant(sandbox.home / ".local" / "state" / "nix" / "profile" / "bin")
        self.plant(sandbox.home / ".nix-profile" / "bin")
        result = sandbox.run_isolated(shell="/bin/bash")
        self.assert_uses_nix(sandbox, result, modern)

    def test_xdg_state_home_profile_is_used(self) -> None:
        sandbox = self.make_sandbox()
        state = sandbox.path / "xdg-state"
        candidate = self.plant(state / "nix" / "profile" / "bin")
        self.plant(sandbox.home / ".local" / "state" / "nix" / "profile" / "bin")
        result = sandbox.run_isolated(shell="/bin/bash", extra={"XDG_STATE_HOME": str(state)})
        self.assert_uses_nix(sandbox, result, candidate, shell=False)
        self.assert_block(
            sandbox.home / ".bashrc", sandbox.dirs(nix=candidate.parent, state=state)
        )

    def test_legacy_profile_is_used_when_the_others_are_absent(self) -> None:
        sandbox = self.make_sandbox()
        legacy = self.plant(sandbox.home / ".nix-profile" / "bin")
        result = sandbox.run_isolated(shell="/bin/bash")
        self.assert_uses_nix(sandbox, result, legacy)

    def test_broken_path_candidate_falls_back_to_a_working_profile(self) -> None:
        sandbox = self.make_sandbox()
        broken_dir = sandbox.path / "broken-nix"
        self.plant(broken_dir, BROKEN_NIX)
        legacy = self.plant(sandbox.home / ".nix-profile" / "bin")
        result = sandbox.run_isolated(shell="/bin/bash", path_prepend=(broken_dir,))
        self.assert_uses_nix(sandbox, result, legacy)

    def test_candidate_that_does_not_work_is_reported(self) -> None:
        cases = {"fails --version": (BROKEN_NIX, 0o755), "not executable": (FAKE_NIX, 0o644)}
        for name, (text, mode) in cases.items():
            with self.subTest(case=name):
                sandbox = self.make_sandbox()
                nix = self.plant(sandbox.home / ".nix-profile" / "bin", text, mode)
                result = sandbox.run_isolated(shell="/bin/bash")
                self.assert_ok(result)
                self.assertEqual(
                    result.stdout,
                    INSTALLING
                    + INSTALLED
                    + SHELL_DONE
                    + "\n"
                    + f"pkg found {nix}, but it does not work. "
                    "Repair Nix, then run this installer again.\n",
                )
                self.assertEqual(sandbox.background_calls(), [])
                self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(nix=None))


class NixQuestionTests(IsolatedTestCase):
    """The Nix question on a real pseudo-terminal."""

    def assert_only_client(self, sandbox: Sandbox, result) -> None:
        self.assertEqual(result.returncode, 0, result.output)
        self.assertTrue(result.output.endswith("\n" + NIX_GUIDANCE), result.output)
        self.assertNotIn("✓ Installed Nix", result.output)
        self.assertEqual((sandbox.bindir / "pkg").read_bytes(), sandbox.client_bytes)
        self.assertEqual(sandbox.background_calls(), [])
        self.assertFalse((sandbox.home / ".nix-profile").exists())
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(nix=None))
        self.assert_clean(sandbox)

    def test_declining_installs_only_the_client(self) -> None:
        sandbox = self.make_sandbox()
        sandbox.write_nix_installer()
        result = sandbox.run_pty("n", shell="/bin/bash")
        self.assertTrue(result.asked, result.output)
        self.assert_only_client(sandbox, result)
        self.assertNotIn(NIX_INSTALLER_URL, sandbox.curl_urls())
        self.assertFalse(sandbox.nix_installer_log.exists())

    def accept(self, answer: str) -> None:
        sandbox = self.make_sandbox()
        sandbox.write_nix_installer()
        result = sandbox.run_pty(answer, shell="/bin/bash")
        self.assertTrue(result.asked, result.output)
        self.assertEqual(result.returncode, 0, result.output)
        self.assertEqual(
            sandbox.nix_installer_log.read_text().splitlines(),
            ["ARGS install --no-confirm", "STDIN tty"],
        )
        self.assertIn(NIX_INSTALLER_URL, sandbox.curl_urls())
        output = result.output
        steps = [
            INSTALLING,
            NIX_QUESTION,
            "Installing Nix with the Determinate Nix installer. It asks for your password.\n",
            "✓ Installed Nix\n",
            INSTALLED,
            SHELL_DONE,
            SEARCH_DONE,
            "\n" + OPEN_TERMINAL,
        ]
        positions = [output.find(step) for step in steps]
        self.assertNotIn(-1, positions, output)
        self.assertEqual(positions, sorted(positions), output)
        self.assertNotIn(NIX_GUIDANCE, output)
        nix = sandbox.home / ".nix-profile" / "bin" / "nix"
        self.assert_block(sandbox.home / ".bashrc", sandbox.dirs(nix=nix.parent))
        self.assertEqual(sandbox.log("NIX"), [str(nix)])
        self.assertEqual(sandbox.background_calls(), ["background enable"])
        self.assert_clean(sandbox)

    def test_accepting_runs_the_determinate_installer(self) -> None:
        self.accept("y")

    def test_empty_answer_means_yes(self) -> None:
        self.accept("")

    def test_failed_nix_installer_keeps_the_client(self) -> None:
        sandbox = self.make_sandbox()
        sandbox.write_nix_installer(exit_code=1, create_nix=False)
        result = sandbox.run_pty("y", shell="/bin/bash")
        self.assertTrue(result.asked, result.output)
        self.assertIn("Nix was not installed. The Nix installer stopped with an error.\n", result.output)
        self.assert_only_client(sandbox, result)

    def test_failed_nix_installer_download_keeps_the_client(self) -> None:
        sandbox = self.make_sandbox()
        result = sandbox.run_pty("y", shell="/bin/bash")
        self.assertTrue(result.asked, result.output)
        self.assertIn(
            "Nix was not installed: the Nix installer could not be downloaded.\n", result.output
        )
        self.assertIn(NIX_INSTALLER_URL, sandbox.curl_urls())
        self.assert_only_client(sandbox, result)

    def test_no_question_when_a_nix_candidate_exists(self) -> None:
        for name, text in (("working", FAKE_NIX), ("broken", BROKEN_NIX)):
            with self.subTest(candidate=name):
                sandbox = self.make_sandbox()
                sandbox.write_nix_installer()
                nix = self.plant(sandbox.home / ".nix-profile" / "bin", text)
                result = sandbox.run_pty(None, shell="/bin/bash")
                self.assertEqual(result.returncode, 0, result.output)
                self.assertFalse(result.asked, result.output)
                self.assertNotIn(NIX_INSTALLER_URL, sandbox.curl_urls())
                if name == "broken":
                    self.assertIn(f"pkg found {nix}, but it does not work.", result.output)
                else:
                    self.assertIn(SEARCH_DONE, result.output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
