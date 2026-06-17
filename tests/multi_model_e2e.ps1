# =============================================================================
# LMForge — Multi-Model E2E Integration Test (Windows)
# =============================================================================
param(
    [switch]$Full,
    [switch]$WithVlm,
    [switch]$WithRerank,
    [switch]$WithMtp,
    [int]$N = $(if ($env:N_REQUESTS) { [int]$env:N_REQUESTS } else { 10 })
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
. (Join-Path $RepoRoot "scripts\lib\e2e-api.ps1")

$SkipPull    = ($env:SKIP_PULL -match '^(1|true|yes)$')
$SkipStart   = ($env:SKIP_START -match '^(1|true|yes)$')
$SkipBuild   = ($env:SKIP_BUILD -match '^(1|true|yes)$')
$DoVlm       = $WithVlm.IsPresent -or $Full.IsPresent -or ($env:DO_VLM -match '^(1|true|yes)$')
$DoRerank    = $WithRerank.IsPresent -or $Full.IsPresent -or ($env:DO_RERANK -match '^(1|true|yes)$')
$DoMtp       = $WithMtp.IsPresent -or ($env:DO_MTP -match '^(1|true|yes)$')

$Results = [System.Collections.Generic.List[object]]::new()
$Pulled = @{
    $script:EmbedModel  = $false
    $script:ChatModel   = $false
    $script:VlmModel    = $false
    $script:RerankModel = $false
    $script:MtpModel    = $false
}
$DaemonStartedByUs = $false
$DaemonProc = $null
$Bin = $null

function Ok($m)   { Write-Host "  [+] $m" -ForegroundColor Green }
function Info($m) { Write-Host "  [*] $m" -ForegroundColor Cyan }
function Fail($m) { Write-Host "  [x] $m" -ForegroundColor Red; throw $m }
function Record($Id, $Status, $Desc, $Detail) {
    $Results.Add([pscustomobject]@{ Id = $Id; Status = $Status; Desc = $Desc; Detail = $Detail })
}

try {
    Write-Host ""
    Write-Host "  LMForge Multi-Model E2E (Windows)" -ForegroundColor White
    Write-Host "  chat=$($script:ChatModel)  embed=$($script:EmbedModel)  burst=$N"
    if ($DoVlm -or $DoRerank -or $DoMtp) {
        Write-Host "  optional: vlm=$DoVlm rerank=$DoRerank mtp=$DoMtp"
    }
    Write-Host ""

    if ($SkipBuild) {
        $Bin = Resolve-E2eBin -RepoRoot $RepoRoot
        if (-not $Bin) { Fail "SKIP_BUILD=1 but no lmforge binary found" }
        Ok "Using binary: $Bin"
    } else {
        Info "Building lmforge (release)..."
        Push-Location $RepoRoot
        cargo build --release --bin lmforge 2>&1 | Select-Object -Last 3
        Pop-Location
        $Bin = Resolve-E2eBin -RepoRoot $RepoRoot
        if (-not $Bin) { Fail "build finished but binary not found" }
        Ok "Build complete -> $Bin"
    }

    if (-not $SkipPull) {
        Info "Pulling models..."
        foreach ($pair in @(
            @($script:EmbedModel, "embed"),
            @($script:ChatModel, "chat")
        )) {
            $ref = [ref]$Pulled[$pair[0]]
            Ok "$(Pull-E2eModelIfNeeded -Bin $Bin -Model $pair[0] -PulledFlag $ref) $($pair[0])"
        }
        if ($DoVlm) {
            $ref = [ref]$Pulled[$script:VlmModel]
            Ok "$(Pull-E2eModelIfNeeded -Bin $Bin -Model $script:VlmModel -PulledFlag $ref) $($script:VlmModel)"
        }
        if ($DoRerank) {
            $ref = [ref]$Pulled[$script:RerankModel]
            Ok "$(Pull-E2eModelIfNeeded -Bin $Bin -Model $script:RerankModel -PulledFlag $ref) $($script:RerankModel)"
        }
        if ($DoMtp) {
            $ref = [ref]$Pulled[$script:MtpModel]
            Ok "$(Pull-E2eModelIfNeeded -Bin $Bin -Model $script:MtpModel -PulledFlag $ref) $($script:MtpModel)"
        }
    }

    if (-not $SkipStart) {
        Info "Starting daemon..."
        $DaemonProc = Start-Process -FilePath $Bin -ArgumentList "start" -WindowStyle Hidden -PassThru
        $DaemonStartedByUs = $true
        if (-not (Wait-E2eHealth -TimeoutSec 90)) { Fail "daemon not healthy within 90s" }
        Ok "Daemon healthy"
    } else {
        if (-not (Test-E2eHealth)) { Fail "SKIP_START=1 but daemon not healthy" }
        Ok "Using running daemon"
    }

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $r = Invoke-E2eEmbed "what is natural language processing?"
    $sw.Stop(); Assert-E2eEmbed $r "TC-E01 embed"
    $sw.Restart(); $r = Invoke-E2eChat "Say hello in one word."
    $sw.Stop(); Assert-E2eChat $r "TC-E01 chat"
    $status = Get-E2eStatus
    foreach ($m in @($script:EmbedModel, $script:ChatModel)) {
        if (-not ($status.running_models | Where-Object { $_.model_id -eq $m })) { Fail "TC-E01: $m not in running_models" }
    }
    Record "TC-E01" "PASS" "Cold-start co-load" "embed=$($sw.ElapsedMilliseconds)ms"

    for ($i = 1; $i -le $N; $i++) {
        $sw.Restart(); $r = Invoke-E2eEmbed "sequential embed $i"; $sw.Stop()
        Assert-E2eEmbed $r "TC-E02 req $i"
    }
    Record "TC-E02" "PASS" "Sequential embed ($N)" "ok"

    for ($i = 1; $i -le $N; $i++) {
        $sw.Restart(); $r = Invoke-E2eChat "What is 1 + $i? Answer with only the number."; $sw.Stop()
        Assert-E2eChat $r "TC-E03 req $i"
    }
    Record "TC-E03" "PASS" "Sequential chat ($N)" "ok"

    $sw.Restart()
    $jobs = 1..$N | ForEach-Object {
        $n = $_
        Start-Job { param($h,$m,$n) Invoke-RestMethod -Uri "$h/v1/embeddings" -Method Post -Body (@{model=$m;input="concurrent $n"}|ConvertTo-Json -Compress) -ContentType "application/json" } -ArgumentList $script:LfHost, $script:EmbedModel, $n
    }
    $jobs | Wait-Job | Out-Null; $sw.Stop()
    foreach ($j in $jobs) { Assert-E2eEmbed (Receive-Job $j) "TC-E04"; Remove-Job $j }
    Record "TC-E04" "PASS" "Concurrent embed ($N)" "wall=$($sw.ElapsedMilliseconds)ms"

    $sw.Restart()
    $j1 = Start-Job { param($h,$m) Invoke-RestMethod -Uri "$h/v1/embeddings" -Method Post -Body (@{model=$m;input="mixed"}|ConvertTo-Json -Compress) -ContentType "application/json" } -ArgumentList $script:LfHost, $script:EmbedModel
    $j2 = Start-Job { param($h,$m) Invoke-RestMethod -Uri "$h/v1/chat/completions" -Method Post -Body (@{model=$m;messages=@(@{role="user";content="Say concurrent"});stream=$false;max_tokens=32;chat_template_kwargs=@{enable_thinking=$false}}|ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" } -ArgumentList $script:LfHost, $script:ChatModel
    Wait-Job $j1,$j2 | Out-Null; $sw.Stop()
    Assert-E2eEmbed (Receive-Job $j1) "TC-E05 embed"
    Assert-E2eChat (Receive-Job $j2) "TC-E05 chat"
    Remove-Job $j1,$j2
    Record "TC-E05" "PASS" "Simultaneous embed+chat" "wall=$($sw.ElapsedMilliseconds)ms"

    $codeE = Get-E2eHttpPostCode -Path "/v1/chat/completions" -BodyJson (@{model=$script:EmbedModel;messages=@(@{role="user";content="hi"});stream=$false}|ConvertTo-Json -Compress)
    $codeC = Get-E2eHttpPostCode -Path "/v1/embeddings" -BodyJson (@{model=$script:ChatModel;input="test"}|ConvertTo-Json -Compress)
    if ($codeE -ne 400 -or $codeC -ne 400) { Fail "TC-E06: expected 400/400 got $codeE/$codeC" }
    Record "TC-E06" "PASS" "Cross-endpoint rejection" "embed@chat=$codeE chat@embed=$codeC"

    $status = Get-E2eStatus
    foreach ($m in @($script:EmbedModel, $script:ChatModel)) {
        $slot = $status.running_models | Where-Object { $_.model_id -eq $m } | Select-Object -First 1
        if (-not $slot -or $slot.status -ne "ready") { Fail "TC-E07: $m not ready" }
    }
    Record "TC-E07" "PASS" "State consistency" "both ready"

    if ($DoVlm) {
        $sw.Restart(); $r = Invoke-E2eChat "Say okay in one word." -Model $script:VlmModel; $sw.Stop()
        Assert-E2eChat $r "TC-E08"; Record "TC-E08" "PASS" "VLM text-only" "$($sw.ElapsedMilliseconds)ms"
        $sw.Restart(); $r = Invoke-E2eVlmImageRemote -Model $script:VlmModel; $sw.Stop()
        Assert-E2eChat $r "TC-E09" 20; Record "TC-E09" "PASS" "VLM image_url (remote)" "$($sw.ElapsedMilliseconds)ms"
        $sw.Restart(); $r = Invoke-E2eVlmImageBase64 -Model $script:VlmModel; $sw.Stop()
        Assert-E2eChat $r "TC-E10"; Record "TC-E10" "PASS" "VLM image_url (base64)" "$($sw.ElapsedMilliseconds)ms"
    }

    if ($DoRerank) {
        if (-not (Test-E2eEngineSupportsRerank)) { Fail "TC-E11: engine lacks reranking" }
        $sw.Restart(); $r = Invoke-E2eRerank -Model $script:RerankModel; $sw.Stop()
        if (@($r.results).Count -lt 1) { Fail "TC-E11: empty rerank results" }
        Record "TC-E11" "PASS" "Rerank endpoint" "$($sw.ElapsedMilliseconds)ms"
    }

    if ($DoMtp) {
        Invoke-E2eMtpWarm -Model $script:MtpModel
        Start-Sleep -Seconds 2
        $slot = (Get-E2eStatus).running_models | Where-Object { $_.model_id -eq $script:MtpModel } | Select-Object -First 1
        if ($slot.spec_mode -eq "mtp" -or [int]$slot.spec_stats.samples -ge 1) {
            Record "TC-E12" "PASS" "MTP speculative" "mode=$($slot.spec_mode)"
        } else { Fail "TC-E12: MTP not active" }
    }

    Write-Host ""; Write-Host "========== SUMMARY ==========" -ForegroundColor White
    $fail = 0
    foreach ($row in $Results) {
        $line = "$($row.Id)  $($row.Status)  $($row.Desc)  $($row.Detail)"
        if ($row.Status -eq "PASS") { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red; $fail++ }
    }
    if ($fail -gt 0) { exit 1 }
}
finally {
    if ($Bin) {
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            if ($DaemonStartedByUs -and -not $SkipStart) {
                & $Bin stop 2>&1 | Out-Null
                if ($DaemonProc) { Stop-Process -Id $DaemonProc.Id -Force -ErrorAction SilentlyContinue }
            }
            Remove-E2ePulledModels -Bin $Bin -PulledMap $Pulled
        } finally {
            $ErrorActionPreference = $prevEap
        }
    }
}
