# multi_model_e2e report

- when: 20260704_081927
- machine: **darwin-arm64**
- build: lmforge 0.1.5 (0d045c8-dirty 2026-07-04) | checkout: 0d045c8
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=0 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=2733ms  chat=4087ms |
| TC-E02 | PASS | Sequential embed (10x) | min=50ms  avg=54ms  p50=55ms  max=58ms |
| TC-E03 | PASS | Sequential chat (10x) | min=1310ms  avg=1502ms  p50=1561ms  max=1625ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=266ms  throughput=37 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=1562ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=2s  chat: status=ready idle=2s   |
| TC-E08 | PASS | VLM text-only | 3504ms |
| TC-E09 | PASS | VLM image_url (remote) | 3542ms |
| TC-E10 | PASS | VLM image_url (base64) | 1314ms |
| TC-E11 | PASS | Rerank endpoint | 971ms count=3 |
| TC-E14 | PASS | Thinking off non-blank | 236ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 24362ms r=5804 a=4 |
| TC-E15 | PASS | Thinking budget answer | 3904ms a=207 |
