# multi_model_e2e report

- when: 20260704_113005
- machine: **linux-x86_64**
- build: lmforge 0.1.5 (8ab6260 2026-07-04) | checkout: 8ab6260
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=1 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=1771ms  chat=3401ms |
| TC-E02 | PASS | Sequential embed (10x) | min=14ms  avg=16ms  p50=15ms  max=29ms |
| TC-E03 | PASS | Sequential chat (10x) | min=637ms  avg=640ms  p50=639ms  max=646ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=207ms  throughput=48 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=647ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=0s  chat: status=ready idle=0s   |
| TC-E08 | PASS | VLM text-only | 3276ms |
| TC-E09 | PASS | VLM image_url (remote) | 1063ms |
| TC-E10 | PASS | VLM image_url (base64) | 433ms |
| TC-E11 | PASS | Rerank endpoint | 1920ms count=3 |
| TC-E12 | PASS | MTP speculative | mode=mtp samples=1 |
| TC-E14 | PASS | Thinking off non-blank | 3013ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 11704ms r=5372 a=252 |
| TC-E15 | PASS | Thinking budget answer | 2235ms a=577 |
