#requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Executable = 'target/x86_64-pc-windows-msvc/release/mpd-bot.exe',
    [string]$OutputDirectory = 'dist'
)
$ErrorActionPreference = 'Stop'
$release = & (Join-Path $PSScriptRoot 'release-version.ps1') -Tag "v$Version"
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
$stage = Join-Path $outputPath ('staging-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
$stdout = Join-Path $stage 'version.stdout'
$stderr = Join-Path $stage 'version.stderr'
$process = Start-Process -FilePath $executablePath -ArgumentList '--version' -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr
if (-not $process.WaitForExit(10000)) { throw 'Version check timed out; no package was produced.' }
$process.Refresh()
$actual = (Get-Content -LiteralPath $stdout -Raw).Trim()
if ($process.ExitCode -ne 0 -or $actual -cne "MPD Bot $Version") {
    throw "Executable version does not match the requested release ($Version); no package was produced."
}
Copy-Item -LiteralPath $executablePath -Destination (Join-Path $stage 'mpd-bot.exe')
@"
MPD Bot $Version - Windows x64

Extract this ZIP and launch mpd-bot.exe. No installer is needed.
Quit any running MPD Bot instance before upgrading.
Settings and credentials stay in your user configuration directory, outside this ZIP.
The About tab shows the embedded build version.

Source and release notes: https://github.com/kc2-io/mpd-bot/releases/tag/$($release.Tag)
"@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8NoBOM
$archiveName = "mpd-bot-$($release.Tag)-windows-x86_64.zip"
$archive = Join-Path $outputPath $archiveName
if (Test-Path -LiteralPath $archive) { throw 'Package already exists. Use a fresh output directory.' }
Compress-Archive -LiteralPath (Join-Path $stage 'mpd-bot.exe'), (Join-Path $stage 'README.txt') -DestinationPath $archive -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $outputPath 'SHA256SUMS.txt'), "$hash  $archiveName`n")
# Delete only these explicitly created temporary files; no recursive directory operations.
foreach ($name in @('mpd-bot.exe', 'README.txt', 'version.stdout', 'version.stderr')) {
    Remove-Item -LiteralPath (Join-Path $stage $name)
}
Remove-Item -LiteralPath $stage
Write-Output "Created $archive"
