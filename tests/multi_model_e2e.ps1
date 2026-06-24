# =============================================================================
# LMForge — Multi-Model E2E Integration Test (Windows)
# All capability suites (embed/chat/VLM/rerank/MTP) on by default; SKIP on unavailable.
# =============================================================================
param(
    [switch]$Full,
    [switch]$SkipVlm,
    [switch]$SkipRerank,
    [switch]$SkipMtp,
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

$DoVlm    = (Test-E2eSuiteEnabled "DO_VLM")    -and -not $SkipVlm.IsPresent
$DoRerank = (Test-E2eSuiteEnabled "DO_RERANK") -and -not $SkipRerank.IsPresent
$DoMtp    = (Test-E2eSuiteEnabled "DO_MTP")    -and -not $SkipMtp.IsPresent
if ($WithVlm.IsPresent -or $Full.IsPresent)    { $DoVlm = $true }
if ($WithRerank.IsPresent -or $Full.IsPresent) { $DoRerank = $true }
if ($WithMtp.IsPresent -or $Full.IsPresent)    { $DoMtp = $true }

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
function Warn($m) { Write-Host "  [!] $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "  [x] $m" -ForegroundColor Red; throw $m }

function Record([string]$Id, [string]$Status, [string]$Desc, [string]$Detail) {
    $Results.Add([pscustomobject]@{ Id = $Id; Status = $Status; Desc = $Desc; Detail = $Detail })
}

function Pull-Optional([string]$Model, [ref]$PulledFlag, [ref]$Enabled) {
    if (-not $Enabled.Value) { return }
    try {
        $msg = Pull-E2eModelIfNeeded -Bin $Bin -Model $Model -PulledFlag $PulledFlag
        Ok "$msg $Model"
    } catch {
        Warn "Optional pull failed for ${Model}: $($_.Exception.Message) - skipping suite"
        $Enabled.Value = $false
    }
}

function Try-OptionalChat {
    param([string]$Id, [string]$Desc, [scriptblock]$Action, [int]$MinLen = 20, [scriptblock]$Diag)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $r = & $Action
        $sw.Stop()
        Assert-E2eChat $r $Id $MinLen
        Record $Id "PASS" $Desc "$($sw.ElapsedMilliseconds)ms"
        Ok "$Id $Desc $($sw.ElapsedMilliseconds)ms"
    } catch {
        $sw.Stop()
        # Re-issue without -f to see the real status/body. A 5xx = engine crashed
        # on a valid request (FAIL, don't mask as unsupported); else capability
        # gap (SKIP). Falls back to the raw exception when no diag block given.
        $detail = if ($Diag) { & $Diag } else { $_.Exception.Message }
        if ((Get-E2eDiagClass $detail) -eq "fail") {
            Warn "$Id engine error - $detail"
            Record $Id "FAIL" $Desc $detail
        } else {
            Warn "$Id skipped: $detail"
            Record $Id "SKIP" $Desc $detail
        }
    }
}

try {
    Write-Host ""
    Write-Host "  LMForge Multi-Model E2E (Windows)" -ForegroundColor White
    Write-Host "  chat=$($script:ChatModel)  embed=$($script:EmbedModel)  burst=$N"
    Write-Host "  suites: vlm=$DoVlm rerank=$DoRerank mtp=$DoMtp  chat_max_tokens=$E2E_CHAT_MAX_TOKENS"
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
            @($script:EmbedModel, [ref]$Pulled[$script:EmbedModel]),
            @($script:ChatModel,  [ref]$Pulled[$script:ChatModel])
        )) {
            $msg = Pull-E2eModelIfNeeded -Bin $Bin -Model $pair[0] -PulledFlag $pair[1]
            Ok "$msg $($pair[0])"
        }
        Pull-Optional $script:VlmModel    ([ref]$Pulled[$script:VlmModel])    ([ref]$DoVlm)
        Pull-Optional $script:RerankModel ([ref]$Pulled[$script:RerankModel]) ([ref]$DoRerank)
        Pull-Optional $script:MtpModel    ([ref]$Pulled[$script:MtpModel])    ([ref]$DoMtp)
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
    try { $r = Invoke-E2eEmbed -Text $E2E_EMBED_COLD }
    catch { Fail "TC-E01: embed cold-load failed — $(Get-E2eEmbedDiag -Model $script:EmbedModel -Text $E2E_EMBED_COLD)" }
    $sw.Stop(); Assert-E2eEmbed $r "TC-E01 embed"
    $embedColdMs = $sw.ElapsedMilliseconds
    $sw.Restart()
    try { $r = Invoke-E2eChat -Text $E2E_CHAT_COLD -MaxTokens $E2E_CHAT_MAX_TOKENS }
    catch { Fail "TC-E01: chat cold-load failed — $(Get-E2eChatDiag -Model $script:ChatModel -Text $E2E_CHAT_COLD)" }
    $sw.Stop(); Assert-E2eChat $r "TC-E01 chat" 20
    $chatColdMs = $sw.ElapsedMilliseconds
    $status = Get-E2eStatus
    foreach ($m in @($script:EmbedModel, $script:ChatModel)) {
        if (-not ($status.running_models | Where-Object { $_.model_id -eq $m })) { Fail "TC-E01: $m not in running_models" }
    }
    Record "TC-E01" "PASS" "Cold-start co-load" "embed=${embedColdMs}ms chat=${chatColdMs}ms"

    for ($i = 1; $i -le $N; $i++) {
        $sw.Restart()
        $r = Invoke-E2eEmbed -Text (Get-E2eBurstEmbedText $i $N)
        $sw.Stop()
        Assert-E2eEmbed $r "TC-E02 req $i"
    }
    Record "TC-E02" "PASS" "Sequential embed ($N)" "ok"

    for ($i = 1; $i -le $N; $i++) {
        $sw.Restart()
        $r = Invoke-E2eChat -Text (Get-E2eBurstChatText $i $N) -MaxTokens $E2E_CHAT_MAX_TOKENS
        $sw.Stop()
        Assert-E2eChat $r "TC-E03 req $i" 15
    }
    Record "TC-E03" "PASS" "Sequential chat ($N)" "ok"

    $sw.Restart()
    $jobs = 1..$N | ForEach-Object {
        $n = $_
        $text = Get-E2eBurstEmbedText $n $N
        Start-Job {
            param($h, $m, $t)
            Invoke-RestMethod -Uri "$h/v1/embeddings" -Method Post `
                -Body (@{ model = $m; input = $t } | ConvertTo-Json -Compress) `
                -ContentType "application/json" -TimeoutSec 180
        } -ArgumentList $script:LfHost, $script:EmbedModel, $text
    }
    $jobs | Wait-Job | Out-Null
    $sw.Stop()
    foreach ($j in $jobs) { Assert-E2eEmbed (Receive-Job $j) "TC-E04"; Remove-Job $j }
    Record "TC-E04" "PASS" "Concurrent embed ($N)" "wall=$($sw.ElapsedMilliseconds)ms"

    $sw.Restart()
    $chatBody = @{
        model = $script:ChatModel
        messages = @(@{ role = "user"; content = $E2E_CHAT_MIXED })
        stream = $false
        max_tokens = $E2E_CHAT_MAX_TOKENS
        temperature = 0
        chat_template_kwargs = @{ enable_thinking = $false }
    } | ConvertTo-Json -Depth 8 -Compress
    $j1 = Start-Job {
        param($h, $m, $t)
        Invoke-RestMethod -Uri "$h/v1/embeddings" -Method Post `
            -Body (@{ model = $m; input = $t } | ConvertTo-Json -Compress) `
            -ContentType "application/json" -TimeoutSec 180
    } -ArgumentList $script:LfHost, $script:EmbedModel, $E2E_EMBED_MIXED
    $j2 = Start-Job {
        param($h, $body)
        Invoke-RestMethod -Uri "$h/v1/chat/completions" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 180
    } -ArgumentList $script:LfHost, $chatBody
    Wait-Job $j1, $j2 | Out-Null
    $sw.Stop()
    Assert-E2eEmbed (Receive-Job $j1) "TC-E05 embed"
    Assert-E2eChat (Receive-Job $j2) "TC-E05 chat" 20
    Remove-Job $j1, $j2
    Record "TC-E05" "PASS" "Simultaneous embed+chat" "wall=$($sw.ElapsedMilliseconds)ms"

    $codeE = Get-E2eHttpPostCode -Path "/v1/chat/completions" -BodyJson (@{ model = $script:EmbedModel; messages = @(@{ role = "user"; content = "hi" }); stream = $false } | ConvertTo-Json -Compress)
    $codeC = Get-E2eHttpPostCode -Path "/v1/embeddings" -BodyJson (@{ model = $script:ChatModel; input = "test" } | ConvertTo-Json -Compress)
    if ($codeE -ne 400 -or $codeC -ne 400) { Fail "TC-E06: expected 400/400 got $codeE/$codeC" }
    Record "TC-E06" "PASS" "Cross-endpoint rejection" "embed@chat=$codeE chat@embed=$codeC"

    $status = Get-E2eStatus
    foreach ($m in @($script:EmbedModel, $script:ChatModel)) {
        $slot = $status.running_models | Where-Object { $_.model_id -eq $m } | Select-Object -First 1
        if (-not $slot -or $slot.status -ne "ready") { Fail "TC-E07: $m not ready" }
    }
    Record "TC-E07" "PASS" "State consistency" "both ready"

    if ($DoVlm) {
        Try-OptionalChat "TC-E08" "VLM text-only" { Invoke-E2eVlmText -Model $script:VlmModel } 20 { Get-E2eChatDiag -Model $script:VlmModel -Text $E2E_VLM_TEXT }
        Try-OptionalChat "TC-E09" "VLM image_url (remote)" { Invoke-E2eVlmImageRemote -Model $script:VlmModel } 30 { Get-E2eVlmRemoteDiag -Model $script:VlmModel }
        Try-OptionalChat "TC-E10" "VLM image_url (base64)" { Invoke-E2eVlmImageBase64 -Model $script:VlmModel } 15 { Get-E2eVlmBase64Diag -Model $script:VlmModel }
    }

    if ($DoRerank) {
        if (-not (Test-E2eEngineSupportsRerank)) {
            Warn "TC-E11: engine lacks reranking - skipping"
            Record "TC-E11" "SKIP" "Rerank endpoint" "engine lacks reranking"
        } else {
            $sw = [System.Diagnostics.Stopwatch]::StartNew()
            try {
                $r = Invoke-E2eRerank -Model $script:RerankModel
                $sw.Stop()
                if (@($r.results).Count -lt 1) { throw "empty rerank results" }
                Record "TC-E11" "PASS" "Rerank endpoint" "$($sw.ElapsedMilliseconds)ms"
            } catch {
                $sw.Stop()
                $detail = Get-E2eRerankDiag -Model $script:RerankModel
                if ((Get-E2eDiagClass $detail) -eq "fail") {
                    Warn "TC-E11 engine error - $detail"
                    Record "TC-E11" "FAIL" "Rerank endpoint" $detail
                } else {
                    Warn "TC-E11 skipped: $detail"
                    Record "TC-E11" "SKIP" "Rerank endpoint" $detail
                }
            }
        }
    }

    if ($DoMtp) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            Invoke-E2eMtpWarm -Model $script:MtpModel | Out-Null
            $sw.Stop()
            Start-Sleep -Seconds 2
            $slot = (Get-E2eStatus).running_models | Where-Object { $_.model_id -eq $script:MtpModel } | Select-Object -First 1
            if ($slot.spec_mode -eq "mtp" -or [int]$slot.spec_stats.samples -ge 1) {
                Record "TC-E12" "PASS" "MTP speculative" "mode=$($slot.spec_mode) warm=$($sw.ElapsedMilliseconds)ms"
            } else {
                Warn "TC-E12: MTP not active - skipping"
                Record "TC-E12" "SKIP" "MTP speculative" "mode=$($slot.spec_mode)"
            }
        } catch {
            $sw.Stop()
            Warn "TC-E12 skipped: $($_.Exception.Message)"
            Record "TC-E12" "SKIP" "MTP speculative" $_.Exception.Message
        }
    }

    Write-Host ""
    Write-Host "========== SUMMARY ==========" -ForegroundColor White
    $fail = 0
    foreach ($row in $Results) {
        $line = "$($row.Id)  $($row.Status)  $($row.Desc)  $($row.Detail)"
        switch ($row.Status) {
            "PASS" { Write-Host $line -ForegroundColor Green }
            "SKIP" { Write-Host $line -ForegroundColor Yellow }
            default { Write-Host $line -ForegroundColor Red; $fail++ }
        }
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
