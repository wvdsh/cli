[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ArtifactDirectory,
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedSubject
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-ReleaseSignature([string]$FilePath) {
    $signature = Get-AuthenticodeSignature -LiteralPath $FilePath
    if ($signature.Status -ne 'Valid') {
        throw "Invalid signature on ${FilePath}: $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -cne $ExpectedSubject) {
        throw "Unexpected signing publisher on $FilePath"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "Missing trusted timestamp on $FilePath"
    }
    Write-Host "Verified $FilePath (publisher: $ExpectedSubject)"
}

if ([string]::IsNullOrWhiteSpace($ExpectedSubject)) {
    throw 'WINDOWS_SIGNING_SUBJECT must match the Public Trust certificate subject exactly'
}

$artifactRoot = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$installer = Join-Path $artifactRoot 'wavedash-installer.ps1'
$archivePath = Join-Path $artifactRoot 'wavedash-x86_64-pc-windows-msvc.zip'
$checksumPath = "$archivePath.sha256"
foreach ($required in @($installer, $archivePath, $checksumPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Missing required Windows release artifact: $required"
    }
}

$checksum = (Get-Content -LiteralPath $checksumPath -Raw).Trim()
if ($checksum -notmatch '^([a-fA-F0-9]{64})\s+\*?wavedash-x86_64-pc-windows-msvc\.zip$') {
    throw 'Invalid Windows archive checksum file'
}
if ((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash -ne $Matches[1]) {
    throw 'Windows archive checksum mismatch'
}

Assert-ReleaseSignature $installer

# Extract only the expected executable, without trusting paths in ZIP entries.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$verifyDirectory = Join-Path $temporaryRoot ('wavedash-signature-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $verifyDirectory
try {
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $executables = @($archive.Entries | Where-Object { $_.FullName -ceq 'wavedash.exe' })
        if ($executables.Count -ne 1) {
            throw 'Expected exactly one wavedash.exe at the root of the Windows ZIP'
        }
        $binaryPath = Join-Path $verifyDirectory 'wavedash.exe'
        [IO.Compression.ZipFileExtensions]::ExtractToFile($executables[0], $binaryPath)
    } finally {
        $archive.Dispose()
    }
    Assert-ReleaseSignature $binaryPath
} finally {
    $resolvedDirectory = [IO.Path]::GetFullPath($verifyDirectory)
    if ([IO.Path]::GetDirectoryName($resolvedDirectory) -ne $temporaryRoot.TrimEnd('\', '/')) {
        throw "Refusing to clean up unexpected path: $resolvedDirectory"
    }
    Remove-Item -LiteralPath $resolvedDirectory -Recurse -Force
}
