<#
.SYNOPSIS
    Builds the Go desk-tailnet-helper binary with stripped symbols.
.DESCRIPTION
    Compiles apps/desk-tailnet-helper into desk-tailnet-helper.exe
    ready for embedding into the single DeskRemote-Ajan.exe executable.
#>

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$GoHelperDir = Join-Path $RepoRoot "apps\desk-tailnet-helper"

if (-not (Test-Path $GoHelperDir)) {
    Write-Error "apps/desk-tailnet-helper directory not found at $GoHelperDir"
    exit 1
}

Write-Host "Compiling desk-tailnet-helper.exe in $GoHelperDir..." -ForegroundColor Cyan
Push-Location $GoHelperDir
try {
    & go build -ldflags="-s -w" -o desk-tailnet-helper.exe .
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Go compilation failed with exit code $LASTEXITCODE"
        exit 1
    }
    $file = Get-Item desk-tailnet-helper.exe
    Write-Host "Successfully built desk-tailnet-helper.exe ($($file.Length) bytes)" -ForegroundColor Green
} finally {
    Pop-Location
}
