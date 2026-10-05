# Exercise the publication gate without Azure credentials or changes to trust stores.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression
$verifier = Join-Path $PSScriptRoot '../verify-windows-signatures.ps1'
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$fixtureRoot = Join-Path $temporaryRoot ('wavedash-signature-tests-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $fixtureRoot
$publisher = 'CN=Example Publisher, O=Example Company, C=US'
$script:checks = 0

function New-Fixture([string]$Name, [string]$EntryName = 'wavedash.exe') {
    $directory = Join-Path $fixtureRoot $Name
    $null = New-Item -ItemType Directory -Path $directory
    Set-Content -LiteralPath (Join-Path $directory 'wavedash-installer.ps1') -Value '# unsigned test fixture'
    $zipPath = Join-Path $directory 'wavedash-x86_64-pc-windows-msvc.zip'
    $zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $zip.CreateEntry($EntryName)
        $writer = [IO.StreamWriter]::new($entry.Open())
        try { $writer.Write('unsigned test binary') } finally { $writer.Dispose() }
    } finally { $zip.Dispose() }
    $hash = (Get-FileHash -LiteralPath $zipPath).Hash
    Set-Content -LiteralPath "$zipPath.sha256" -Value "$hash *wavedash-x86_64-pc-windows-msvc.zip"
    return $directory
}

function Assert-Rejected([string]$Directory, [string]$Message) {
    $caught = $null
    try { & $verifier -ArtifactDirectory $Directory -ExpectedSubject $publisher } catch { $caught = $_ }
    if ($null -eq $caught -or $caught.ToString() -notlike "*$Message*") {
        throw "Expected rejection containing '$Message'; got '$caught'"
    }
    $script:checks++
}

try {
    $fixture = New-Fixture 'normal'
    # Real Windows trust engine must reject an unsigned installer.
    Assert-Rejected $fixture 'Invalid signature'

    # Mock only the OS signature result to cover cases requiring paid certificates.
    function Get-AuthenticodeSignature {
        param([string]$LiteralPath)
        $result = [pscustomobject]@{
            Status = 'Valid'
            SignerCertificate = [pscustomobject]@{ Subject = $publisher }
            TimeStamperCertificate = [pscustomobject]@{ Subject = 'Test timestamp' }
        }
        if ($LiteralPath.EndsWith($signatureState.extension)) {
            switch ($signatureState.case) {
                'unsigned' { $result.Status = 'NotSigned' }
                'tampered' { $result.Status = 'HashMismatch' }
                'untrusted' { $result.Status = 'NotTrusted' }
                'publisher' { $result.SignerCertificate.Subject = 'CN=Another Publisher' }
                'timestamp' { $result.TimeStamperCertificate = $null }
            }
        }
        return $result
    }
    $signatureState = @{ extension = ''; case = '' }
    foreach ($extension in @('.ps1', '.exe')) {
        $signatureState.extension = $extension
        foreach ($case in @(
            @('unsigned', 'Invalid signature'),
            @('tampered', 'Invalid signature'),
            @('untrusted', 'Invalid signature'),
            @('publisher', 'Unexpected signing publisher'),
            @('timestamp', 'Missing trusted timestamp')
        )) {
            $signatureState.case = $case[0]
            Assert-Rejected $fixture $case[1]
        }
    }
    $signatureState.case = 'valid'
    & $verifier -ArtifactDirectory $fixture -ExpectedSubject $publisher
    $script:checks++

    $badArchive = New-Fixture 'bad-archive' '../wavedash.exe'
    Assert-Rejected $badArchive 'Expected exactly one wavedash.exe'
    $badChecksum = New-Fixture 'bad-checksum'
    Set-Content -LiteralPath (Join-Path $badChecksum 'wavedash-x86_64-pc-windows-msvc.zip.sha256') -Value (('0' * 64) + ' *wavedash-x86_64-pc-windows-msvc.zip')
    Assert-Rejected $badChecksum 'checksum mismatch'
    $missingInstaller = New-Fixture 'missing-installer'
    Remove-Item -LiteralPath (Join-Path $missingInstaller 'wavedash-installer.ps1')
    Assert-Rejected $missingInstaller 'Missing required Windows release artifact'
    Write-Host "Passed $script:checks Windows signature gate checks"
} finally {
    $resolvedDirectory = [IO.Path]::GetFullPath($fixtureRoot)
    if ([IO.Path]::GetDirectoryName($resolvedDirectory) -ne $temporaryRoot.TrimEnd('\', '/')) {
        throw "Refusing to clean up unexpected path: $resolvedDirectory"
    }
    Remove-Item -LiteralPath $resolvedDirectory -Recurse -Force
}
