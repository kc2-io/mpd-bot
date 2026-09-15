#requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Tag)
$ErrorActionPreference = 'Stop'
if (-not $Tag.StartsWith('v', [StringComparison]::Ordinal)) {
    throw 'Release tags must start with v, for example v0.2.0 or v0.2.0-rc.1.'
}
$version = $Tag.Substring(1)
try { $parsed = [System.Management.Automation.SemanticVersion]::Parse($version) }
catch { throw 'Release tags must contain a valid semantic version, for example v0.2.0 or v0.2.0-rc.1.' }
if ($parsed.ToString() -cne $version -or $version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
    throw 'Release tags require an exact major.minor.patch semantic version.'
}
[pscustomobject]@{ Version = $version; Tag = $Tag; Prerelease = $version.Split('+')[0].Contains('-') }
