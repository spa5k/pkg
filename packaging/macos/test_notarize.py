import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("notarize", Path(__file__).with_name("notarize.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.assets = self.work / "assets"
        self.assets.mkdir()
        for name in module.BINARIES:
            path = self.assets / name
            path.write_bytes(b"original")
            path.chmod(0o700)
        keychain = self.work / "keychain"
        keychain.write_bytes(b"fixture")
        self.args = argparse.Namespace(assets=self.assets, output=self.work / "output", version="1.0.0",
            team="ABCDEFGHIJ", application_identity="Developer ID Application: Test (ABCDEFGHIJ)",
            installer_identity="Developer ID Installer: Test (ABCDEFGHIJ)", keychain=keychain, profile="pkg")

    def test_incorrect_certificate_kind_refuses_before_commands(self):
        self.args.installer_identity = self.args.application_identity
        with self.assertRaises(ValueError):
            module.publish(self.args, lambda *args: self.fail("executed with invalid identities"))

    def test_rejected_notarization_does_not_publish_or_mutate_inputs(self):
        calls = []
        def command(*args):
            calls.append(args)
            return json.dumps({"status": "Invalid", "id": "fixture"}) if "notarytool" in args else ""
        with self.assertRaises(ValueError):
            module.publish(self.args, command)
        self.assertFalse(self.args.output.exists())
        self.assertTrue(all((self.assets / name).read_bytes() == b"original" for name in module.BINARIES))
        self.assertFalse(any("staple" in call for call in calls))

    def test_gatekeeper_failure_never_publishes(self):
        def command(*args):
            if args[0] == "/usr/sbin/spctl":
                raise RuntimeError("Gatekeeper refused")
            return json.dumps({"status": "Accepted", "id": "fixture"}) if "notarytool" in args else ""
        with self.assertRaises(RuntimeError):
            module.publish(self.args, command)
        self.assertFalse(self.args.output.exists())
