$ErrorActionPreference = "Stop"
# Parse every tracked PowerShell script. A single parse error makes a whole
# script unusable on Windows (e.g. "$Total:" read as a scope-qualified
# variable broke tests/multi_model_e2e.ps1), so CI runs this on every PR.
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Push-Location $root
try {
    $scripts = @(git ls-files '*.ps1')
} finally {
    Pop-Location
}
if ($scripts.Count -eq 0) { throw "no .ps1 files found under $root" }
$fail = 0
foreach ($rel in $scripts) {
    $path = Join-Path $root $rel
    $errs = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$null, [ref]$errs)
    if ($errs.Count -gt 0) {
        Write-Host "FAIL $rel"
        $errs | ForEach-Object { Write-Host "  L$($_.Extent.StartLineNumber): $($_.Message)" }
        $fail++
    } else {
        Write-Host "OK   $rel"
    }
}
if ($fail -gt 0) { exit 1 }
