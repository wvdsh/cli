# Windows release signing

Windows releases use Azure Artifact Signing with a Public Trust certificate.
`wavedash.exe` is signed before cargo-dist packages and hashes the Windows ZIP.
The generated PowerShell installer is signed after the global build. A separate
Windows job verifies the final installer and the executable extracted from the
release ZIP before `host` can create a public GitHub release. It requires a valid
Authenticode signature, the configured publisher, and a timestamp on both files.
Missing configuration or a failed check stops publication for the entire release.

This addresses the unsigned distribution identified during WVDSH-2364. The
original Defender warning was not reproduced with CLI 0.1.98. Signing establishes
publisher identity; it does not guarantee immediate SmartScreen reputation or
rule out an antivirus false positive.

## One-time Azure setup

An Azure administrator must complete this before the next release tag:

1. Create an Artifact Signing account in the company subscription, following the
   [Azure quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart).
   Complete organization identity validation in the Azure portal, then create a
   **Public Trust** certificate profile. A Private Trust or Public Trust Test
   profile is not appropriate for public releases.
2. Create a Microsoft Entra application/service principal for this repository.
   Configure [GitHub OIDC federation](https://learn.microsoft.com/en-us/azure/developer/github/connect-from-azure-openid-connect)
   with issuer `https://token.actions.githubusercontent.com`, audience
   `api://AzureADTokenExchange`, and subject
   `repo:wvdsh/cli:environment:release`.
3. Grant that identity the **Artifact Signing Certificate Profile Signer** role
   scoped to the selected certificate profile. No client secret or exported
   private key is needed.
4. Create a GitHub environment named `release`. Restrict deployments to release
   tags and protect who can create those tags. Do not allow pull request refs to
   use this environment. The default cargo-dist PR workflow only plans releases;
   do not enable signed PR builds with production signing access.

Set these **secrets** in the GitHub `release` environment. They identify the OIDC
principal; cargo-dist's Azure login step reads them from the secrets context:

| Name | Value |
| --- | --- |
| `AZURE_CLIENT_ID` | Application/client ID of the signing principal |
| `AZURE_TENANT_ID` | Microsoft Entra tenant ID |
| `AZURE_SUBSCRIPTION_ID` | Subscription containing the signing account |

Set these **variables** in that same environment:

| Name | Value |
| --- | --- |
| `WINDOWS_SIGNING_ENDPOINT` | Account's regional HTTPS endpoint, for example `https://eus.codesigning.azure.net/` |
| `WINDOWS_SIGNING_ACCOUNT` | Actual Artifact Signing account name |
| `WINDOWS_SIGNING_PROFILE` | Actual Public Trust certificate profile name |
| `WINDOWS_SIGNING_SUBJECT` | Exact X.509 subject of the validated publisher, as returned by `(Get-AuthenticodeSignature <signed-file>).SignerCertificate.Subject` |

Obtain the publisher subject from the validated profile/certificate or a sample
signed with that profile. Do not guess the company's legal name or copy the
subject from an unrelated binary. The gate matches the complete subject rather
than a certificate thumbprint so Azure can rotate short-lived certificates.

`scripts/configure-windows-signing.py` replaces the markers in
`dist-workspace.toml` only inside signing jobs. It rejects missing configuration
and non-Azure endpoints. There are no working account names or credentials in
the repository. Repository secrets already used for Doppler and Homebrew remain
required by the existing pipeline.

## Validation before rollout

Local checks (Python 3.11+ and Windows PowerShell 5.1 or PowerShell 7):

```powershell
python -m unittest discover -s scripts/tests -p 'test_*.py'
./scripts/tests/test-windows-signatures.ps1
dist plan
actionlint .github/workflows/release.yml .github/workflows/ci.yml
```

The tests use the real Windows trust engine to reject an unsigned script and
mock signature results to cover wrong publishers, tampering, untrusted chains,
missing timestamps, and the successful path without a paid signing account.
These tests do not establish that Azure authentication or production signing
works. Run the first signed prerelease through the release workflow after setup.

For that prerelease, use a fresh Windows 11 installation with current Defender
signatures and Smart App Control enabled. A GitHub-hosted Windows Server runner
does not substitute for this test:

1. Download the release ZIP and installer through a browser. Verify both
   signatures and their timestamps with the verification script. Check the
   publisher displayed by Windows.
2. Install through PowerShell, run `wavedash --version`, and check Defender
   Protection History. Also exercise a browser-downloaded executable through
   Explorer to cover SmartScreen, not just terminal execution.
3. Record Windows version, Defender signature version, Smart App Control state,
   artifact hashes, and any exact warning. A clean antivirus scan does not
   establish SmartScreen reputation.
4. After promoting the signed release, download
   `https://wavedash.com/cli/install.ps1` to disk and verify that its bytes/hash
   match the signed release installer and its Authenticode status is `Valid`.
   The website/CDN must serve the signed artifact unchanged.

Do not disable Defender or Smart App Control to make this test pass. A named
malware detection needs investigation separately from a reputation warning; see
[Microsoft's developer FAQ](https://learn.microsoft.com/en-us/defender-xdr/developer-faq).

## Maintaining the workflow

The pipeline is based on cargo-dist 0.33.0's Azure signing support. Its
`release.yml` is deliberately hand-maintained, with `allow-dirty = ["ci"]`,
because the generated template does not inject account configuration or require
our publisher verification before hosting. When upgrading cargo-dist, generate
a comparison workflow in a temporary checkout without `allow-dirty`, then port
the changes while preserving:

- Configuration before signing in both Windows jobs, OIDC authentication, and
  the protected `release` environment.
- Executable signing before ZIP/checksum generation and installer signing after
  the global build. Upload replacement global artifacts only after signing.
- Explicit failure propagation for native commands and the
  `verify-windows-signatures` dependency and success condition on `host`.
- Read-only verification permissions and OIDC token permission limited to the
  build/signing jobs.

Do not merge/activate this pipeline without the Azure/GitHub setup: the next
release will intentionally fail instead of publishing unsigned Windows files.
