# multi_model_e2e report

- when: 20260707_015511
- machine: **darwin-arm64** | gpu: apple (27.0 GB) | engine: omlx
- build: lmforge 0.1.5 (201079c 2026-07-06) | checkout: 201079c
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=0 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=3929ms  chat=4338ms |
| TC-E02 | PASS | Sequential embed (10x) | min=45ms  avg=50ms  p50=51ms  max=54ms |
| TC-E03 | PASS | Sequential chat (10x) | min=1235ms  avg=1425ms  p50=1489ms  max=1563ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=257ms  throughput=38 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=1470ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=4s  chat: status=ready idle=2s   |
| TC-E08 | PASS | VLM text-only | 3459ms |
| TC-E09 | PASS | VLM image_url (remote) | 2603ms |
| TC-E10 | PASS | VLM image_url (base64) | 1122ms |
| TC-E11 | PASS | Rerank endpoint | 1250ms count=3 |
| TC-E14 | PASS | Thinking off non-blank | 231ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 23754ms r=5549 a=4 |
| TC-E15 | PASS | Thinking budget answer | 3957ms a=269 |
