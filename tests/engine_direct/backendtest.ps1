param([string]$Exe = "$env:USERPROFILE\.lmforge\engines\llama-server.exe")

$model  = "$env:USERPROFILE\.lmforge\models\qwen3.5-2b-gguf\Qwen3.5-2B-UD-Q4_K_XL.gguf"
$sargs  = @("-m", $model, "--port", "8090", "-ngl", "99", "--ctx-checkpoints", "0")
$errLog = "$env:TEMP\llama-stderr-$PID.txt"
Remove-Item $errLog -ErrorAction SilentlyContinue

Write-Host "exe: $Exe"
$p = Start-Process -FilePath $Exe -ArgumentList $sargs -PassThru -WindowStyle Hidden -RedirectStandardError $errLog
do { Start-Sleep -Milliseconds 800
     try { $h = Invoke-RestMethod http://127.0.0.1:8090/health -TimeoutSec 2 } catch { $h = $null }
} until ($h -or $p.HasExited)

Write-Host "---- backend detection (from server log) ----"
Select-String -Path $errLog -Pattern "CUDA|Vulkan|vulkan|using device|offloaded" |
    Select-Object -First 8 | ForEach-Object { Write-Host "  $($_.Line)" }
Write-Host "----------------------------------------------"
if ($p.HasExited) { Write-Host "ENGINE DIED — full log tail:"; Get-Content $errLog -Tail 15; exit 1 }

$prompts = @(
  "What is 2+2?", "Name three primary colors.", "What is the capital of France?",
  "A bat and ball cost 1.10 total, bat costs 1 more than ball. Ball price?",
  "How many r letters in strawberry?", "What is 17 times 3?"
)
$i = 0
foreach ($q in $prompts) {
    $i++
    $body = @{ messages = @(@{ role = "user"; content = $q }); max_tokens = 400; temperature = 0 } | ConvertTo-Json -Depth 5
    try {
        $r   = Invoke-RestMethod -Uri http://127.0.0.1:8090/v1/chat/completions -Method Post -ContentType "application/json" -Body $body
        $msg = $r.choices[0].message
        $c   = "$($msg.content)"; $rc = "$($msg.reasoning_content)"
        $bad = (($rc + " " + $c) -match '([^\s]{2,10})(\s*\1){6,}')
        $tps = [math]::Round($r.timings.predicted_per_second, 0)
        $head = (($(if ($c.Trim()) { $c } else { $rc })) -replace "`r?`n", " ").Trim()
        if ($head.Length -gt 60) { $head = $head.Substring(0, 60) }
        Write-Host ("{0}: {1} {2,4} t/s  {3}" -f $i, $(if ($bad) { "CORRUPT" } else { "ok     " }), $tps, $head)
    } catch { Write-Host ("{0}: REQUEST FAILED: {1}" -f $i, $_.Exception.Message) }
}
Stop-Process -Id $p.Id -Force