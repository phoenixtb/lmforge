# multi_model_e2e report

- when: 20260704_005800
- machine: **win-x64**
- build: lmforge 0.1.5 (1509c9b 2026-07-03) | checkout: 1509c9b
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=True vlm=True rerank=True mtp=True chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Sequential load embed+chat | embed=4013ms chat=6458ms (no-burst: co-residency not required) |
| TC-E02 | PASS | Sequential embed (10) | ok |
| TC-E03 | PASS | Sequential chat (10) | ok |
| TC-E04 | SKIP | Concurrent embed (10) | no-burst mode |
| TC-E05 | SKIP | Simultaneous embed+chat | no-burst mode |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400 chat@embed=400 |
| TC-E07 | PASS | State consistency (sequential) | 2 ready |
| TC-E08 | PASS | VLM text-only | 7859ms |
| TC-E09 | PASS | VLM image_url (remote) | 4307ms |
| TC-E10 | PASS | VLM image_url (base64) | 3863ms |
| TC-E11 | PASS | Rerank endpoint | 3913ms |
| TC-E12 | SKIP | MTP speculative | mode=off |
| TC-E14 | PASS | Thinking off non-blank | 5 chars |
| TC-E13 | FAIL | Thinking on reasoning+answer | r='0' a='1000' (expected both non-empty) |
| TC-E15 | PASS | Thinking budget answer | 737 chars |
