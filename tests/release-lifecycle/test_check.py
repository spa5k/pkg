import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("lifecycle", Path(__file__).with_name("check.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        with patch.object(module.Path, "home", return_value=self.work):
            self.checks = module.Checks(self.work)

    def test_system_fallback_cannot_hide_a_missing_managed_command(self):
        fallback = self.work / "fallback"
        fallback.mkdir()
        executable = fallback / "fzf"
        executable.write_text("#!/bin/sh\nexit 0\n")
        executable.chmod(0o700)
        self.checks.env["PATH"] = str(fallback) + os.pathsep + os.environ["PATH"]
        with self.assertRaises(FileNotFoundError):
            self.checks.package("fzf", "Missing managed command")
        self.assertEqual(self.checks.rows[-1]["status"], "failed")

    def test_removal_detects_even_a_broken_activation_link(self):
        self.checks.bin.mkdir(parents=True)
        (self.checks.bin / "fzf").symlink_to("/missing-proof-output")
        with self.assertRaises(ValueError):
            self.checks.removed("fzf", "")

    def test_invalid_tag_is_refused(self):
        with self.assertRaises(ValueError):
            module.valid_tag("v1.0.0-alpha.1; id")

    def test_missing_confirmation_does_not_probe_or_mutate_the_host(self):
        with patch.object(module.subprocess, "check_output", side_effect=AssertionError("host probe")):
            with self.assertRaises(ValueError):
                module.require_disposable("yes")
