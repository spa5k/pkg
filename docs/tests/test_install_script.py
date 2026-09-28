#!/usr/bin/env python3
"""Offline checks for the client downloader install.sh."""

from __future__ import annotations

import hashlib
import io
import os
import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "install.sh"
VERSION = "0.2.0-alpha.2"
TAG = f"v{VERSION}"
SYSTEM = "x86_64-linux"
ARCHIVE = f"pkg-{VERSION}-{SYSTEM}.tar.gz"
MEMBER_ROOT = f"pkg-{VERSION}-{SYSTEM}"
CLIENT = f"{MEMBER_ROOT}/bin/pkg"
CLIENT_BYTES = b"#!/bin/sh\necho pkg-ok\n"

FAKE_UNAME = '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n'


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


def run_installer(
    fixtures: pathlib.Path, sandbox: pathlib.Path
) -> subprocess.CompletedProcess[str]:
    """Run install.sh with fixture downloads into sandbox targets.

    curl and uname are faked from a private PATH entry. TMPDIR is inside
    the sandbox, so any file the installer (or a malicious archive) writes
    outside the install targets still lands inside the sandbox.
    """
    fake_bin = sandbox / "fake-bin"
    fake_bin.mkdir()
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

    home = sandbox / "home"
    home.mkdir()
    tmpdir = sandbox / "tmp"
    tmpdir.mkdir()
    env = dict(os.environ)
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    env["HOME"] = str(home)
    env["TMPDIR"] = str(tmpdir)
    env["PKG_INSTALL_BIN"] = str(sandbox / "bin-target")
    env["PKG_INSTALL_COMPLETIONS"] = str(sandbox / "completions-target")
    return subprocess.run(
        ["sh", str(SCRIPT)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def no_tree_extracted(sandbox: pathlib.Path) -> bool:
    """The archive's directory tree must never appear on disk."""
    return not list(sandbox.rglob(MEMBER_ROOT))


class DownloaderTests(unittest.TestCase):
    def test_takes_no_arguments(self) -> None:
        result = subprocess.run(
            ["sh", str(SCRIPT), "--url", "https://attacker.invalid"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Usage:", result.stderr)

    def test_unsupported_system_refuses_before_network(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fake_bin = pathlib.Path(directory)
            marker = pathlib.Path(directory) / "curl-was-called"
            fake_bin.joinpath("uname").write_text(
                '#!/bin/sh\necho SunOS\n'
            )
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
            for name in ("curl", "tar", "install", "awk", "grep"):
                fake_bin.joinpath(name).write_text("#!/bin/sh\nexit 99\n")
            for name in ("uname", "curl", "tar", "install", "awk", "grep"):
                fake_bin.joinpath(name).chmod(0o700)
            result = subprocess.run(
                ["/bin/sh", str(SCRIPT)],
                env={"PATH": directory},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("shasum or sha256sum", result.stderr)

    def test_downloader_contract(self) -> None:
        text = SCRIPT.read_text()
        # One exact release: version, tag, and archive names must agree.
        self.assertIn(f'version="{VERSION}"', text)
        self.assertIn(f'tag="v${{version}}"', text)
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
        # No shell piping into execution and no eval.
        self.assertNotIn("curl |", text)
        self.assertNotRegex(text, r"(?m)^\s*eval\s")

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
        # Members that escape via .. or an absolute path pass an honest
        # release checksum. They must never be written anywhere.
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
        # ../../ lands beside the sandboxed TMPDIR, root/.. inside it.
        self.assertFalse((sandbox / "outside.txt").exists())
        self.assertFalse((sandbox / "tmp" / "inside-outside.txt").exists())
        self.assertFalse(pathlib.Path("/pkg-test-absolute.txt").exists())
        self.assertTrue(no_tree_extracted(sandbox))

    def test_client_symlink_member_is_refused(self) -> None:
        # bin/pkg stored as a link must not be read as the client.
        result, sandbox = self.run_once(
            omit_client=True,
            extra_members=((CLIENT, tarfile.SYMTYPE, "/etc/passwd"),),
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("regular file", result.stderr)
        self.assertFalse((sandbox / "bin-target").exists())
        self.assertTrue(no_tree_extracted(sandbox))

    def test_duplicate_client_member_is_refused(self) -> None:
        # A member stored twice, or stored once as a file and once as a
        # link, must be refused: exactly one regular file is required.
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
        # bin/ stored as a symlink out of the tree, with bin/pkg stored as
        # a regular file under it: only member bytes are read to stdout, so
        # nothing may appear at the link target.
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
