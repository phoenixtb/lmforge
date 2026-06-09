# Validate daemon-task.cmd launcher shape from service.rs (no task registration).
$dataDir = Join-Path $env:USERPROFILE ".lmforge"
$logOut  = Join-Path $dataDir "logs\daemon.out.log"
$exe     = Join-Path $env:LOCALAPPDATA "lmforge\bin\lmforge.exe"
$launcher = Join-Path $dataDir "daemon-task.cmd"

$expected = "@echo off`r`n`"$exe`" start --foreground >> `"$logOut`" 2>&1`r`n"
if ($expected -notmatch 'lmforge\.exe') { exit 1 }
if ($expected -notmatch 'daemon\.out\.log') { exit 1 }
if ($expected -notmatch 'start --foreground') { exit 1 }

if (Test-Path -LiteralPath $launcher) {
    $onDisk = Get-Content -LiteralPath $launcher -Raw
    if ($onDisk -notmatch 'start --foreground') {
        Write-Host "WARN daemon-task.cmd on disk does not match expected shape"
        exit 1
    }
    Write-Host "OK   daemon-task.cmd on disk"
}

Write-Host "OK   scheduled-task launcher shape"
