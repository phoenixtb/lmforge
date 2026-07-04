# multi_model_e2e report

- when: 20260704_081950
- machine: **win-x64**
- build: lmforge 0.1.5 (0d045c8 2026-07-04) | checkout: 0d045c8
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=False vlm=True rerank=True mtp=True chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=4076ms chat=5726ms |
| TC-E02 | PASS | Sequential embed (10) | ok |
| TC-E03 | PASS | Sequential chat (10) | ok |
| TC-E04 | PASS | Concurrent embed (10) | wall=16852ms |
| TC-E05 | PASS | Simultaneous embed+chat | wall=1226ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400 chat@embed=400 |
| TC-E07 | PASS | State consistency | both ready |
| TC-E08 | PASS | VLM text-only | 7692ms |
| TC-E09 | PASS | VLM image_url (remote) | 1192ms |
| TC-E10 | PASS | VLM image_url (base64) | 1064ms |
| TC-E11 | PASS | Rerank endpoint | 7581ms |
| TC-E12 | PASS | MTP speculative | mode=mtp warm=15666ms |
| TC-E14 | PASS | Thinking off non-blank | 5 chars |
| TC-E13 | PASS | Thinking on reasoning+answer | r=2834 a=599 |
| TC-E15 | PASS | Thinking budget answer | 504 chars |
