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
