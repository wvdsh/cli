# Windows installer integrity

The release workflow runs `scripts/add-installer-checksums.py target/distrib`
after cargo-dist's global build and before uploading the generated PowerShell
installer. It hashes each referenced Windows ZIP, checks the existing release
checksum file, and embeds the digest in every platform entry (including aliases).
The installer compares the downloaded archive with that embedded SHA256 before
extracting or copying files. Missing files, hashing errors, and mismatches stop
installation; there is no unchecked fallback.

This uses cargo-dist 0.30.2 without a signing service. The installer and its
embedded hashes still depend on HTTPS delivery from the trusted release source.
Checksums detect changed downloads; they do not establish a publisher identity
or protect against an attacker replacing both the installer and the archive.
This improvement is related to WVDSH-2364, but is not a confirmed fix for its
unreproduced Defender warning.

## Tests

On Windows with Python 3.11+:

```powershell
python -m unittest discover -s scripts/tests -p 'test_*.py'
$env:INSTALLER_TEST_POWERSHELL = 'pwsh'
python -m unittest discover -s scripts/tests -p 'test_*.py'
```

The tests patch the actual cargo-dist 0.30.2 installer published for CLI 0.1.98,
stored under `scripts/tests/fixtures` with its original license header. They
install harmless fixture archives using local file URLs, temporary directories,
and the unmanaged-install option. No binary is executed and user PATH is not
modified. CI runs this suite on Windows PowerShell 5.1 and PowerShell 7.

## Maintaining the release workflow

`release.yml` is hand-maintained with `allow-dirty = ["ci"]`. Preserve the
checksum step after the global build when regenerating CI. Template changes,
new unsupported artifact types, missing archives, and mismatched checksums cause
the patch step to fail before upload. Update the patcher and regenerate the test
fixture together when upgrading cargo-dist; validate the resulting installer.

If combined with Windows signing, keep checksum injection **before** installer
signing. The ZIP digest must describe the archive containing the already signed
executable. Never modify the installer after signing. The signing draft uses
cargo-dist 0.33.0, so its regenerated installer also needs the integration tests
before both changes ship together.
