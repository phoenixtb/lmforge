# Dry-run the Scheduled Task argument shape from service.rs (no task registration).
$logOut = "$env:USERPROFILE/.lmforge/logs/daemon.out.log"
$exe    = "$env:LOCALAPPDATA/lmforge/bin/lmforge.exe"
$arg    = '/c ""' + $exe + '"" start --foreground >> ""' + $logOut + '"" 2>&1'
Write-Host "arg=$arg"
if ($arg -notmatch 'lmforge\.exe') { exit 1 }
if ($arg -notmatch 'daemon\.out\.log') { exit 1 }
Write-Host "OK scheduled-task arg shape"
