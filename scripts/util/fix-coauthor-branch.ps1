# Rebuild origin/0.2.0-rc1 without Cursor co-author trailers.
# Creates branch 0.2.0-rc1-clean locally. Force-push when satisfied:
#   git push origin 0.2.0-rc1-clean:0.2.0-rc1 --force
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")

git fetch origin 0.2.0-rc1

$base = "642774c"
$commits = @(
    @{
        Hash = "8a9b631"
        Message = @"
chore: bump core + UI to 0.2.0-rc1

Versions aligned across Cargo.toml, ui/package.json,
ui/src-tauri/Cargo.toml, and ui/src-tauri/tauri.conf.json.
Includes the new docs/architecture/ARCHITECTURE.md write-up.
"@
    },
    @{
        Hash = "2c964a1"
        Message = @"
fix(tauri): use plain 0.2.0 for Tauri bundle version

Windows MSI (WiX) requires numeric-only pre-release identifiers,
so 0.2.0-rc1 fails to bundle. Cargo and npm package versions remain
0.2.0-rc1 - only the MSI ProductVersion drops to 0.2.0. The GitHub
Release name still tracks the tag (v0.2.0-rc1).
"@
    },
    @{
        Hash = "f42a88e"
        Message = "Update README.md"
    }
)

git checkout -B 0.2.0-rc1-clean $base

foreach ($c in $commits) {
    git cherry-pick $c.Hash --no-commit
    git commit -m $c.Message
    Write-Host "[+] Rebuilt $($c.Hash)"
}

Write-Host ""
Write-Host "Done. Verify: git log --oneline -5"
Write-Host "Then: git push origin 0.2.0-rc1-clean:0.2.0-rc1 --force"
