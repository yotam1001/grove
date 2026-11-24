# Build a standalone Windows beta with deterministic public path remapping.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$taskRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$taskPreviousFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $env:CARGO_ENCODED_RUSTFLAGS = '-Clink-self-contained=yes' + [char]31 + ('--remap-path-prefix=' + $taskRoot + '=/workspace') + [char]31 + ('--remap-path-prefix=' + $(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $taskRoot '.tools\cargo' }) + '=/cargo')
    & (Join-Path $PSScriptRoot 'cargo-local.ps1') -CargoArgs @('build','--release','--package','grove-music','--locked','--offline')
    if ($LASTEXITCODE -ne 0) { throw 'Grove release build failed.' }
} finally { $env:CARGO_ENCODED_RUSTFLAGS = $taskPreviousFlags }
