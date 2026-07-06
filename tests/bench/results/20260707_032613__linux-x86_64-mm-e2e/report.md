# multi_model_e2e report

- when: 20260707_032613
- machine: **linux-x86_64** | gpu: nvidia (15.428711 GB) | engine: llamacpp
- build: lmforge 0.1.5 (017922f 2026-07-06) | checkout: 017922f
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=1 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=1727ms  chat=2348ms |
| TC-E02 | PASS | Sequential embed (10x) | min=14ms  avg=16ms  p50=15ms  max=30ms |
| TC-E03 | PASS | Sequential chat (10x) | min=620ms  avg=622ms  p50=622ms  max=628ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=169ms  throughput=59 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=621ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=0s  chat: status=ready idle=0s   |
| TC-E08 | PASS | VLM text-only | 2186ms |
| TC-E09 | PASS | VLM image_url (remote) | 1033ms |
| TC-E10 | PASS | VLM image_url (base64) | 424ms |
| TC-E11 | PASS | Rerank endpoint | 1753ms count=3 |
| TC-E12 | PASS | MTP speculative | mode=mtp samples=1 |
| TC-E14 | PASS | Thinking off non-blank | 3206ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 11396ms r=5597 a=263 |
| TC-E15 | PASS | Thinking budget answer | 1975ms a=467 |
