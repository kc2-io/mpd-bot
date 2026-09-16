#requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Executable)
$ErrorActionPreference = 'Stop'
$path = (Resolve-Path -LiteralPath $Executable).Path
$signature = Get-AuthenticodeSignature -LiteralPath $path
if ($signature.Status -ne 'Valid' -or $signature.SignatureType -ne 'Authenticode' -or $null -eq $signature.SignerCertificate) {
    throw "Executable must have a valid embedded Authenticode signature; status: $($signature.Status)."
}
if ($null -eq $signature.TimeStamperCertificate) {
    throw 'Executable must have a timestamped signature.'
}
$codeSigning = $signature.SignerCertificate.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq '1.3.6.1.5.5.7.3.3' }
if (-not $codeSigning) { throw 'Certificate does not include the code-signing usage.' }
Write-Output "Verified timestamped signature: $($signature.SignerCertificate.Subject)"
