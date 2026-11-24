# Write GroveUpdate.json for a release: the installer's name, size and SHA-256.
# The in-app updater (crates/grove/src/update.rs) accepts a download only when
# all three match. Releases are not signed yet; see packaging/winget/README.md.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Installer,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$Destination
)
$ErrorActionPreference = 'Stop'
$file = Get-Item -LiteralPath $Installer
if ($file.Name -ne "GroveSetup-$Version.exe") { throw "Expected GroveSetup-$Version.exe" }
if (Test-Path -LiteralPath $Destination) { throw 'Refusing to overwrite the update manifest.' }
$manifest = [ordered]@{
    version = $Version
    installer = $file.Name
    size = $file.Length
    sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
}
[IO.File]::WriteAllText([IO.Path]::GetFullPath($Destination), ($manifest | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
Write-Output "Update manifest written: $Destination"
