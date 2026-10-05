"""Exercise the real generated installer with local archives; no network or PATH edits."""

import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "add-installer-checksums.py"
spec = importlib.util.spec_from_file_location("installer_checksums", SCRIPT)
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)
FIXTURE = Path(__file__).parent / "fixtures" / "wavedash-installer-0.1.98.ps1"
ARCHIVE = "wavedash-x86_64-pc-windows-msvc.zip"
PAYLOAD = b"Harmless installer test fixture, never executed."


class InstallerChecksums(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="wavedash-checksums-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.artifacts = self.root / "release"
        self.artifacts.mkdir()
        self.installer = self.artifacts / "wavedash-installer.ps1"
        shutil.copyfile(FIXTURE, self.installer)
        self.archive = self.artifacts / ARCHIVE
        self.checksum = self.artifacts / (ARCHIVE + ".sha256")
        self.write_archive(PAYLOAD)

    def write_archive(self, content):
        with zipfile.ZipFile(self.archive, "w") as archive:
            archive.writestr("wavedash.exe", content)
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksum.write_text(f"{digest} *{ARCHIVE}\n", encoding="utf-8")

    def run_installer(self):
        shell = os.environ.get("INSTALLER_TEST_POWERSHELL", "powershell")
        if not shutil.which(shell):
            self.skipTest(f"{shell} is required for Windows installer integration tests")
        install = self.root / "installed"
        install.mkdir(exist_ok=True)
        # Existing installs must survive a rejected download unchanged.
        (install / "wavedash.exe").write_bytes(b"existing installation")
        scratch = self.root / "scratch"
        scratch.mkdir(exist_ok=True)
        env = os.environ.copy()
        for key in ("WAVEDASH_DOWNLOAD_URL", "INSTALLER_DOWNLOAD_URL", "HTTPS_PROXY",
                    "ALL_PROXY", "WAVEDASH_GITHUB_TOKEN", "WAVEDASH_INSTALLER_GHE_BASE_URL",
                    "WAVEDASH_INSTALLER_GITHUB_BASE_URL"):
            env.pop(key, None)
        env.update(WAVEDASH_DOWNLOAD_URL=self.artifacts.as_uri(),
                   WAVEDASH_UNMANAGED_INSTALL=str(install), WAVEDASH_NO_MODIFY_PATH="1",
                   TEMP=str(scratch), TMP=str(scratch))
        result = subprocess.run(
            [shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
             "-File", str(self.installer)],
            env=env, capture_output=True, text=True, timeout=45,
        )
        return result, install, scratch

    def test_valid_archive_installs(self):
        patcher.add_checksums(self.artifacts)
        # Runtime verification uses the embedded digest, not a mutable sidecar.
        self.checksum.unlink()
        result, install, _ = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((install / "wavedash.exe").read_bytes(), PAYLOAD)

    def test_changed_archive_stops_before_extraction(self):
        patcher.add_checksums(self.artifacts)
        # Update BOTH archive and sidecar after embedding; this must still fail.
        self.write_archive(b"changed download")
        result, install, scratch = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA256 mismatch", result.stdout + result.stderr)
        self.assertEqual((install / "wavedash.exe").read_bytes(), b"existing installation")
        self.assertEqual(list(scratch.rglob("wavedash.exe")), [])

    def test_missing_archive_fails_without_changing_install(self):
        patcher.add_checksums(self.artifacts)
        self.archive.unlink()
        result, install, _ = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((install / "wavedash.exe").read_bytes(), b"existing installation")

    def test_build_rejects_bad_or_missing_checksum_without_editing_installer(self):
        original = self.installer.read_bytes()
        for contents in ("not a checksum", "0" * 64 + " *" + ARCHIVE,
                         "0" * 64 + " *another.zip", None):
            with self.subTest(contents=contents):
                if contents is None:
                    self.checksum.unlink()
                else:
                    self.checksum.write_text(contents)
                with self.assertRaises((ValueError, FileNotFoundError)):
                    patcher.add_checksums(self.artifacts)
                self.assertEqual(self.installer.read_bytes(), original)

    def test_template_changes_fail_closed(self):
        original = self.installer.read_text(encoding="utf-8")
        for changed in (original.replace(patcher.UNPACK, "# changed template"),
                        original.replace(ARCHIVE, "../unexpected.zip"),
                        original + "\n# SIG # Begin signature block\n",
                        original.replace(ARCHIVE, "unsupported.tar.gz")):
            with self.subTest():
                self.installer.write_text(changed, encoding="utf-8")
                with self.assertRaises(ValueError):
                    patcher.add_checksums(self.artifacts)
                self.assertEqual(self.installer.read_text(encoding="utf-8"), changed)

    def test_second_patch_is_rejected(self):
        patcher.add_checksums(self.artifacts)
        once = self.installer.read_bytes()
        with self.assertRaises(ValueError):
            patcher.add_checksums(self.artifacts)
        self.assertEqual(self.installer.read_bytes(), once)


if __name__ == "__main__":
    unittest.main()
