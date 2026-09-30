#!/usr/bin/env python3
"""Offline behavioral checks for the client downloader install.sh.

Every test runs the real script with temporary HOME, XDG_STATE_HOME, and
ZDOTDIR targets and fake downloads. No test writes a real user file or
config. Tests that must not see the host's /nix installation run the
script inside a private mount namespace (unshare) with a tmpfs over /nix;
when that isolation is unavailable they are skipped, never faked.
"""

from __future__ import annotations

import hashlib
import io
import os
import pathlib
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "install.sh"
VERSION = "0.2.0-alpha.8"
TAG = f"v{VERSION}"
SYSTEM = "x86_64-linux"
ARCHIVE = f"pkg-{VERSION}-{SYSTEM}.tar.gz"
MEMBER_ROOT = f"pkg-{VERSION}-{SYSTEM}"
CLIENT = f"{MEMBER_ROOT}/bin/pkg"
CLIENT_BYTES = b"#!/bin/sh\necho pkg-ok\n"

FAKE_UNAME = '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n'
BEGIN_MARKER = "# >>> pkg paths >>>"
END_MARKER = "# <<< pkg paths <<<"

# The isolation prefix for tests that must not observe the host /nix.
UNSHARE = [
    "unshare", "--user", "--map-root-user", "--mount",
    "--propagation", "private",
]


def add_file(tf: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o755
    tf.addfile(info, io.BytesIO(data))


def add_link(tf: tarfile.TarFile, name: str, kind: int, target: str) -> None:
    info = tarfile.TarInfo(name)
    info.type = kind
    info.linkname = target
    tf.addfile(info)


def build_release(
    directory: pathlib.Path,
    *,
    client_bytes: bytes = CLIENT_BYTES,
    omit_client: bool = False,
    completions: tuple[str, ...] = ("bash", "zsh", "fish"),
    extra_members: tuple[tuple[str, int, str], ...] = (),
    sums_hash: str | None = None,
) -> None:
    """Write the ARCHIVE and SHA256SUMS download fixtures into directory."""
    archive_path = directory / ARCHIVE
    with tarfile.open(archive_path, "w:gz") as tf:
        if not omit_client:
            add_file(tf, CLIENT, client_bytes)
        for shell in completions:
            add_file(
                tf, f"{MEMBER_ROOT}/completions/{shell}.txt",
                f"# {shell} completion\n".encode(),
            )
        for name, kind, target in extra_members:
            if kind is tarfile.REGTYPE:
                add_file(tf, name, b"evil\n")
            else:
                add_link(tf, name, kind, target)
    digest = sums_hash or hashlib.sha256(archive_path.read_bytes()).hexdigest()
    (directory / "SHA256SUMS").write_text(f"{digest}  {ARCHIVE}\n")


def make_fake_nix(directory: pathlib.Path, *, body: str | None = None) -> pathlib.Path:
    """Write an executable fake `nix` into directory and return its path."""
    directory.mkdir(parents=True, exist_ok=True)
    nix = directory / "nix"
    nix.write_text(body or '#!/bin/sh\necho "nix (Nix) 99.0-fake"\n')
    nix.chmod(0o755)
    return nix


def make_fake_tools(fake_bin: pathlib.Path, fixtures: pathlib.Path) -> None:
    """Fake uname and curl (serving the fixtures) inside fake_bin."""
    fake_bin.mkdir(parents=True, exist_ok=True)
    fake_bin.joinpath("uname").write_text(FAKE_UNAME)
    fake_bin.joinpath("curl").write_text(
        "#!/bin/sh\n"
        "out=\n"
        "prev=\n"
        'for arg in "$@"; do\n'
        '  if [ "$prev" = "-o" ]; then out="$arg"; fi\n'
        '  prev="$arg"\n'
        "done\n"
        'case "$(basename "$out")" in\n'
        f'  {ARCHIVE}) cat "{fixtures / ARCHIVE}" > "$out";;\n'
        f'  SHA256SUMS) cat "{fixtures / "SHA256SUMS"}" > "$out";;\n'
        '  *) echo "fake curl: refused $out" >&2; exit 22;;\n'
        "esac\n"
    )
    for name in ("uname", "curl"):
        fake_bin.joinpath(name).chmod(0o700)


def run_installer(
    fixtures: pathlib.Path,
    sandbox: pathlib.Path,
    *,
    env_extra: dict[str, str] | None = None,
    path_prepend: tuple[str, ...] = (),
    sandbox_targets: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run install.sh with fixture downloads into sandbox targets.

    curl and uname are faked from a private PATH entry. TMPDIR is inside
    the sandbox, so any file the installer (or a malicious archive) writes
    outside the install targets still lands inside the sandbox. The
    environment is built from scratch: no SHELL is inherited, so shell
    setup happens only in tests that ask for it.
    """
    fake_bin = sandbox / "fake-bin"
    make_fake_tools(fake_bin, fixtures)

    home = sandbox / "home"
    home.mkdir(exist_ok=True)
    tmpdir = sandbox / "tmp"
    tmpdir.mkdir(exist_ok=True)
    env = {
        "PATH": ":".join([*path_prepend, str(fake_bin), "/usr/bin:/bin"]),
        "HOME": str(home),
        "TMPDIR": str(tmpdir),
    }
    if sandbox_targets:
        env["PKG_INSTALL_BIN"] = str(sandbox / "bin-target")
        env["PKG_INSTALL_COMPLETIONS"] = str(sandbox / "completions-target")
    if env_extra:
        env.update(env_extra)
    result = subprocess.run(
        ["sh", str(SCRIPT)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )
    result.combined = result.stdout + result.stderr  # type: ignore[attr-defined]
    return result


def run_isolated_installer(
    sandbox: pathlib.Path,
    *,
    env_extra: dict[str, str] | None = None,
    inside_setup: str = "",
    path_prepend: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """Run install.sh inside a mount namespace with tmpfs over /nix.

    inside_setup runs inside the namespace before the downloader: the
    host /nix is already hidden when it runs, so fake system-profile
    Nix candidates can be planted safely. HOME and TMPDIR point inside
    the sandbox; nothing outside it is reachable or changed.
    """
    fake_bin = sandbox / "fake-bin"
    home = sandbox / "home"
    tmpdir = sandbox / "tmp"
    make_fake_tools(fake_bin, sandbox / "fixtures")
    for directory in (home, tmpdir):
        directory.mkdir(parents=True, exist_ok=True)
    env = {
        "PATH": ":".join([*path_prepend, str(fake_bin), "/usr/bin:/bin"]),
        "HOME": str(home),
        "TMPDIR": str(tmpdir),
        "PKG_INSTALL_BIN": str(sandbox / "bin-target"),
        "PKG_INSTALL_COMPLETIONS": str(sandbox / "completions-target"),
    }
    if env_extra:
        env.update(env_extra)
    runner = sandbox / "isolated-runner.sh"
    runner.write_text(
        "#!/bin/sh\n"
        "mount -t tmpfs tmpfs /nix || exit 90\n"
        f"{inside_setup}\n"
        f'exec sh {shlex.quote(str(SCRIPT))}\n'
    )
    result = subprocess.run(
        UNSHARE + ["sh", str(runner)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )
    result.combined = result.stdout + result.stderr  # type: ignore[attr-defined]
    return result


def no_tree_extracted(sandbox: pathlib.Path) -> bool:
    """The archive's directory tree must never appear on disk."""
    return not list(sandbox.rglob(MEMBER_ROOT))


def marker_pairs(text: str) -> int:
    """Count well-formed marker pairs: BEGIN ... END in order, one at a time."""
    pairs = 0
    inside = False
    for line in text.splitlines():
        if line == BEGIN_MARKER and not inside:
            inside = True
        elif line == END_MARKER and inside:
            inside = False
            pairs += 1
        elif line in (BEGIN_MARKER, END_MARKER):
            return -1  # malformed nesting
    return pairs if not inside else -1


def block_dirs(path: pathlib.Path) -> list[str]:
    """The literal directory of each case guard inside the block."""
    lines = path.read_text().splitlines()
    dirs: list[str] = []
    inside = False
    for line in lines:
        if line == BEGIN_MARKER:
            inside = True
            continue
        if line == END_MARKER:
            inside = False
            continue
        if inside and line.startswith("  *:") and line.endswith(":*) ;;"):
            raw = line[len("  *:"):-len(":*) ;;")]
            if raw.startswith("'") and raw.endswith("'"):
                dirs.append(raw[1:-1].replace("'\\''", "'"))
    return dirs


def shell_output(
    argv: list[str], home: pathlib.Path, *, extra_env: dict[str, str] | None = None,
) -> str:
    """Run one realistic shell with a minimal environment; return stdout."""
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "TERM": "dumb"}
    if extra_env:
        env.update(extra_env)
    result = subprocess.run(
        argv, env=env, capture_output=True, text=True, check=False, timeout=60
    )
    return result.stdout


class Sandbox:
    """One disposable sandbox with release fixtures, cleaned up per test."""

    def __init__(self) -> None:
        self.path = pathlib.Path(tempfile.mkdtemp(prefix="pkg-installer-test-"))
        self.fixtures = self.path / "fixtures"
        self.fixtures.mkdir()
        build_release(self.fixtures)

    def __enter__(self) -> "Sandbox":
        return self

    def __exit__(self, *exc: object) -> None:
        shutil.rmtree(self.path, ignore_errors=True)

    @property
    def home(self) -> pathlib.Path:
        home = self.path / "home"
        home.mkdir(exist_ok=True)
        return home


class DownloaderTests(unittest.TestCase):
    def test_takes_no_arguments(self) -> None:
        result = subprocess.run(
            ["sh", str(SCRIPT), "--url", "https://attacker.invalid"],
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Usage:", result.stderr)

    def test_unsupported_system_refuses_before_network(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fake_bin = pathlib.Path(directory)
            marker = fake_bin / "curl-was-called"
            fake_bin.joinpath("uname").write_text('#!/bin/sh\necho SunOS\n')
            fake_bin.joinpath("curl").write_text(
                f"#!/bin/sh\ntouch '{marker}'\nexit 99\n"
            )
            for name in ("curl", "uname"):
                fake_bin.joinpath(name).chmod(0o700)
            result = subprocess.run(
                ["sh", str(SCRIPT)],
                env={"PATH": f"{directory}:{os.environ['PATH']}"},
                capture_output=True,
                text=True,
                check=False,
                timeout=60,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("unsupported system", result.stderr)
            self.assertFalse(marker.exists())

    def test_missing_checksum_tool_is_a_clean_error(self) -> None:
        # Every tool before the checksum step is present (faked), but neither
        # shasum nor sha256sum exists. The downloader must refuse with a
        # single clear error before any download.
        with tempfile.TemporaryDirectory() as directory:
            fake_bin = pathlib.Path(directory)
            fake_bin.joinpath("uname").write_text(
                '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n'
            )
            for name in ("curl", "tar", "install", "awk", "grep", "sed", "cmp", "tail", "mktemp"):
                fake_bin.joinpath(name).write_text("#!/bin/sh\nexit 99\n")
                fake_bin.joinpath(name).chmod(0o700)
            fake_bin.joinpath("uname").chmod(0o700)
            result = subprocess.run(
                ["/bin/sh", str(SCRIPT)],
                env={"PATH": directory, "HOME": "/tmp"},
                capture_output=True,
                text=True,
                check=False,
                timeout=60,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("shasum or sha256sum", result.stderr)

    def test_downloader_contract(self) -> None:
        text = SCRIPT.read_text()
        # One exact release: version, tag, and archive names must agree.
        self.assertIn(f'version="{VERSION}"', text)
        self.assertIn('tag="v${{version}}"'.replace("{{", "{").replace("}}", "}"), text)
        self.assertIn('archive="pkg-${version}-${system}.tar.gz"', text)
        self.assertIn("releases/download/${tag}", text)
        self.assertIn("SHA256SUMS", text)
        # Checksum verification supports both common tools.
        self.assertIn("shasum -a 256", text)
        self.assertIn("sha256sum", text)
        # Members are read to stdout into chosen temporary files only.
        self.assertIn("tar -xOzf", text)
        self.assertNotIn("tar -xzf", text)
        self.assertNotIn(' -C "$tmp"', text)
        # The client must exist and must not be empty.
        self.assertIn('if [ ! -s "$tmp/pkg" ]', text)
        # No shell piping into execution and no eval in startup text.
        self.assertNotIn("curl |", text)
        self.assertNotRegex(text, r"(?m)^\s*eval\s")
        # Managed markers are version-independent, so later versions
        # replace this block instead of adding a second one.
        self.assertIn(f'begin_marker="{BEGIN_MARKER}"', text)
        self.assertIn(f'end_marker="{END_MARKER}"', text)
        self.assertNotIn(">>> pkg paths (${", text)

    def test_script_documents_no_nix_clause(self) -> None:
        text = SCRIPT.read_text()
        self.assertIn("does not install Nix", text)


class DownloaderArchiveTests(unittest.TestCase):
    def run_once(self, **build_kwargs) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
        sandbox = pathlib.Path(tempfile.mkdtemp(prefix="pkg-installer-test-"))
        self.addCleanup(shutil.rmtree, sandbox, ignore_errors=True)
        fixtures = sandbox / "fixtures"
        fixtures.mkdir()
        build_release(fixtures, **build_kwargs)
        return run_installer(fixtures, sandbox), sandbox

    def test_installs_client_and_completions(self) -> None:
        result, sandbox = self.run_once()
        self.assertEqual(result.returncode, 0, result.stderr)
        bindir = sandbox / "bin-target"
        compldir = sandbox / "completions-target"
        pkg = bindir / "pkg"
        self.assertTrue(pkg.is_file())
        self.assertEqual(pkg.read_bytes(), CLIENT_BYTES)
        self.assertEqual(pkg.stat().st_mode & 0o777, 0o755)
        self.assertEqual({p.name for p in bindir.iterdir()}, {"pkg"})
        self.assertEqual(
            {p.name for p in compldir.iterdir()},
            {"bash.txt", "zsh.txt", "fish.txt"},
        )
        self.assertEqual(
            (compldir / "zsh.txt").read_text(), "# zsh completion\n"
        )
        # The archive directory tree is never extracted.
        self.assertTrue(no_tree_extracted(sandbox))

    def test_missing_completion_is_skipped(self) -> None:
        result, sandbox = self.run_once(completions=("bash", "fish"))
        self.assertEqual(result.returncode, 0, result.stderr)
        compldir = sandbox / "completions-target"
        self.assertEqual(
            {p.name for p in compldir.iterdir()}, {"bash.txt", "fish.txt"}
        )
        self.assertTrue(no_tree_extracted(sandbox))

    def test_checksum_mismatch_installs_nothing(self) -> None:
        result, sandbox = self.run_once(sums_hash="0" * 64)
        self.assertEqual(result.returncode, 1)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse((sandbox / "bin-target" / "pkg").exists())
        self.assertTrue(no_tree_extracted(sandbox))

    def test_empty_client_is_refused(self) -> None:
        result, sandbox = self.run_once(client_bytes=b"")
        self.assertEqual(result.returncode, 1)
        self.assertIn("is empty", result.stderr)
        self.assertFalse((sandbox / "bin-target").exists())

    def test_missing_client_is_refused(self) -> None:
        result, sandbox = self.run_once(omit_client=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(CLIENT, result.stderr)
        self.assertIn("regular file", result.stderr)
        self.assertFalse((sandbox / "bin-target").exists())

    def test_traversal_members_write_nothing_outside(self) -> None:
        result, sandbox = self.run_once(
            extra_members=(
                ("../../outside.txt", tarfile.REGTYPE, ""),
                (f"{MEMBER_ROOT}/../../inside-outside.txt", tarfile.REGTYPE, ""),
                ("/pkg-test-absolute.txt", tarfile.REGTYPE, ""),
            )
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (sandbox / "bin-target" / "pkg").read_bytes(), CLIENT_BYTES
        )
        self.assertFalse((sandbox / "outside.txt").exists())
        self.assertFalse((sandbox / "tmp" / "inside-outside.txt").exists())
        self.assertFalse(pathlib.Path("/pkg-test-absolute.txt").exists())
        self.assertTrue(no_tree_extracted(sandbox))

    def test_client_symlink_member_is_refused(self) -> None:
        result, sandbox = self.run_once(
            omit_client=True,
            extra_members=((CLIENT, tarfile.SYMTYPE, "/etc/passwd"),),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("regular file", result.stderr)
        self.assertFalse((sandbox / "bin-target").exists())
        self.assertTrue(no_tree_extracted(sandbox))

    def test_duplicate_client_member_is_refused(self) -> None:
        for extra in (
            (CLIENT, tarfile.REGTYPE, ""),
            (CLIENT, tarfile.SYMTYPE, "/etc/passwd"),
        ):
            result, sandbox = self.run_once(extra_members=(extra,))
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("regular file", result.stderr)
            self.assertFalse((sandbox / "bin-target").exists())
            self.assertTrue(no_tree_extracted(sandbox))

    def test_parent_symlink_writes_nothing_through_it(self) -> None:
        evil = "evil-target"
        result, sandbox = self.run_once(
            omit_client=True,
            extra_members=(
                (f"{MEMBER_ROOT}/bin", tarfile.SYMTYPE, evil),
                (CLIENT, tarfile.REGTYPE, ""),
            )
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (sandbox / "bin-target" / "pkg").read_bytes(), b"evil\n"
        )
        self.assertFalse((sandbox / "tmp" / evil).exists())
        self.assertFalse(list(sandbox.rglob(evil)))
        self.assertTrue(no_tree_extracted(sandbox))


class ShellSetupTests(unittest.TestCase):
    """Behavioral tests for Nix detection and managed shell setup."""

    def run_setup(self, sandbox: Sandbox, **kwargs) -> subprocess.CompletedProcess[str]:
        return run_installer(sandbox.fixtures, sandbox.path, **kwargs)

    # -- first install, both Bash files, realistic shells -----------------

    def test_bash_first_install_cover_interactive_and_login(self) -> None:
        with Sandbox() as sandbox:
            nix_dir = sandbox.path / "nix on path"
            make_fake_nix(nix_dir)
            result = self.run_setup(
                sandbox,
                path_prepend=(str(nix_dir),),
                env_extra={"SHELL": "/bin/bash"},
                sandbox_targets=False,
            )
            self.assertEqual(result.returncode, 0, result.combined)
            bindir = sandbox.home / ".local" / "bin"
            self.assertTrue((bindir / "pkg").is_file())

            bashrc = sandbox.home / ".bashrc"
            profile = sandbox.home / ".profile"
            for path in (bashrc, profile):
                self.assertTrue(path.is_file(), path)
                self.assertEqual(marker_pairs(path.read_text()), 1, path)
            expected = [
                str(bindir),
                str(nix_dir),
                str(sandbox.home / ".local/state/nix/profiles/pkg/bin"),
            ]
            self.assertEqual(block_dirs(bashrc), expected)
            self.assertEqual(block_dirs(profile), expected)
            self.assertIn("Shell setup: complete", result.combined)
            self.assertIn(f"Detected Nix: {nix_dir}/nix", result.combined)

            # A realistic fresh interactive Bash resolves everything.
            out = shell_output(
                ["bash", "-ic", 'printf "%s\\n" "$PATH"; command -v pkg'],
                sandbox.home,
            )
            for directory in expected:
                self.assertIn(directory, out)
            self.assertIn(str(bindir / "pkg"), out)
            # A realistic fresh login Bash (reads .profile, not .bashrc).
            out = shell_output(
                ["bash", "-lic", 'printf "%s\\n" "$PATH"; command -v pkg'],
                sandbox.home,
            )
            for directory in expected:
                self.assertIn(directory, out)
            self.assertIn(str(bindir / "pkg"), out)
            # Nix stays reachable although this shell's PATH has no nix.
            out = shell_output(
                ["bash", "-ic", "command -v nix"], sandbox.home
            )
            self.assertIn(str(nix_dir / "nix"), out)

    def test_bash_existing_login_file_wins_and_bash_login_not_created(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            home.joinpath(".bash_profile").write_text("export EDITOR=vi\n")
            home.joinpath(".profile").write_text("keep me\n")
            result = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertEqual(marker_pairs(home.joinpath(".bash_profile").read_text()), 1)
            self.assertTrue(home.joinpath(".bashrc").is_file())
            self.assertEqual(home.joinpath(".profile").read_text(), "keep me\n")
            self.assertFalse(home.joinpath(".bash_login").exists())
            # Login Bash reads the winning .bash_profile, not .profile.
            out = shell_output(
                ["bash", "-lic", "command -v pkg"], home
            )
            self.assertIn(str(sandbox.path / "bin-target" / "pkg"), out)

    def test_bash_login_wins_bash_login_before_profile(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            home.joinpath(".bash_login").write_text("# custom\n")
            home.joinpath(".profile").write_text("keep me\n")
            result = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertEqual(marker_pairs(home.joinpath(".bash_login").read_text()), 1)
            self.assertEqual(home.joinpath(".profile").read_text(), "keep me\n")
            self.assertFalse(home.joinpath(".bash_profile").exists())

    # -- rerun and upgrade ------------------------------------------------

    def test_rerun_is_idempotent(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            home.joinpath(".bashrc").write_text("alias ll='ls -l'\n")
            first = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(first.returncode, 0, first.combined)
            bashrc = home / ".bashrc"
            profile = home / ".profile"
            first_bashrc, first_profile = bashrc.read_bytes(), profile.read_bytes()
            second = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(second.returncode, 0, second.combined)
            self.assertEqual(bashrc.read_bytes(), first_bashrc)
            self.assertEqual(profile.read_bytes(), first_profile)
            self.assertEqual(marker_pairs(bashrc.read_text()), 1)
            self.assertIn("already holds the current", second.combined)

    def test_upgrade_replaces_only_the_managed_block(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            home.joinpath(".bashrc").write_text("alias ll='ls -l'\n")
            self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            new_bindir = sandbox.path / "new-bin"
            second = self.run_setup(
                sandbox, env_extra={"SHELL": "/bin/bash",
                                    "PKG_INSTALL_BIN": str(new_bindir)}
            )
            self.assertEqual(second.returncode, 0, second.combined)
            bashrc = home / ".bashrc"
            text = bashrc.read_text()
            self.assertEqual(marker_pairs(text), 1)
            self.assertIn(str(new_bindir), text)
            self.assertNotIn(str(sandbox.path / "bin-target"), text)
            self.assertIn("alias ll='ls -l'", text)
            self.assertTrue(text.startswith("alias ll='ls -l'\n"))

    def test_upgrade_with_backslash_tmpdir_replaces_block(self) -> None:
        # Regression: the REPLACE step once passed the block file to awk
        # through -v, where a literal backslash-n in the path became a
        # newline. Paths must travel as file arguments instead.
        with Sandbox() as sandbox:
            back_tmp = sandbox.path / "tmp\\nweird"  # literal backslash-n
            back_tmp.mkdir()
            home = sandbox.home
            home.joinpath(".bashrc").write_text("alias ll='ls -l'\n")
            first = self.run_setup(
                sandbox, env_extra={"SHELL": "/bin/bash", "TMPDIR": str(back_tmp)}
            )
            self.assertEqual(first.returncode, 0, first.combined)
            self.assertEqual(
                marker_pairs(home.joinpath(".bashrc").read_text()), 1
            )
            new_bindir = back_tmp / "bin"
            second = self.run_setup(
                sandbox,
                env_extra={"SHELL": "/bin/bash", "TMPDIR": str(back_tmp),
                           "PKG_INSTALL_BIN": str(new_bindir)},
            )
            self.assertEqual(second.returncode, 0, second.combined)
            bashrc = home / ".bashrc"
            text = bashrc.read_text()
            self.assertEqual(marker_pairs(text), 1, text)
            self.assertIn(str(new_bindir), text)
            self.assertNotIn(str(sandbox.path / "bin-target"), text)
            self.assertTrue(text.startswith("alias ll='ls -l'\n"))

    def test_upgrade_preserves_trailing_line_without_final_newline(self) -> None:
        # Regression: the REPLACE step once added a newline to a partial
        # trailing line after the managed block. Unrelated trailing bytes
        # must survive a block replacement byte for byte.
        with Sandbox() as sandbox:
            home = sandbox.home
            zshrc = home / ".zshrc"
            first = self.run_setup(sandbox, env_extra={"SHELL": "/bin/zsh"})
            self.assertEqual(first.returncode, 0, first.combined)
            sentinel = b"export USER_SENTINEL=ok"
            with zshrc.open("ab") as fh:
                fh.write(sentinel)  # no trailing newline
            second = self.run_setup(
                sandbox,
                env_extra={"SHELL": "/bin/zsh",
                           "XDG_STATE_HOME": str(sandbox.path / "other-state")},
            )
            self.assertEqual(second.returncode, 0, second.combined)
            data = zshrc.read_bytes()
            self.assertTrue(data.endswith(sentinel), data)
            self.assertEqual(marker_pairs(zshrc.read_text()), 1)
            self.assertIn(str(sandbox.path / "other-state" / "nix" / "profiles" / "pkg" / "bin"),
                          block_dirs(zshrc))
            self.assertNotIn(str(sandbox.home / ".local" / "state"),
                             block_dirs(zshrc))

    def test_update_uses_a_random_sibling_and_leaves_no_leftover(self) -> None:
        # Regression: the pending file once had a predictable
        # `$file.pkg-new.$$` name that could hit an unrelated file. It
        # must come from mktemp and must never remain after the run.
        with Sandbox() as sandbox:
            home = sandbox.home
            bashrc = home / ".bashrc"
            bashrc.write_text("keep\n")
            bashrc.chmod(0o600)
            decoy = home / ".bashrc.pkg-new.999999"
            decoy.write_text("decoy\n")
            new_bindir = sandbox.path / "new-bin"
            second = self.run_setup(
                sandbox, env_extra={"SHELL": "/bin/bash",
                                    "PKG_INSTALL_BIN": str(new_bindir)}
            )
            self.assertEqual(second.returncode, 0, second.combined)
            self.assertEqual(marker_pairs(bashrc.read_text()), 1)
            self.assertEqual(decoy.read_text(), "decoy\n")
            self.assertEqual(bashrc.stat().st_mode & 0o777, 0o600)
            leftovers = [
                p.name for p in home.iterdir()
                if "pkg-new" in p.name and p != decoy
            ]
            self.assertEqual(leftovers, [])

    # -- preservation: unrelated bytes, modes, no final newline ------------

    def test_unrelated_content_mode_and_missing_final_newline_kept(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            original = b"alias ll='ls -l'\nexport A='x y'\nno newline at end"
            bashrc = home / ".bashrc"
            bashrc.write_bytes(original)
            bashrc.chmod(0o600)
            profile = home / ".profile"
            profile.write_bytes(b"P=1\n")
            profile.chmod(0o640)
            result = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertEqual(bashrc.stat().st_mode & 0o777, 0o600)
            self.assertEqual(profile.stat().st_mode & 0o777, 0o640)
            self.assertTrue(bashrc.read_bytes().startswith(original))
            self.assertEqual(marker_pairs(bashrc.read_text()), 1)
            self.assertTrue(profile.read_bytes().startswith(b"P=1\n"))
            self.assertEqual(marker_pairs(profile.read_text()), 1)

    def test_symlink_startup_file_is_skipped_not_followed(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            target = sandbox.path / "real-zshrc"
            target.write_text("# my config\n")
            zdotdir = sandbox.path / "zdot"
            zdotdir.mkdir()
            zshrc = zdotdir / ".zshrc"
            zshrc.symlink_to(target)
            result = self.run_setup(
                sandbox,
                env_extra={"SHELL": "/bin/zsh", "ZDOTDIR": str(zdotdir)},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertTrue(zshrc.is_symlink())
            self.assertEqual(target.read_text(), "# my config\n")
            self.assertIn("symlink", result.combined)
            self.assertIn("not complete", result.combined)
            self.assertNotIn("Shell setup: complete", result.combined)

    def test_malformed_markers_leave_file_untouched(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            for content in (
                f"{BEGIN_MARKER}\nuser line\n{BEGIN_MARKER}\n{END_MARKER}\n",
                f"{BEGIN_MARKER}\n{END_MARKER}\nuser line\n"
                f"{BEGIN_MARKER}\n{END_MARKER}\n",
                f"{END_MARKER}\nuser line\n{BEGIN_MARKER}\n",
            ):
                bashrc = home / ".bashrc"
                bashrc.write_text(content)
                bashrc.chmod(0o600)
                before = bashrc.read_bytes()
                result = self.run_setup(
                    sandbox, env_extra={"SHELL": "/bin/bash"}
                )
                self.assertEqual(result.returncode, 0, result.combined)
                self.assertEqual(bashrc.read_bytes(), before, content)
                self.assertEqual(bashrc.stat().st_mode & 0o777, 0o600)
                self.assertIn("did not change the file", result.combined)
                # The independent login file still gets its block.
                self.assertEqual(
                    marker_pairs(home.joinpath(".profile").read_text()), 1
                )

    # -- opt-out, unsupported shells, missing SHELL ------------------------

    def test_opt_out_changes_no_startup_file(self) -> None:
        with Sandbox() as sandbox:
            nix_dir = sandbox.path / "nixbin"
            make_fake_nix(nix_dir)
            result = self.run_setup(
                sandbox,
                path_prepend=(str(nix_dir),),
                env_extra={"SHELL": "/bin/bash", "PKG_INSTALL_SHELL_SETUP": "0"},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            home = sandbox.home
            self.assertFalse(home.joinpath(".bashrc").exists())
            self.assertFalse(home.joinpath(".profile").exists())
            self.assertIn("PKG_INSTALL_SHELL_SETUP=0", result.combined)
            self.assertNotIn("Shell setup: complete", result.combined)
            self.assertIn(str(sandbox.path / "bin-target"), result.combined)
            # Nix is reported as detected, but the report makes no claim
            # about new shells when setup did not run.
            self.assertIn(f"Detected Nix: {nix_dir}/nix", result.combined)
            self.assertNotIn("new shells", result.combined)
            self.assertNotIn("New terminals pick the setup up", result.combined)

    def test_unsupported_shell_gets_no_bash_syntax(self) -> None:
        with Sandbox() as sandbox:
            for shell in ("/usr/bin/fish", "/bin/ksh", "/usr/local/bin/tcsh"):
                result = self.run_setup(sandbox, env_extra={"SHELL": shell})
                self.assertEqual(result.returncode, 0, result.combined)
                home = sandbox.home
                self.assertFalse(home.joinpath(".bashrc").exists())
                self.assertFalse(home.joinpath(".zshrc").exists())
                self.assertFalse(home.joinpath(".profile").exists())
                self.assertNotIn("Shell setup: complete", result.combined)
            # Every unsupported shell, Fish included, gets the plain
            # directory list only. No unverified per-shell command line
            # is printed, and no summary claims new terminals get nix.
            for shell in ("/usr/bin/fish", "/bin/ksh"):
                result = self.run_setup(sandbox, env_extra={"SHELL": shell})
                self.assertIn(str(sandbox.path / "bin-target"), result.combined)
                self.assertNotIn("fish_add_path", result.combined)
                self.assertNotIn("export PATH=", result.combined)
                self.assertNotIn("New terminals pick the setup up", result.combined)

    def test_empty_shell_variable_writes_nothing(self) -> None:
        # SHELL is set explicitly to the empty string: on macOS /bin/sh
        # (Bash) synthesizes SHELL from the login account when it is
        # absent, so an unset variable cannot test the unknown-shell
        # case portably. An empty value stays empty everywhere.
        with Sandbox() as sandbox:
            result = self.run_setup(sandbox, env_extra={"SHELL": ""})
            self.assertEqual(result.returncode, 0, result.combined)
            home = sandbox.home
            self.assertFalse(home.joinpath(".bashrc").exists())
            self.assertFalse(home.joinpath(".profile").exists())
            self.assertFalse(home.joinpath(".zshrc").exists())
            self.assertIn("SHELL is empty or not set", result.combined)

    def test_checksum_failure_makes_no_startup_edits(self) -> None:
        with Sandbox() as sandbox:
            build_release(sandbox.fixtures, sums_hash="0" * 64)
            result = self.run_setup(sandbox, env_extra={"SHELL": "/bin/bash"})
            self.assertEqual(result.returncode, 1)
            self.assertIn("checksum mismatch", result.stderr)
            home = sandbox.home
            self.assertFalse(home.joinpath(".bashrc").exists())
            self.assertFalse(home.joinpath(".profile").exists())
            self.assertFalse((sandbox.path / "bin-target").exists())

    # -- hostile paths -----------------------------------------------------

    def test_paths_with_spaces_and_metacharacters(self) -> None:
        with Sandbox() as sandbox:
            weird = sandbox.path / "we'ird \"dq $HOME `tick` *?[ dir"
            bindir = weird / "bin"
            nix_dir = weird / "nixbin"
            make_fake_nix(nix_dir)
            result = self.run_setup(
                sandbox,
                path_prepend=(str(nix_dir),),
                env_extra={"SHELL": "/bin/bash",
                           "PKG_INSTALL_BIN": str(bindir)},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            bashrc = sandbox.home / ".bashrc"
            dirs = block_dirs(bashrc)
            self.assertIn(str(bindir), dirs)
            self.assertIn(str(nix_dir), dirs)
            # A real shell keeps the directory literal and usable.
            out = shell_output(
                ["bash", "-ic", 'printf "%s\\n" "$PATH"; command -v pkg'],
                sandbox.home,
            )
            self.assertIn(str(bindir), out)
            self.assertIn(str(bindir / "pkg"), out)
            self.assertIn("pkg-ok", subprocess.run(
                ["bash", "-ic", "pkg"],
                env={"HOME": str(sandbox.home), "PATH": "/usr/bin:/bin",
                     "TERM": "dumb"},
                capture_output=True, text=True, check=False, timeout=60,
            ).stdout)

    # -- invalid bases are rejected, never silently accepted ---------------

    def test_relative_paths_rejected_before_download(self) -> None:
        with Sandbox() as sandbox:
            fake_bin = sandbox.path / "fake-bin"
            fake_bin.mkdir(exist_ok=True)
            marker = fake_bin / "curl-was-called"
            fake_bin.joinpath("curl").write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 9\n")
            fake_bin.joinpath("curl").chmod(0o700)
            for setting, value in (
                ("PKG_INSTALL_BIN", "relative/bin"),
                ("PKG_INSTALL_COMPLETIONS", "relative/completions"),
                ("XDG_STATE_HOME", "relative/state"),
            ):
                result = run_installer(
                    sandbox.fixtures, sandbox.path,
                    env_extra={setting: value},
                )
                self.assertEqual(result.returncode, 1, result.combined)
                self.assertIn(f"{setting} is not an absolute path", result.stderr)
                self.assertFalse(marker.exists())
            result = run_installer(
                sandbox.fixtures, sandbox.path, env_extra={"HOME": "relative/home"}
            )
            self.assertEqual(result.returncode, 1, result.combined)
            self.assertIn("HOME is not an absolute path", result.stderr)
            self.assertFalse(marker.exists())

    def test_unrepresentable_path_directory_skips_shell_setup(self) -> None:
        with Sandbox() as sandbox:
            result = self.run_setup(
                sandbox,
                env_extra={"SHELL": "/bin/bash",
                           "PKG_INSTALL_BIN": str(sandbox.path / "bad:dir")},
            )
            self.assertEqual(result.returncode, 1, result.combined)
            self.assertIn("colon", result.combined)
            home = sandbox.home
            self.assertFalse(home.joinpath(".bashrc").exists())
            self.assertFalse(home.joinpath(".profile").exists())
            # The client itself stays installed.
            self.assertTrue((sandbox.path / "bad:dir" / "pkg").is_file())

    # -- Zsh ---------------------------------------------------------------

    def test_zsh_setup_with_zdotdir_and_native_shell(self) -> None:
        with Sandbox() as sandbox:
            nix_dir = sandbox.path / "nixdir"
            make_fake_nix(nix_dir)
            zdotdir = sandbox.path / "fresh-zdot"  # does not exist yet
            result = self.run_setup(
                sandbox,
                path_prepend=(str(nix_dir),),
                env_extra={"SHELL": "/bin/zsh", "ZDOTDIR": str(zdotdir)},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertTrue(zdotdir.is_dir())
            zshrc = zdotdir / ".zshrc"
            self.assertEqual(marker_pairs(zshrc.read_text()), 1)
            if shutil.which("zsh"):
                out = shell_output(
                    ["zsh", "-ic", 'printf "%s\\n" "$PATH"; command -v pkg'],
                    sandbox.home, extra_env={"ZDOTDIR": str(zdotdir)},
                )
                self.assertIn(str(sandbox.path / "bin-target"), out)
                self.assertIn(str(sandbox.path / "bin-target" / "pkg"), out)
                self.assertIn(str(nix_dir), out)
            else:
                self.skipTest("zsh is not installed for the native-shell check")

    def test_zsh_without_zdotdir_uses_home_zshrc(self) -> None:
        with Sandbox() as sandbox:
            result = self.run_setup(sandbox, env_extra={"SHELL": "/bin/zsh"})
            self.assertEqual(result.returncode, 0, result.combined)
            zshrc = sandbox.home / ".zshrc"
            self.assertEqual(marker_pairs(zshrc.read_text()), 1)
            # Bash files stay untouched for a Zsh user.
            self.assertFalse(sandbox.home.joinpath(".bashrc").exists())

    def test_relative_zdotdir_is_rejected(self) -> None:
        with Sandbox() as sandbox:
            result = self.run_setup(
                sandbox, env_extra={"SHELL": "/bin/zsh", "ZDOTDIR": "relative/zdot"}
            )
            self.assertEqual(result.returncode, 1, result.combined)
            self.assertIn("ZDOTDIR is not an absolute path", result.stderr)
            self.assertFalse(sandbox.home.joinpath(".zshrc").exists())

    def test_xdg_state_home_moves_profile_bin(self) -> None:
        with Sandbox() as sandbox:
            nix_dir = sandbox.path / "nixdir"
            make_fake_nix(nix_dir)
            state = sandbox.path / "xdg-state"
            result = self.run_setup(
                sandbox,
                path_prepend=(str(nix_dir),),
                env_extra={"SHELL": "/bin/bash", "XDG_STATE_HOME": str(state)},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            dirs = block_dirs(sandbox.home / ".bashrc")
            self.assertIn(str(state / "nix" / "profiles" / "pkg" / "bin"), dirs)


class NixDetectionTests(unittest.TestCase):
    """Nix detection without a trusted PATH, under real /nix isolation."""

    def setUp(self) -> None:
        try:
            subprocess.run(
                UNSHARE + ["true"], capture_output=True, check=True, timeout=30
            )
        except (OSError, subprocess.SubprocessError):
            self.skipTest(
                "mount-namespace isolation (unshare) is unavailable here; "
                "the host /nix exists, so the missing-Nix case cannot be "
                "tested soundly without it"
            )

    def test_no_nix_found_leaves_client_with_guidance(self) -> None:
        with Sandbox() as sandbox:
            result = run_isolated_installer(
                sandbox.path, env_extra={"SHELL": "/bin/bash"}
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertTrue((sandbox.path / "bin-target" / "pkg").is_file())
            self.assertIn("No usable Nix installation was found", result.combined)
            self.assertIn(
                "https://docs.determinate.systems/determinate-nix/",
                result.combined,
            )
            # The summary never claims nix for new shells when none exists.
            self.assertNotIn("nix, and pkg packages on PATH", result.combined)
            # Bash setup still completed, without any Nix directory.
            dirs = block_dirs(sandbox.home / ".bashrc")
            self.assertEqual(len(dirs), 2)  # client bin + profile bin
            self.assertIn("Shell setup: complete", result.combined)

    def test_system_profile_found_outside_path(self) -> None:
        with Sandbox() as sandbox:
            fake = "/nix/var/nix/profiles/default/bin"
            result = run_isolated_installer(
                sandbox.path,
                env_extra={"SHELL": "/bin/bash"},
                inside_setup=(
                    f"mkdir -p '{fake}' && "
                    "printf '#!/bin/sh\\necho nix-ok\\n' > "
                    f"'{fake}/nix' && chmod +x '{fake}/nix'"
                ),
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn(f"Detected Nix: {fake}/nix", result.combined)
            dirs = block_dirs(sandbox.home / ".bashrc")
            self.assertIn(fake, dirs)
            self.assertIn("Shell setup: complete", result.combined)

    def test_modern_user_profile_preferred_over_legacy(self) -> None:
        with Sandbox() as sandbox:
            home = sandbox.home
            modern = home / ".local/state/nix/profile/bin"
            legacy = home / ".nix-profile/bin"
            make_fake_nix(modern)
            make_fake_nix(legacy)
            result = run_isolated_installer(
                sandbox.path, env_extra={"SHELL": "/bin/bash"}
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn(f"Detected Nix: {modern}/nix", result.combined)
            dirs = block_dirs(sandbox.home / ".bashrc")
            self.assertIn(str(modern), dirs)
            self.assertNotIn(str(legacy), dirs)

    def test_legacy_user_profile_used_when_modern_absent(self) -> None:
        with Sandbox() as sandbox:
            legacy = sandbox.home / ".nix-profile/bin"
            make_fake_nix(legacy)
            result = run_isolated_installer(
                sandbox.path, env_extra={"SHELL": "/bin/bash"}
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn(f"Detected Nix: {legacy}/nix", result.combined)

    def test_xdg_state_home_nix_profile_candidate(self) -> None:
        with Sandbox() as sandbox:
            state = sandbox.path / "xdg-state"
            candidate = state / "nix/profile/bin"
            make_fake_nix(candidate)
            result = run_isolated_installer(
                sandbox.path,
                env_extra={"SHELL": "/bin/bash", "XDG_STATE_HOME": str(state)},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn(f"Detected Nix: {candidate}/nix", result.combined)

    def test_non_executable_candidate_is_skipped(self) -> None:
        with Sandbox() as sandbox:
            dead = sandbox.path / "dead-nix"
            dead.mkdir()
            nix = dead / "nix"
            nix.write_text("#!/bin/sh\necho nix-ok\n")
            nix.chmod(0o644)  # exists, but not executable
            result = run_isolated_installer(
                sandbox.path,
                path_prepend=(str(dead),),
                env_extra={"SHELL": "/bin/bash"},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn("No usable Nix installation was found", result.combined)
            self.assertNotIn("Detected Nix", result.combined)

    def test_probe_failure_reports_the_candidate(self) -> None:
        with Sandbox() as sandbox:
            broken = sandbox.path / "broken-nix"
            make_fake_nix(broken, body="#!/bin/sh\nexit 3\n")
            result = run_isolated_installer(
                sandbox.path,
                path_prepend=(str(broken),),
                env_extra={"SHELL": "/bin/bash"},
            )
            self.assertEqual(result.returncode, 0, result.combined)
            self.assertIn("No usable Nix installation was found", result.combined)
            self.assertIn(str(broken / "nix"), result.combined)
            self.assertIn("did not answer", result.combined)


if __name__ == "__main__":
    unittest.main(verbosity=2)
