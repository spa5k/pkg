#!/usr/bin/env python3
"""Behavior checks for Formula identity, support decisions, and bottle writes."""

import copy
import gzip
import importlib.util
import io
import os
import pathlib
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


generator = load("formula_generator", ROOT / "tools/formula-catalog/generate.py")
bottle = load("formula_bottle", ROOT / "nix/formulae/lib/bottle.py")


def formula():
    digest = "a" * 64
    return {"name": "probe", "tap": "homebrew/core", "tap_git_head": "b" * 40,
            "homepage": "https://github.com/example/probe", "aliases": [], "desc": "Probe tool",
            "versions": {"stable": "2.0"}, "revision": 0, "deprecated": False, "disabled": False,
            "dependencies": [], "recommended_dependencies": [], "optional_dependencies": [],
            "uses_from_macos": [], "requirements": [], "post_install_defined": False,
            "post_install_steps": [], "service": None, "pour_bottle_only_if": None,
            "bottle": {"stable": {"files": {"arm64_sonoma": {"cellar": ":any_skip_relocation", "sha256": digest,
                "url": f"https://ghcr.io/v2/homebrew/core/probe/blobs/sha256:{digest}"}}}}}


class FormulaTests(unittest.TestCase):
    def test_equivalent_requires_project_and_version_family(self):
        package = formula()
        metadata = {"probe": {"homepage": "https://github.com/example/probe/", "version": "2.1"}}
        self.assertEqual(generator.equivalent(package, metadata)["attribute"], ["probe"])
        for key, value in [("homepage", "https://github.com/other/probe"), ("version", "1.0")]:
            wrong = copy.deepcopy(metadata)
            wrong["probe"][key] = value
            self.assertIsNone(generator.equivalent(package, wrong))

    def test_version_alias_reaches_correct_native_major(self):
        package = formula()
        package.update(name="awscli", aliases=["awscli@2"])
        metadata = {name: {"homepage": package["homepage"], "version": version}
                    for name, version in [("awscli", "1.0"), ("awscli2", "2.1")]}
        self.assertEqual(generator.equivalent(package, metadata)["attribute"], ["awscli2"])
        package.update(name="probe@2.1", aliases=["probe"])
        metadata = {"probe": {"homepage": package["homepage"], "version": "2.0"}}
        self.assertIsNone(generator.equivalent(package, metadata))
        metadata["probe"]["version"] = "2.1.3"
        self.assertEqual(generator.equivalent(package, metadata)["attribute"], ["probe"])

    def test_bottle_refuses_dependencies_hooks_relocation_and_wrong_origin(self):
        package = formula()
        self.assertEqual(generator.bottle_target(package, "aarch64-darwin")["kind"], "formula-bottle")
        for field, value, reason in [("dependencies", ["python"], "formula-dependency"),
                                    ("requirements", None, "malformed-metadata"),
                                    ("post_install_defined", True, "install-hook"),
                                    ("post_install_defined", None, "install-hook"),
                                    ("service", {"run": "daemon"}, "install-hook"),
                                    ("service", False, "install-hook"),
                                    ("pour_bottle_only_if", {"default_prefix": True}, "install-hook")]:
            changed = copy.deepcopy(package)
            changed[field] = value
            self.assertEqual(generator.bottle_target(changed, "aarch64-darwin")["reason"], reason)
        for field, value, reason in [("cellar", ":any", "homebrew-relocation"),
                                    ("url", "https://example.com/probe.tar.gz", "unsupported-bottle-origin")]:
            changed = copy.deepcopy(package)
            changed["bottle"]["stable"]["files"]["arm64_sonoma"][field] = value
            self.assertEqual(generator.bottle_target(changed, "aarch64-darwin")["reason"], reason)

    def test_platform_variation_and_disabled_formula_are_not_bypassed(self):
        package = formula()
        package["variations"] = {"arm64_sonoma": {"dependencies": ["libfoo"]}}
        self.assertEqual(generator.bottle_target(package, "aarch64-darwin")["reason"], "formula-dependency")
        package["disabled"] = True
        metadata = {system: {"probe": {"homepage": package["homepage"], "version": "2.1"}} for system in generator.SYSTEMS}
        catalog = generator.generate([package], metadata, "c" * 64)
        self.assertTrue(all(target["reason"] == "disabled" for target in catalog["entries"]["probe"]["targets"].values()))


class BottleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.old_cwd = os.getcwd()
        os.chdir(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(lambda: os.chdir(self.old_cwd))
        self.plan = {"name": "probe", "cellarVersion": "2.0"}

    def archive(self, members):
        with tarfile.open("probe.tar.gz", "w:gz") as archive:
            for name, body, link in members:
                entry = tarfile.TarInfo(name)
                entry.mode = 0o755
                if link is not None:
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = link
                    archive.addfile(entry)
                else:
                    entry.size = len(body)
                    archive.addfile(entry, io.BytesIO(body))

    def test_install_preserves_internal_links_and_discards_brew_receipt(self):
        self.archive([("probe/2.0/bin/probe", b"#!/bin/sh\necho probe\n", None),
                      ("probe/2.0/bin/alias", b"", "probe"),
                      ("probe/2.0/.brew/probe.rb", b"unsafe recipe code", None),
                      ("probe/2.0/INSTALL_RECEIPT.json", b"{}", None)])
        bottle.install("probe.tar.gz", self.plan, "output")
        self.assertEqual(pathlib.Path("output/bin/alias").read_text(), "#!/bin/sh\necho probe\n")
        self.assertFalse(pathlib.Path("output/.brew").exists())
        self.assertFalse(pathlib.Path("output/INSTALL_RECEIPT.json").exists())

    def test_path_escape_is_refused_before_writes(self):
        self.archive([("probe/2.0/bin/probe", b"safe", None), ("../../outside", b"bad", None)])
        with self.assertRaises(ValueError):
            bottle.install("probe.tar.gz", self.plan, "output")
        self.assertEqual(list(pathlib.Path("bottle-stage").iterdir()), [])
        self.assertFalse(pathlib.Path("output").exists())

    def test_declared_expansion_is_refused_before_reading_the_payload(self):
        entry = tarfile.TarInfo("probe/2.0/bin/probe")
        entry.size = 3 * 1024**3
        pathlib.Path("probe.tar.gz").write_bytes(gzip.compress(entry.tobuf() + bytes(1024)))
        with self.assertRaisesRegex(ValueError, "extraction limits"):
            bottle.install("probe.tar.gz", self.plan, "output")
        self.assertEqual(list(pathlib.Path("bottle-stage").iterdir()), [])
        self.assertFalse(pathlib.Path("output").exists())

    def test_external_link_is_refused(self):
        self.archive([("probe/2.0/bin/probe", b"", "/opt/homebrew/bin/probe")])
        with self.assertRaises(ValueError):
            bottle.install("probe.tar.gz", self.plan, "output")
        self.assertFalse(pathlib.Path("output").exists())

    def test_remaining_homebrew_reference_is_refused(self):
        self.archive([("probe/2.0/bin/probe", b"#!/bin/sh\n/opt/homebrew/bin/dependency\n", None)])
        with self.assertRaises(ValueError):
            bottle.install("probe.tar.gz", self.plan, "output")
        self.assertFalse(pathlib.Path("output").exists())


if __name__ == "__main__":
    unittest.main()
