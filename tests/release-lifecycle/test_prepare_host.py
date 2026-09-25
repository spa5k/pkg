import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("prepare_host", Path(__file__).with_name("prepare_host.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class HostFixtureTests(unittest.TestCase):
    def test_local_host_is_refused_before_mutation(self):
        with patch.dict(module.os.environ, {}, clear=True), patch.object(module.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                module.prepare()
            run.assert_not_called()

    def test_later_symlink_is_refused_before_any_directory_change(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            link = root / "bin"
            link.symlink_to(root, target_is_directory=True)
            with (patch.dict(module.os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}),
                  patch.object(module.os, "geteuid", return_value=1000),
                  patch.object(module.os.path, "lexists", return_value=False),
                  patch.object(module, "PREFIXES", (root, link)),
                  patch.object(module.subprocess, "run") as run):
                with self.assertRaises(ValueError):
                    module.prepare()
                run.assert_not_called()
