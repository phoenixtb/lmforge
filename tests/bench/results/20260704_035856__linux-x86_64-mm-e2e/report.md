# multi_model_e2e report

- when: 20260704_035856
- machine: **linux-x86_64**
- build: lmforge 0.1.5 (72d70a1 2026-07-03) | checkout: 72d70a1
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=1 chat_max_tokens=128

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=2519ms  chat=7878ms |
| TC-E02 | PASS | Sequential embed (10x) | min=32ms  avg=33ms  p50=32ms  max=47ms |
| TC-E03 | PASS | Sequential chat (10x) | min=691ms  avg=694ms  p50=695ms  max=701ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=1364ms  throughput=7 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=699ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | PASS | State consistency | embed: status=ready idle=7s  chat: status=ready idle=4s   |
| TC-E08 | PASS | VLM text-only | 6036ms |
| TC-E09 | PASS | VLM image_url (remote) | 5334ms |
| TC-E10 | PASS | VLM image_url (base64) | 542ms |
| TC-E11 | PASS | Rerank endpoint | 7887ms count=3 |
| TC-E12 | PASS | MTP speculative | mode=mtp samples=1 |
| TC-E14 | PASS | Thinking off non-blank | 3000ms |
| TC-E13 | PASS | Thinking on reasoning+answer | 6072ms r=2078 a=695 |
| TC-E15 | PASS | Thinking budget answer | 21621ms a=355 |
