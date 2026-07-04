# multi_model_e2e report

- when: 20260704_024916
- machine: **linux-x86_64**
- build: lmforge 0.1.5 (52c21a6 2026-07-03) | checkout: 52c21a6
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=1 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=1730ms  chat=2362ms |
| TC-E02 | PASS | Sequential embed (10x) | min=14ms  avg=16ms  p50=15ms  max=29ms |
| TC-E03 | PASS | Sequential chat (10x) | min=634ms  avg=638ms  p50=639ms  max=644ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=174ms  throughput=57 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=647ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=0s  chat: status=ready idle=0s   |
| TC-E08 | PASS | VLM text-only | 2215ms |
| TC-E09 | PASS | VLM image_url (remote) | 1153ms |
| TC-E10 | PASS | VLM image_url (base64) | 454ms |
| TC-E11 | PASS | Rerank endpoint | 1878ms count=3 |
| TC-E12 | PASS | MTP speculative | mode=mtp samples=1 |
| TC-E14 | PASS | Thinking off non-blank | 1959ms |
| TC-E13 | FAIL | Thinking on reasoning+answer | reasoning='0' answer='249' (expected both non-empty) |
| TC-E15 | PASS | Thinking budget answer | 2086ms a=559 |
