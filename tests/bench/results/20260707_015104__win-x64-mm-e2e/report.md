# multi_model_e2e report

- when: 20260707_015104
- machine: **win-x64** | gpu: none | engine: llamacpp
- build: lmforge 0.1.5 (201079c 2026-07-06) | checkout: 201079c
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=False vlm=True rerank=True mtp=True chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=3439ms chat=6375ms |
| TC-E02 | PASS | Sequential embed (10) | ok |
| TC-E03 | PASS | Sequential chat (10) | ok |
| TC-E04 | PASS | Concurrent embed (10) | wall=13782ms |
| TC-E05 | PASS | Simultaneous embed+chat | wall=3393ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400 chat@embed=400 |
| TC-E07 | PASS | State consistency | both ready |
| TC-E08 | PASS | VLM text-only | 6745ms |
| TC-E09 | PASS | VLM image_url (remote) | 4241ms |
| TC-E10 | PASS | VLM image_url (base64) | 3855ms |
| TC-E11 | PASS | Rerank endpoint | 3747ms |
| TC-E12 | SKIP | MTP speculative | mode=off |
| TC-E14 | PASS | Thinking off non-blank | 5 chars |
| TC-E13 | PASS | Thinking on reasoning+answer | r=5411 a=254 |
| TC-E15 | PASS | Thinking budget answer | 407 chars |
