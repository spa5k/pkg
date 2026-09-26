import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("render", Path(__file__).with_name("render.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class RenderTests(unittest.TestCase):
    def test_missing_or_linked_asset_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                module.render("v0.1.0-alpha.49", root)
            payload = root / "payload"
            payload.write_bytes(b"bytes")
            (root / "pkg-install-x86_64-linux").symlink_to(payload)
            with self.assertRaises(ValueError):
                module.render("v0.1.0-alpha.49", root)

    def test_invalid_tag_cannot_inject_shell(self):
        for tag in ["$(id)", "v1.0.0'; echo injected", "../v1.0.0", "v1.0.0\n"]:
            with self.assertRaises(ValueError):
                module.render(tag, Path("."))

    def test_render_contains_exact_bytes_for_both_platforms(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ["pkg-install-x86_64-linux", "pkg-0.1.0-alpha.49-preview.pkg", "install-preview.sh"]:
                (root / name).write_bytes(b"bytes")
            text = module.render("v0.1.0-alpha.49", root)
            self.assertNotRegex(text, r"@PKG_[A-Z0-9_]+@")
            self.assertIn("Darwin:arm64", text)
            self.assertIn("Linux:x86_64", text)

    def test_doctor_path_is_idempotent(self):
        import subprocess
        source = (module.ROOT / "docs/install.sh").read_text()
        start = source.index("pkg_check_path=$PATH")
        end = source.index('if ! PATH="$pkg_check_path"', start)
        script = source[start:end] + '\nprintf "%s" "$pkg_check_path"\n'
        managed = "/Users/test/Library/Application Support/pkg/current/bin"
        for initial in ["/usr/bin:/bin", f"{managed}:/usr/local/bin:/usr/bin:/bin", "/usr/local/bin:/usr/bin:/bin"]:
            result = subprocess.check_output(["/bin/sh", "-c", script], env={"PATH": initial, "pkg_user_bin": managed}, text=True)
            self.assertEqual(result.split(":" ).count(managed), 1)
            self.assertEqual(result.split(":" ).count("/usr/local/bin"), 1)
            repeated = subprocess.check_output(["/bin/sh", "-c", script], env={"PATH": result, "pkg_user_bin": managed}, text=True)
            self.assertEqual(repeated, result)

    def test_guarded_startup_survives_removed_binary(self):
        import subprocess
        import shlex
        source = (module.ROOT / "docs/install.sh").read_text()
        snippet = next(line for line in source.split("'") if line.startswith("  [ ! -x /usr/local/bin/pkg ]"))
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "pkg"
            binary.write_text('#!/bin/sh\nprintf "export PKG_SHELL_TEST=ready\\n"\n')
            binary.chmod(0o700)
            snippet = snippet.replace("/usr/local/bin/pkg", shlex.quote(str(binary)))
            for shell in ["/bin/bash", "/bin/zsh"]:
                if not Path(shell).exists():
                    continue
                installed = subprocess.run([shell, "-c", snippet + '; test "$PKG_SHELL_TEST" = ready'], capture_output=True)
                self.assertEqual(installed.returncode, 0, installed.stderr)
            binary.unlink()
            for shell in ["/bin/bash", "/bin/zsh"]:
                if Path(shell).exists():
                    removed = subprocess.run([shell, "-c", snippet], capture_output=True)
                    self.assertEqual(removed.returncode, 0, removed.stderr)
                    self.assertEqual(removed.stderr, b"")
