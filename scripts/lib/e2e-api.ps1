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

function Pull-E2eModelIfNeeded {
    param([string]$Bin, [string]$Model, [ref]$PulledFlag)
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        # lmforge logs to stderr; must not treat INFO lines as terminating errors.
        $out = & $Bin pull $Model 2>&1 | ForEach-Object { "$_" } | Out-String
        if ($LASTEXITCODE -ne 0) { throw "pull $Model failed: $out" }
        if ($out -match 'already installed') { return "already present" }
        $PulledFlag.Value = $true
        return "downloaded"
    } finally {
        $ErrorActionPreference = $prevEap
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

function Invoke-E2eEmbed {
    param([string]$Text, [string]$Model = $script:EmbedModel, [string]$HostUrl = $script:LfHost)
    $body = @{ model = $Model; input = $Text } | ConvertTo-Json -Compress
    Invoke-RestMethod -Uri "$HostUrl/v1/embeddings" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eChat {
    param(
        [string]$Text,
        [string]$Model = $script:ChatModel,
        [int]$MaxTokens = 64,
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

function Invoke-E2eVlmImageRemote {
    param(
        [string]$Model = $script:VlmModel,
        [string]$ImageUrl = $E2E_VLM_IMAGE_URL,
        [string]$HostUrl = $script:LfHost
    )
    $body = @{
        model = $Model
        messages = @(@{
            role = "user"
            content = @(
                @{ type = "text"; text = "Describe in less than 50 words but more than 35 words." }
                @{ type = "image_url"; image_url = @{ url = $ImageUrl } }
            )
        })
        max_tokens = 96
        temperature = 0
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 240
}

function Invoke-E2eVlmImageBase64 {
    param([string]$Model = $script:VlmModel, [string]$HostUrl = $script:LfHost)
    $body = @{
        model = $Model
        messages = @(@{
            role = "user"
            content = @(
                @{ type = "text"; text = "What color? One word." }
                @{ type = "image_url"; image_url = @{ url = "data:image/png;base64,$E2E_RED_PNG_B64" } }
            )
        })
        max_tokens = 16
        temperature = 0
    }
    Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post `
        -Body ($body | ConvertTo-Json -Depth 8 -Compress) -ContentType "application/json" -TimeoutSec 180
}

function Invoke-E2eRerank {
    param([string]$Model = $script:RerankModel, [string]$HostUrl = $script:LfHost)
    $body = @{
        model = $Model
        query = "What is Python?"
        documents = @("Python is a language.", "The sky is blue.")
        top_n = 2
    } | ConvertTo-Json -Compress
    Invoke-RestMethod -Uri "$HostUrl/v1/rerank" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 120
}

function Invoke-E2eMtpWarm {
    param([string]$Model = $script:MtpModel, [string]$HostUrl = $script:LfHost)
    $env:LMFORGE_SPECULATIVE_MODE = "auto"
    $body = @{
        model = $Model
        messages = @(@{ role = "user"; content = "Count slowly to five." })
        max_tokens = 96
        temperature = 0
        think = $false
    } | ConvertTo-Json -Depth 6 -Compress
    try {
        Invoke-RestMethod -Uri "$HostUrl/v1/chat/completions" -Method Post -Body $body -ContentType "application/json" -TimeoutSec 180 | Out-Null
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
