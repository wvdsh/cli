import importlib.util
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "signing_config", ROOT / "scripts/configure-windows-signing.py"
)
CONFIG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONFIG)


class SigningConfigTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "dist-workspace.toml"
        self.original = (ROOT / "dist-workspace.toml").read_text(encoding="utf-8")
        self.path.write_text(self.original, encoding="utf-8")
        self.env = {
            "WINDOWS_SIGNING_ENDPOINT": "https://eus.codesigning.azure.net/",
            "WINDOWS_SIGNING_ACCOUNT": "example-account",
            "WINDOWS_SIGNING_PROFILE": "example-profile",
        }

    def test_configures_signing_without_changing_other_release_settings(self):
        with patch.dict(os.environ, self.env, clear=True):
            CONFIG.configure(self.path)
        before = tomllib.loads(self.original)
        after = tomllib.loads(self.path.read_text(encoding="utf-8"))
        signing = after["dist"].pop("azure-windows-sign")
        before["dist"].pop("azure-windows-sign")
        self.assertEqual(before, after)
        self.assertEqual(signing["account-name"], "example-account")
        self.assertEqual(signing["certificate-profile-name"], "example-profile")
        self.assertEqual(signing["endpoint"], self.env["WINDOWS_SIGNING_ENDPOINT"])

    def test_missing_configuration_fails_without_partial_write(self):
        for key in self.env:
            with self.subTest(key=key):
                env = {**self.env, key: " "}
                with patch.dict(os.environ, env, clear=True):
                    with self.assertRaisesRegex(ValueError, key):
                        CONFIG.configure(self.path)
                self.assertEqual(self.path.read_text(encoding="utf-8"), self.original)

    def test_rejects_non_azure_or_insecure_endpoints(self):
        for endpoint in (
            "http://eus.codesigning.azure.net/",
            "https://eus.codesigning.azure.net.example.com/",
            "https://example.com/",
            "https://user:secret@eus.codesigning.azure.net/",
            "https://eus.codesigning.azure.net/extra",
            "https://eus.codesigning.azure.net/?extra=1",
        ):
            with self.subTest(endpoint=endpoint):
                with patch.dict(os.environ, {**self.env, "WINDOWS_SIGNING_ENDPOINT": endpoint}, clear=True):
                    with self.assertRaises(ValueError):
                        CONFIG.configure(self.path)
                self.assertEqual(self.path.read_text(encoding="utf-8"), self.original)

    def test_values_cannot_inject_toml_settings(self):
        value = 'profile"\n[dist]\ncargo-dist-version="other'
        with patch.dict(os.environ, {**self.env, "WINDOWS_SIGNING_PROFILE": value}, clear=True):
            CONFIG.configure(self.path)
        result = tomllib.loads(self.path.read_text(encoding="utf-8"))
        self.assertEqual(result["dist"]["azure-windows-sign"]["certificate-profile-name"], value)
        self.assertEqual(result["dist"]["cargo-dist-version"], "0.33.0")

    def test_rejects_changed_marker_without_partial_write(self):
        changed = self.original.replace('account-name = "CONFIGURED_IN_CI"', 'account-name = "already-set"')
        self.path.write_text(changed, encoding="utf-8")
        with patch.dict(os.environ, self.env, clear=True):
            with self.assertRaisesRegex(ValueError, "account-name marker"):
                CONFIG.configure(self.path)
        self.assertEqual(self.path.read_text(encoding="utf-8"), changed)


if __name__ == "__main__":
    unittest.main()
