"""Embed release archive hashes in cargo-dist's generated PowerShell installer.

Run after the global build, before uploading or signing the installer. Fail closed
if cargo-dist changes the template or the downloaded build artifacts disagree.
"""

import argparse
import hashlib
from pathlib import Path
import re


ARTIFACT = re.compile(r'(?m)^([ \t]*)"artifact_name" = "([A-Za-z0-9_.-]+\.zip)"$')
UNPACK = '  Write-Verbose "Unpacking to $tmp"'
VERIFY = '''  # Verify against the release archive hash embedded by Wavedash CI.
  $expected_hash = $info["sha256"]
  if ($expected_hash -notmatch '^[a-fA-F0-9]{64}$') {
    throw "ERROR: missing or invalid SHA256 for $artifact_name"
  }
  $actual_hash = (Get-FileHash -LiteralPath $dir_path -Algorithm SHA256 -ErrorAction Stop).Hash
  if ($actual_hash -ne $expected_hash) {
    throw "ERROR: SHA256 mismatch for $artifact_name; installation stopped before extraction"
  }

'''


def add_checksums(directory: Path) -> None:
    installer = directory / "wavedash-installer.ps1"
    original = installer.read_text(encoding="utf-8")
    matches = list(ARTIFACT.finditer(original))
    if not matches or original.count(UNPACK) != 1 or '"sha256"' in original:
        raise ValueError("Unexpected or already patched cargo-dist PowerShell template")
    # Every artifact entry must be covered, including architecture fallbacks.
    if len(matches) != len(re.findall(r'"artifact_name"\s*=', original)):
        raise ValueError("Unsupported installer artifact; all downloads must be verified")
    if "# SIG # Begin signature block" in original:
        raise ValueError("Patch the installer before signing it")

    hashes = {}
    for name in sorted({match[2] for match in matches}):
        archive = directory / name
        with archive.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksum = (directory / (name + ".sha256")).read_text(encoding="utf-8").strip()
        expected = re.fullmatch(r"([a-fA-F0-9]{64})\s+\*?" + re.escape(name), checksum)
        if expected is None or expected[1].lower() != digest:
            raise ValueError(f"Release archive checksum mismatch or invalid checksum file: {name}")
        hashes[name] = digest

    result = ARTIFACT.sub(
        lambda match: match[0] + '\n' + match[1] + '"sha256" = "' + hashes[match[2]] + '"',
        original,
    )
    result = result.replace(UNPACK, VERIFY + UNPACK)
    installer.write_text(result, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact_directory", type=Path)
    add_checksums(parser.parse_args().artifact_directory)
