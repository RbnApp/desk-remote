<#
.SYNOPSIS
    Desk-Remote Automated Quality Gates (PowerShell)
.DESCRIPTION
    Runs all architectural quality gates defined in docs/architecture/gates.md.
    Enforces format, clippy lints, unit tests, code duplication, transport isolation,
    hardcoded colors, version consistency, and dependency licenses/advisories.
#>

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "          Desk-Remote Quality Gates Pipeline             " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "Working Directory: $RepoRoot`n"

# Helper function to check CLI tool presence
function Assert-ToolInstalled {
    param(
        [string]$CommandName,
        [string]$ToolFriendlyName
    )
    $cmd = Get-Command $CommandName -ErrorAction SilentlyContinue
    if (-not $cmd) {
        Write-Error "CRITICAL: Required tool '$ToolFriendlyName' ($CommandName) is not installed or not in PATH."
        exit 1
    }
}

# ---------------------------------------------------------
# Step 0: Tool Presence Check
# ---------------------------------------------------------
Write-Host "[Gate Pre-check] Verifying required tools..." -ForegroundColor Yellow
Assert-ToolInstalled "cargo" "Rust Cargo"
Assert-ToolInstalled "rustc" "Rust Compiler"
Assert-ToolInstalled "npx" "Node.js npx"

# Check cargo deny
$hasCargoDeny = $false
try {
    & cargo deny --version >$null 2>&1
    if ($LASTEXITCODE -eq 0) { $hasCargoDeny = $true }
} catch {}

if (-not $hasCargoDeny) {
    Write-Error "CRITICAL: 'cargo-deny' is not installed. Run 'cargo install cargo-deny' to install."
    exit 1
}

Write-Host "All required enforcement tools detected.`n" -ForegroundColor Green

# ---------------------------------------------------------
# Gate 1: Version Consistency Gate
# ---------------------------------------------------------
Write-Host "--> Running Gate 1: Version Consistency..." -ForegroundColor Yellow
if (-not (Test-Path "VERSION")) {
    Write-Error "Gate 1 Failed: Root VERSION file not found!"
    exit 1
}

$RootVersion = (Get-Content -Path "VERSION" -Raw).Trim()
if ($RootVersion -ne "0.1.0") {
    Write-Error "Gate 1 Failed: Root VERSION must strictly be 0.1.0, found: '$RootVersion'"
    exit 1
}
Write-Host "  [PASS] Root VERSION is 0.1.0" -ForegroundColor Green

# ---------------------------------------------------------
# Gate 2: Format Gate (rustfmt)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 2: Code Format Gate (cargo fmt)..." -ForegroundColor Yellow
& cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) {
    Write-Error "Gate 2 Failed: Code formatting violations detected. Run 'cargo fmt --all' to fix."
    exit 1
}
Write-Host "  [PASS] All code conforms to rustfmt rules." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 3: Clippy Lint Gate (Forbidden prints, dbg, unwraps)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 3: Clippy Strict Lint Gate..." -ForegroundColor Yellow
& cargo clippy --all-targets --all-features -- `
    -D warnings `
    -D clippy::print_stdout `
    -D clippy::print_stderr `
    -D clippy::dbg_macro `
    -D clippy::unwrap_used `
    -D clippy::expect_used

if ($LASTEXITCODE -ne 0) {
    Write-Error "Gate 3 Failed: Clippy warnings or banned macros detected!"
    exit 1
}
Write-Host "  [PASS] Clippy passed with zero warnings." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 4: Test Suite Gate (cargo test)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 4: Workspace Unit Tests (cargo test)..." -ForegroundColor Yellow
& cargo test --workspace
if ($LASTEXITCODE -ne 0) {
    Write-Error "Gate 4 Failed: One or more unit tests failed!"
    exit 1
}
Write-Host "  [PASS] All workspace unit tests passed." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 5: Code Duplication Gate (jscpd)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 5: Code Duplication Gate (jscpd)..." -ForegroundColor Yellow
& npx jscpd@4.0.5 crates/ apps/ --threshold 3 --min-tokens 50 --ignore "**/target/**,**/*.json"
if ($LASTEXITCODE -ne 0) {
    Write-Error "Gate 5 Failed: Code duplication threshold (>3%) exceeded!"
    exit 1
}
Write-Host "  [PASS] Code duplication within threshold." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 6: Transport Isolation Grep Gate
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 6: Transport Isolation Grep Gate..." -ForegroundColor Yellow
$TransportViolations = Get-ChildItem -Path crates, apps -Recurse -Filter "*.rs" | 
    Where-Object { 
        $_.FullName -notmatch "crates[\\/]desk-transport" -and 
        $_.FullName -notmatch "apps[\\/]desk-relay" -and
        $_.FullName -notmatch "spikes[\\/]"
    } | 
    Select-String -Pattern "std::net|tokio::net|reqwest|tungstenite|webrtc::"

if ($TransportViolations) {
    Write-Error "Gate 6 Failed: Rogue networking calls detected outside desk-transport / desk-relay:"
    $TransportViolations | ForEach-Object { Write-Host "  $($_.Filename):$($_.LineNumber): $($_.Line.Trim())" -ForegroundColor Red }
    exit 1
}
Write-Host "  [PASS] Transport isolation respected." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 7: Hardcoded Color Gate
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 7: Hardcoded Color Gate..." -ForegroundColor Yellow
$ColorViolations = Get-ChildItem -Path crates, apps -Recurse -Include *.rs,*.slint,*.css,*.html,*.ts,*.tsx |
    Where-Object { 
        $_.FullName -notmatch "crates[\\/]desk-theme" -and 
        $_.FullName -notmatch "spikes[\\/]" 
    } |
    Select-String -Pattern '#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(' |
    Where-Object { $_.Line -notmatch "color-gate:allow" }

if ($ColorViolations) {
    Write-Error "Gate 7 Failed: Hardcoded colors detected outside desk-theme:"
    $ColorViolations | ForEach-Object { Write-Host "  $($_.Filename):$($_.LineNumber): $($_.Line.Trim())" -ForegroundColor Red }
    exit 1
}
Write-Host "  [PASS] No hardcoded colors found." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 8: Dependency Licenses & Security Advisories Gate (cargo-deny)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 8: Dependency & Advisory Gate (cargo-deny)..." -ForegroundColor Yellow
& cargo deny check bans licenses advisories
if ($LASTEXITCODE -ne 0) {
    Write-Error "Gate 8 Failed: cargo-deny reported license or advisory violations!"
    exit 1
}
Write-Host "  [PASS] Dependency bans, licenses, and advisories verified." -ForegroundColor Green

# ---------------------------------------------------------
# Gate 9: Go Code Format & Lint Gate (gofmt, go vet)
# ---------------------------------------------------------
Write-Host "`n--> Running Gate 9: Go Code Format & Vet Gate (gofmt, go vet)..." -ForegroundColor Yellow
$GoHelperDir = Join-Path $RepoRoot "apps\desk-tailnet-helper"
if (Test-Path $GoHelperDir) {
    Assert-ToolInstalled "go" "Go Compiler & Toolchain"
    Push-Location $GoHelperDir
    try {
        $GofmtViolations = & gofmt -s -l .
        if ($GofmtViolations) {
            Write-Error "Gate 9 Failed: Go code formatting violations detected in: $GofmtViolations"
            exit 1
        }
        & go vet .
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Gate 9 Failed: 'go vet' detected issues in apps/desk-tailnet-helper!"
            exit 1
        }
    } finally {
        Pop-Location
    }
    Write-Host "  [PASS] Go helper conforms to gofmt and go vet standards." -ForegroundColor Green
}

Write-Host "`n==========================================================" -ForegroundColor Green
Write-Host "       SUCCESS: ALL 9 AUTOMATED GATES PASSED!            " -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Green
exit 0

