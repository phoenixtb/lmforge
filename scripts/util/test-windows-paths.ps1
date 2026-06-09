$ErrorActionPreference = "Stop"
$checks = @(
    @{ Name = "core binary"; Path = "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe" },
    @{ Name = "UI binary"; Path = "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe" },
    @{ Name = "bundled core in UI"; Path = "$env:LOCALAPPDATA\LMForge\bin\lmforge.exe" },
    @{ Name = "vulkan loader"; Path = "$env:SystemRoot\System32\vulkan-1.dll" }
)
$fail = 0
foreach ($c in $checks) {
    if (Test-Path $c.Path) {
        Write-Host "OK   $($c.Name): $($c.Path)"
    } else {
        Write-Host "MISS $($c.Name): $($c.Path)"
        $fail++
    }
}
# AMD detection helper (string logic)
function Test-AmdCompat([string]$s) {
    $l = $s.ToLower()
    $l.Contains("advanced micro devices") -or $l.Contains("amd")
}
if (Test-AmdCompat "NVIDIA;Advanced Micro Devices, Inc.") {
    Write-Host "OK   AMD compat string detection"
} else {
    Write-Host "FAIL AMD compat string detection"
    $fail++
}
if ($fail -gt 0) { exit 1 }
