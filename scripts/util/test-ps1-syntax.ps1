$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot "..\.."
$scripts = @(
    "scripts/install-core.ps1",
    "scripts/install-ui.ps1",
    "scripts/uninstall-core.ps1",
    "scripts/uninstall-ui.ps1"
)
$fail = 0
foreach ($rel in $scripts) {
    $path = Join-Path $root $rel
    $errs = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$null, [ref]$errs)
    if ($errs.Count -gt 0) {
        Write-Host "FAIL $rel"
        $errs | ForEach-Object { Write-Host $_.ToString() }
        $fail++
    } else {
        Write-Host "OK   $rel"
    }
}
if ($fail -gt 0) { exit 1 }
