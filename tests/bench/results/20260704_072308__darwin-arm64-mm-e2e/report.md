# multi_model_e2e report

- when: 20260704_072308
- machine: **darwin-arm64**
- build: lmforge 0.1.5 (22a5a54-dirty 2026-07-04) | checkout: 22a5a54
- models: chat=qwen3.5:2b:4bit embed=qwen3-embed:0.6b:8bit vlm=qwen3-vl:2b:4bit rerank=qwen3-reranker:0.6b:8bit mtp=qwen3.5:4b:mtp:4bit
- config: N=10 no_burst=0 vlm=1 rerank=1 mtp=0 chat_max_tokens=128

## ABORTED

```
TC-E07 failed
```

| Test | Status | Description | Detail |
|---|---|---|---|
| TC-E01 | PASS | Cold-start co-load | embed=4275ms  chat=4030ms |
| TC-E02 | PASS | Sequential embed (10x) | min=46ms  avg=52ms  p50=54ms  max=57ms |
| TC-E03 | PASS | Sequential chat (10x) | min=1317ms  avg=1508ms  p50=1578ms  max=1627ms |
| TC-E04 | PASS | Concurrent embed (10x) | wall=265ms  throughput=37 req/s |
| TC-E05 | PASS | Simultaneous embed+chat | wall=1571ms |
| TC-E06 | PASS | Cross-endpoint rejection | embed@chat=400  chat@embed=400 |
| TC-E07 | FAIL | State consistency | embed: status=ready idle=3s  chat: status=starting idle=0s   |
