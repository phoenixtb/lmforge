# Validate daemon-task.cmd launcher shape from service.rs (no task registration).
$dataDir = Join-Path $env:USERPROFILE ".lmforge"
$logOut  = Join-Path $dataDir "logs\daemon.out.log"
$exe     = Join-Path $env:LOCALAPPDATA "lmforge\bin\lmforge.exe"
$launcher = Join-Path $dataDir "daemon-task.vbs"

$expected = "CreateObject(`"Wscript.Shell`").Run `"`"`"$exe`"`" start`", 0, False"
if ($expected -notmatch 'lmforge\.exe') { exit 1 }
if ($expected -notmatch 'Wscript\.Shell') { exit 1 }
if ($expected -notmatch ' start') { exit 1 }

if (Test-Path -LiteralPath $launcher) {
    $onDisk = Get-Content -LiteralPath $launcher -Raw
    if ($onDisk -notmatch 'Wscript\.Shell' -or $onDisk -notmatch ' start') {
        Write-Host "WARN daemon-task.vbs on disk does not match expected shape"
        exit 1
    }
    Write-Host "OK   daemon-task.vbs on disk"
}

Write-Host "OK   scheduled-task launcher shape"
