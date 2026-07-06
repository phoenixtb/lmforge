# multi_model_e2e report

- when: 20260707_022037
- machine: **linux-x86_64** | gpu: none (0.0 GB) | engine: llamacpp
- build: lmforge 0.1.5 (f1e0e64 2026-07-06) | checkout: f1e0e64
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=1 vlm=1 rerank=1 mtp=1 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Sequential load embed+chat | embed=3777ms  chat=5528ms |
| TC-E02 | PASS | Sequential embed (10x) | min=77ms  avg=107ms  p50=81ms  max=314ms |
| TC-E03 | PASS | Sequential chat (10x) | min=3132ms  avg=3453ms  p50=3409ms  max=4138ms |
| TC-E04 | SKIP | Concurrent embed (10x) | skipped (--no-burst: low memory) |
| TC-E05 | SKIP | Simultaneous embed+chat | skipped (--no-burst: low memory) |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency (sequential) | 2 ready |
| TC-E08 | PASS | VLM text-only | 6121ms |
| TC-E09 | PASS | VLM image_url (remote) | 4743ms |
| TC-E10 | PASS | VLM image_url (base64) | 2382ms |
| TC-E11 | PASS | Rerank endpoint | 2632ms count=3 |
| TC-E12 | SKIP | MTP speculative | mode=off samples=0 |
| TC-E14 | PASS | Thinking off non-blank | 1389ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 48728ms r=4822 a=777 |
| TC-E15 | PASS | Thinking budget answer | 9504ms a=427 |
