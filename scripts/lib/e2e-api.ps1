# Shared LMForge live-API helpers for E2E scripts (Windows) — dot-source only.
if ($script:LMForgeE2eApiLoaded) { return }
$script:LMForgeE2eApiLoaded = $true

$E2eLibDir = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $E2eLibDir "e2e-defaults.ps1")

function Initialize-E2eModels {
    if (-not $env:CHAT_MODEL)   { $script:ChatModel   = $E2E_CHAT_MODEL }   else { $script:ChatModel   = $env:CHAT_MODEL }
    if (-not $env:EMBED_MODEL)  { $script:EmbedModel  = $E2E_EMBED_MODEL }  else { $script:EmbedModel  = $env:EMBED_MODEL }
    if (-not $env:VLM_MODEL)    { $script:VlmModel    = $E2E_VLM_MODEL }    else { $script:VlmModel    = $env:VLM_MODEL }
    if (-not $env:RERANK_MODEL) { $script:RerankModel = $E2E_RERANK_MODEL } else { $script:RerankModel = $env:RERANK_MODEL }
    if (-not $env:MTP_MODEL)    { $script:MtpModel    = $E2E_MTP_MODEL }    else { $script:MtpModel    = $env:MTP_MODEL }
    if (-not $env:LF_HOST)      { $script:LfHost      = "http://127.0.0.1:11430" } else { $script:LfHost = $env:LF_HOST }
}
Initialize-E2eModels

function Test-E2eHealth {
    param([string]$HostUrl = $script:LfHost)
    try {
        Invoke-WebRequest -Uri "$HostUrl/health" -UseBasicParsing -TimeoutSec 3 | Out-Null
        return $true
    } catch { return $false }
}

function Wait-E2eHealth {
    param([int]$TimeoutSec = 90, [string]$HostUrl = $script:LfHost)
    for ($i = 1; $i -le $TimeoutSec; $i++) {
        if (Test-E2eHealth -HostUrl $HostUrl) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

# Poll until a resident slot reports status=ready (handles post-burst warm-up).
function Wait-E2eSlotReady {
    param([string]$ModelId, [int]$TimeoutSec = 90, [string]$HostUrl = $script:LfHost)
    for ($i = 1; $i -le $TimeoutSec; $i++) {
        try {
            $slot = (Invoke-RestMethod -Uri "$HostUrl/lf/status" -TimeoutSec 10).running_models |
                Where-Object { $_.model_id -eq $ModelId } | Select-Object -First 1
            if (-not $slot) { return $false }
            if ($slot.status -eq "ready") { return $true }
        } catch {}
        Start-Sleep -Seconds 1
    }
    return $false
}

function Resolve-E2eBin {
    param([string]$RepoRoot)
    $candidates = @(
        $env:LF_BIN,
        (Join-Path $RepoRoot "target\debug\lmforge.exe"),
        (Join-Path $RepoRoot "target\release\lmforge.exe"),
        "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe"
    ) | Where-Object { $_ }
    foreach ($c in $candidates) {
        if (Test-Path $c) { return (Resolve-Path $c).Path }
    }
    $cmd = Get-Command lmforge -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

# `lmforge pull` prints a native indicatif progress bar to STDERR and status
# lines ("already installed", …) to STDOUT. Redirect only STDOUT to a temp file
# (for the "already installed" probe); leave STDERR attached to the console with
# -NoNewWindow so the real bar renders live instead of being swallowed.
function Pull-E2eModelIfNeeded {
    param([string]$Bin, [string]$Model, [ref]$PulledFlag, [int]$Retries = 3)
    # Retry transient HF download failures (connection reset, body decode errors)
    # with backoff so a flaky network doesn't fail the whole gate. A non-network
    # failure (e.g. bad model id) still surfaces after the attempts are exhausted.
    $attempt = 0
    while ($true) {
        $attempt++
        $outFile = New-TemporaryFile
        try {
            $p = Start-Process -FilePath $Bin -ArgumentList @("pull", $Model) -NoNewWindow -PassThru `
                -RedirectStandardOutput $outFile.FullName
            # Cache the OS handle BEFORE waiting; without this the .NET Process object
            # releases the handle on exit and $p.ExitCode reads back $null, so a
            # successful pull (exit 0) is misreported as a failure.
            $null = $p.Handle
            $p.WaitForExit()
            $out = (Get-Content $outFile.FullName -Raw -EA SilentlyContinue)
            if ($p.ExitCode -eq 0) {
                if ($out -match 'already installed') { return "already present" }
                $PulledFlag.Value = $true
                return "downloaded"
            }
            if ($attempt -le $Retries) {
                $wait = [Math]::Min(30, [Math]::Pow(2, $attempt))
                Write-Host "  [!] pull $Model failed (exit $($p.ExitCode)), retry $attempt/$Retries in ${wait}s..." -ForegroundColor Yellow
                Start-Sleep -Seconds $wait
                continue
            }
            throw "pull $Model failed (exit $($p.ExitCode)) after $Retries retries: $out"
        } finally {
            Remove-Item $outFile.FullName -EA SilentlyContinue
        }
    }
}

function Get-E2eStatus {
    param([string]$HostUrl = $script:LfHost)
    Invoke-RestMethod -Uri "$HostUrl/lf/status" -TimeoutSec 15
}

function Test-E2eEngineSupportsRerank {
    param([string]$HostUrl = $script:LfHost)
    $eng = (Invoke-RestMethod "$HostUrl/lf/engines").engines | Where-Object { $_.active } | Select-Object -First 1
    return ($eng.supports_reranking -eq $true)
}

function Test-E2eModelThinkingCapable {
    param([string]$Model = $script:ChatModel, [string]$HostUrl = $script:LfHost)
    try {
        $m = (Invoke-RestMethod "$HostUrl/lf/model/list").models | Where-Object { $_.id -eq $Model } | Select-Object -First 1
        return ($m.capabilities.thinking -eq $true)
    } catch { return $false }
}

# think=true (top-level), non-streaming → expect reasoning_content + answer.
function Invoke-E2eChatThinkOn {
    param([string]$Text, [string]$Model = $script:ChatModel, [int]$MaxTokens = 512, [string]$HostUrl = $script:LfHost)
    $body = @{
        model = $Model
        messages = @(@{ role = "user"; content = $Text })
        stream = $false; max_tokens = $MaxTokens; temperature = 0.6; think = $true
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 180
}

# think=true + thinking_budget → orchestrator → expect answer.
function Invoke-E2eChatThinkBudget {
    param([string]$Text, [string]$Model = $script:ChatModel, [int]$MaxTokens = 512, [int]$Budget = 256, [string]$HostUrl = $script:LfHost)
    $body = @{
        model = $Model
        messages = @(@{ role = "user"; content = $Text })
        stream = $false; max_tokens = $MaxTokens; temperature = 0.6; think = $true; thinking_budget = $Budget
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eEmbed {
    param([string]$Text, [string]$Model = $script:EmbedModel, [string]$HostUrl = $script:LfHost)
    $body = @{ model = $Model; input = $Text } | ConvertTo-Json -Compress
    Invoke-RestMethod -Uri "$HostUrl/v1/embeddings" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eChat {
    param(
        [string]$Text,
        [string]$Model = $script:ChatModel,
        [int]$MaxTokens = $E2E_CHAT_MAX_TOKENS,
        [string]$HostUrl = $script:LfHost,
        [switch]$DisableThinking
    )
    $body = @{
        model = $Model
        messages = @(@{ role = "user"; content = $Text })
        stream = $false
        max_tokens = $MaxTokens
        temperature = 0
    }
    if ($DisableThinking -or $Model -match 'qwen3') {
        $body.chat_template_kwargs = @{ enable_thinking = $false }
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eVlmText {
    param(
        [string]$Model = $script:VlmModel,
        [string]$Text = $E2E_VLM_TEXT,
        [int]$MaxTokens = $E2E_VLM_TEXT_MAX_TOKENS,
        [string]$HostUrl = $script:LfHost
    )
    Invoke-E2eChat -Text $Text -Model $Model -MaxTokens $MaxTokens -HostUrl $HostUrl -DisableThinking
}

function Invoke-E2eVlmImageRemote {
    param(
        [string]$Model = $script:VlmModel,
        [string]$ImageUrl = $E2E_VLM_IMAGE_URL,
        [int]$MaxTokens = $E2E_VLM_IMAGE_MAX_TOKENS,
        [string]$HostUrl = $script:LfHost
    )
    $body = @{
        model = $Model
        messages = @(@{
            role = "user"
            content = @(
                @{ type = "text"; text = $E2E_VLM_REMOTE_PROMPT }
                @{ type = "image_url"; image_url = @{ url = $ImageUrl } }
            )
        })
        max_tokens = $MaxTokens
        temperature = 0
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 240
}

function Invoke-E2eVlmImageBase64 {
    param(
        [string]$Model = $script:VlmModel,
        [int]$MaxTokens = $E2E_VLM_IMAGE_MAX_TOKENS,
        [string]$HostUrl = $script:LfHost
    )
    $body = @{
        model = $Model
        messages = @(@{
            role = "user"
            content = @(
                @{ type = "text"; text = $E2E_VLM_BASE64_PROMPT }
                @{ type = "image_url"; image_url = @{ url = "data:image/png;base64,$E2E_RED_PNG_B64" } }
            )
        })
        max_tokens = $MaxTokens
        temperature = 0
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eRerank {
    param([string]$Model = $script:RerankModel, [string]$HostUrl = $script:LfHost)
    $body = @{
        model = $Model
        query = $E2E_RERANK_QUERY
        documents = (Get-E2eRerankDocuments)
        top_n = 3
    } | ConvertTo-Json -Compress
    Invoke-RestMethod -Uri "$HostUrl/v1/rerank" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 120
}

function Invoke-E2eMtpWarm {
    param(
        [string]$Model = $script:MtpModel,
        [int]$MaxTokens = $E2E_MTP_MAX_TOKENS,
        [string]$HostUrl = $script:LfHost
    )
    $env:LMFORGE_SPECULATIVE_MODE = "auto"
    $body = @{
        model = $Model
        messages = @(@{ role = "user"; content = $E2E_MTP_WARM })
        max_tokens = $MaxTokens
        temperature = 0
        think = $false
        chat_template_kwargs = @{ enable_thinking = $false }
    } | ConvertTo-Json -Depth 6 -Compress
    try {
        return Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 240
    } finally {
        Remove-Item Env:LMFORGE_SPECULATIVE_MODE -ErrorAction SilentlyContinue
    }
}

function Assert-E2eEmbed($Resp, [string]$Label) {
    $dims = @($Resp.data[0].embedding).Count
    if ($dims -le 0) { throw "${Label}: empty embedding" }
}

function Assert-E2eChat($Resp, [string]$Label, [int]$MinLen = 1) {
    $msg = $Resp.choices[0].message
    $txt = [string]$msg.content
    if ($msg.PSObject.Properties.Name -contains 'reasoning_content') {
        $txt += [string]$msg.reasoning_content
    }
    if (-not $txt -or $txt.Trim().Length -lt $MinLen) { throw "${Label}: empty or short content" }
}

function Get-E2eHttpPostCode {
    param([string]$Path, [string]$BodyJson, [string]$HostUrl = $script:LfHost)
    try {
        Invoke-WebRequest -Uri "$HostUrl$Path" -Method Post -Body $BodyJson -ContentType "application/json" -ErrorAction Stop | Out-Null
        return 200
    } catch {
        if ($_.Exception.Response) { return [int]$_.Exception.Response.StatusCode }
        return 0
    }
}

# ── Failure diagnostics ──────────────────────────────────────────────────────
# Capture HTTP status + error body for a failed request so a cold-load failure
# isn't an opaque exception message. Used only on the failure path.
function Get-E2ePostDiag {
    param([string]$Path, [string]$BodyJson, [string]$HostUrl = $script:LfHost)
    try {
        $resp = Invoke-WebRequest -Uri "$HostUrl$Path" -Method Post -Body $BodyJson `
            -ContentType "application/json" -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
        $b = [string]$resp.Content
        if ($b.Length -gt 300) { $b = $b.Substring(0, 300) }
        return "HTTP $([int]$resp.StatusCode) — $b"
    } catch {
        $code = 0
        if ($_.Exception.Response) { try { $code = [int]$_.Exception.Response.StatusCode } catch {} }
        $body = $null
        if ($_.ErrorDetails -and $_.ErrorDetails.Message) {
            $body = $_.ErrorDetails.Message              # PowerShell 7 puts the body here
        } elseif ($_.Exception.Response) {
            try {
                $sr = New-Object System.IO.StreamReader($_.Exception.Response.GetResponseStream())
                $body = $sr.ReadToEnd()
            } catch {}
        }
        if (-not $body) { $body = $_.Exception.Message }
        if ($body.Length -gt 300) { $body = $body.Substring(0, 300) }
        return "HTTP $code — $body"
    }
}

function Get-E2eEmbedDiag {
    param([string]$Model = $script:EmbedModel, [string]$Text = "probe")
    Get-E2ePostDiag "/v1/embeddings" (@{ model = $Model; input = $Text } | ConvertTo-Json -Compress)
}

function Get-E2eChatDiag {
    param([string]$Model = $script:ChatModel, [string]$Text = "probe")
    $b = @{ model = $Model; messages = @(@{ role = "user"; content = $Text }); stream = $false; max_tokens = 16 } |
        ConvertTo-Json -Depth 6 -Compress
    Get-E2ePostDiag "/v1/chat/completions" $b
}

function Get-E2eVlmRemoteDiag {
    param([string]$Model = $script:VlmModel, [string]$ImageUrl = $E2E_VLM_IMAGE_URL)
    $b = @{ model = $Model; max_tokens = 16; temperature = 0; messages = @(@{ role = "user"; content = @(
        @{ type = "text"; text = $E2E_VLM_REMOTE_PROMPT }
        @{ type = "image_url"; image_url = @{ url = $ImageUrl } }
    ) }) } | ConvertTo-Json -Depth 8 -Compress
    Get-E2ePostDiag "/v1/chat/completions" $b
}

function Get-E2eVlmBase64Diag {
    param([string]$Model = $script:VlmModel)
    $b = @{ model = $Model; max_tokens = 16; temperature = 0; messages = @(@{ role = "user"; content = @(
        @{ type = "text"; text = $E2E_VLM_BASE64_PROMPT }
        @{ type = "image_url"; image_url = @{ url = "data:image/png;base64,$E2E_RED_PNG_B64" } }
    ) }) } | ConvertTo-Json -Depth 8 -Compress
    Get-E2ePostDiag "/v1/chat/completions" $b
}

function Get-E2eRerankDiag {
    param([string]$Model = $script:RerankModel)
    $b = @{ model = $Model; query = $E2E_RERANK_QUERY; documents = (Get-E2eRerankDocuments); top_n = 3 } |
        ConvertTo-Json -Depth 6 -Compress
    Get-E2ePostDiag "/v1/rerank" $b
}

# "fail" for a 5xx (engine accepted the request and crashed — a real defect that
# must not hide as a missing capability); "skip" otherwise (4xx/000 = genuine
# capability gap). Input is a diag string "HTTP <code> — …".
function Get-E2eDiagClass {
    param([string]$Diag)
    if ($Diag -match 'HTTP\s+(5\d\d)\b') { return "fail" }
    return "skip"
}

function Remove-E2ePulledModels {
    param([string]$Bin, [hashtable]$PulledMap)
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        foreach ($kv in $PulledMap.GetEnumerator()) {
            if ($kv.Value) {
                Write-Host "  [*] removing $($kv.Key) (downloaded this run)" -ForegroundColor Cyan
                & $Bin models remove $kv.Key 2>&1 | Out-Null
            }
        }
    } finally {
        $ErrorActionPreference = $prevEap
    }
}
