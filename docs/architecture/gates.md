# Automated Enforcement Gates

## 1. Overview & Purpose

To guarantee that the owner's mandates ("absolute folder discipline, central controls, zero duplicate code, single logger, single transport") are perpetually enforced, Desk-Remote relies on automated quality gates. These gates run in local developer pre-commit hooks and CI pipelines.

Any pull request or commit that violates a gate will be rejected automatically.

---

## 2. Gate Definitions & Commands

| # | Gate Name | Tool & Version | Enforcement Scope | Command |
|---|-----------|----------------|-------------------|---------|
| 1 | **Format Gate** | `rustfmt` (Rust 1.78+) | Code formatting consistency across all crates | `cargo fmt --all -- --check` |
| 2 | **Clippy Lint Gate** | `cargo clippy` (Rust 1.78+) | Banning print macros, debug macros, and unwraps | `cargo clippy --all-targets --all-features -- -D warnings -D clippy::print_stdout -D clippy::print_stderr -D clippy::dbg_macro -D clippy::unwrap_used -D clippy::expect_used` |
| 3 | **Code Duplication Gate** | `jscpd` (v4.0.5+) | Detecting copy-pasted code blocks across crates | `npx jscpd@4.0.5 crates/ apps/ --threshold 3 --min-tokens 50 --ignore "**/target/**,**/*.json"` |
| 4 | **Transport Isolation Grep Gate** | Shell / Ripgrep script | Ensuring raw sockets & HTTP clients only exist in `desk-transport` | See Section 3 below |
| 5 | **Dependency & Advisory Gate** | `cargo-deny` (v0.16.0+) | Verifying MIT-compatible licenses & security advisories | `cargo-deny check bans licenses advisories` |
| 6 | **Version Consistency Gate** | Root script / PowerShell | Verifying all workspace crates match root `VERSION` (0.1.0) | See Section 4 below |
| 7 | **Hardcoded Color Gate** | Shell / Ripgrep script | Ensuring UI sources take colors only from theme tokens | See Section 6 below |

---

## 3. Transport Isolation Grep Gate

To prevent developers from opening rogue network sockets or embedding HTTP requests inside UI, service, or helper crates, the repository enforces an architecture grep gate.

### Rule
Networking primitives (`std::net`, `tokio::net`, `reqwest`, `tungstenite`, `webrtc`) are strictly forbidden outside `crates/desk-transport` and `apps/desk-relay`.

### PowerShell Execution (Windows Local)
```powershell
$Violations = Get-ChildItem -Path crates, apps -Recurse -Filter "*.rs" | 
    Where-Object { 
        $_.FullName -notmatch "crates[\\/]desk-transport" -and 
        $_.FullName -notmatch "apps[\\/]desk-relay" -and
        $_.FullName -notmatch "spikes[\\/]"
    } | 
    Select-String -Pattern "std::net|tokio::net|reqwest|tungstenite|webrtc::"

if ($Violations) {
    Write-Error "Architecture Violation: Network sockets detected outside desk-transport / desk-relay:"
    $Violations | ForEach-Object { Write-Host "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" -ForegroundColor Red }
    exit 1
} else {
    Write-Host "Transport Isolation Gate Passed: No rogue network calls detected." -ForegroundColor Green
}
```

### Bash Execution (CI / Linux Runner)
```bash
#!/usr/bin/env bash
set -euo pipefail

VIOLATIONS=$(grep -rnE "std::net|tokio::net|reqwest|tungstenite|webrtc::" crates/ apps/ \
    --exclude-dir="desk-transport" \
    --exclude-dir="desk-relay" \
    --exclude-dir="spikes" \
    --include="*.rs" || true)

if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: Rogue network primitives detected outside crates/desk-transport:"
    echo "$VIOLATIONS"
    exit 1
fi

echo "Transport Isolation Gate Passed."
```

---

## 4. Version Consistency Gate

Desk-Remote maintains a canonical root `VERSION` file. All crates must synchronize their versioning with this single source of truth.

### Rule
- The root `VERSION` file contains `0.1.0`.
- Individual crate manifests (`Cargo.toml`) in the workspace must use `version.workspace = true` or explicitly match the root `VERSION` string.
- No developer or agent may bump the version without explicit instructions from the project owner.

### Verification Command
```powershell
$RootVersion = (Get-Content -Path "VERSION" -Raw).Trim()
if ($RootVersion -ne "0.1.0") {
    Write-Error "Version Gate Failed: Root VERSION must be 0.1.0, found: $RootVersion"
    exit 1
}
Write-Host "Version Gate Passed: Root VERSION is strictly $RootVersion" -ForegroundColor Green
```

---

## 5. Duplicate Code Gate (`jscpd`)

Desk-Remote uses `jscpd` to mathematically enforce the owner's rule: *"Duplicate code must never be written; parts of the system that share functionality must consume it from a single source"*.

### Configuration (`.jscpd.json`)
```json
{
  "threshold": 3,
  "reporters": ["console"],
  "ignore": [
    "**/target/**",
    "**/tests/**",
    "**/locales/**",
    "**/*.json",
    "**/*.md"
  ],
  "format": ["rust"],
  "minTokens": 50
}
```

If duplicate token blocks exceeding 3% are discovered across `crates/` and `apps/`, the gate immediately breaks the build and identifies the duplicate code locations for refactoring into a shared crate.

---

## 6. Hardcoded Color Gate

All UI colors come from `design/tokens/themes.json` through `desk-theme` (see [Theme System](theme.md)). Color literals in UI sources are rejected.

### Rule
- Hex color literals (`#rgb`, `#rrggbb`, `#rrggbbaa`) and `rgb(`/`rgba(`/`hsl(`/`hsla(` calls are forbidden in `apps/` and `crates/` sources, except inside `design/tokens/themes.json` (not scanned) and `crates/desk-theme` (the resolver and its tests).
- A line may opt out only with a trailing `// color-gate:allow <reason>` comment; every opt-out is reviewed.

### Bash Execution (CI / Linux Runner)
```bash
#!/usr/bin/env bash
set -euo pipefail

PATTERN='#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\('
VIOLATIONS=$(grep -rnE "$PATTERN" apps/ crates/ \
    --exclude-dir="desk-theme" \
    --exclude-dir="spikes" \
    --include="*.rs" --include="*.slint" --include="*.css" --include="*.html" --include="*.ts" --include="*.tsx" \
    | grep -v "color-gate:allow" || true)

if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: Hardcoded colors detected outside the theme system:"
    echo "$VIOLATIONS"
    exit 1
fi

echo "Hardcoded Color Gate Passed."
```

### PowerShell Execution (Windows Local)
```powershell
$Violations = Get-ChildItem -Path crates, apps -Recurse -Include *.rs,*.slint,*.css,*.html,*.ts,*.tsx |
    Where-Object { $_.FullName -notmatch "crates[\/]desk-theme" -and $_.FullName -notmatch "spikes[\/]" } |
    Select-String -Pattern '#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?)\(' |
    Where-Object { $_.Line -notmatch "color-gate:allow" }

if ($Violations) {
    Write-Error "Architecture Violation: Hardcoded colors detected outside the theme system:"
    $Violations | ForEach-Object { Write-Host "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" -ForegroundColor Red }
    exit 1
} else {
    Write-Host "Hardcoded Color Gate Passed." -ForegroundColor Green
}
```
