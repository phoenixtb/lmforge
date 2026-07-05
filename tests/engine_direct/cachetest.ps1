param([int]$CacheRam = 1024, [switch]$NoCheckpoints)

$exe   = "$env:USERPROFILE\.lmforge\engines\llama-server.exe"
$model = "$env:USERPROFILE\.lmforge\models\qwen3.5-2b-gguf\Qwen3.5-2B-UD-Q4_K_XL.gguf"
$sargs = @("-m", $model, "--port", "8090", "-ngl", "99")
if ($CacheRam -gt 0) { $sargs += @("--cache-ram", "$CacheRam") }
if ($NoCheckpoints)  { $sargs += @("--ctx-checkpoints", "0") }
Write-Host "server args: $($sargs -join ' ')"

$p = Start-Process -FilePath $exe -ArgumentList $sargs -PassThru -WindowStyle Hidden
do { Start-Sleep -Milliseconds 800
     try { $h = Invoke-RestMethod http://127.0.0.1:8090/health -TimeoutSec 2 } catch { $h = $null }
} until ($h -or $p.HasExited)
if ($p.HasExited) { Write-Host "ENGINE DIED AT STARTUP"; exit 1 }

$prompts = @(
  "What is 2+2?", "Name three primary colors.", "What is the capital of France?",
  "A bat and ball cost 1.10 total, bat costs 1 more than ball. Ball price?",
  "How many r letters in strawberry?", "What is 17 times 3?",
  "What is 2+2?", "Next number: 2, 4, 8, 16?", "Name three primary colors.",
  "What is 9 squared?", "A bat and ball cost 1.10 total, bat costs 1 more than ball. Ball price?",
  "What is the capital of France?"
)
$i = 0
foreach ($q in $prompts) {
    $i++
    $body = @{ messages = @(@{ role = "user"; content = $q }); max_tokens = 400; temperature = 0 } | ConvertTo-Json -Depth 5
    try {
        $r = Invoke-RestMethod -Uri http://127.0.0.1:8090/v1/chat/completions -Method Post -ContentType "application/json" -Body $body
        $msg  = $r.choices[0].message
        $c    = "$($msg.content)"
        $rc   = "$($msg.reasoning_content)"
        $all  = ($rc + " " + $c)
        $bad  = ($all -match '([^\s]{2,10})(\s*\1){6,}')
        $empty = ($c.Trim().Length -eq 0 -and $rc.Trim().Length -eq 0)
        $verdict = if ($bad) { "CORRUPT" } elseif ($empty) { "EMPTY  " } else { "ok     " }
        $head = (($(if ($c.Trim()) { $c } else { $rc })) -replace "`r?`n", " ").Trim()
        if ($head.Length -gt 70) { $head = $head.Substring(0, 70) }
        Write-Host ("{0,2}: {1} fin={2} c={3,4} r={4,5}  {5}" -f $i, $verdict, $r.choices[0].finish_reason, $c.Length, $rc.Length, $head)
    } catch { Write-Host ("{0,2}: REQUEST FAILED: {1}" -f $i, $_.Exception.Message) }
}
Stop-Process -Id $p.Id -Force