# think_bench report

- when: 20260704_085102
- machine: **darwin-arm64-metal**
- os: Darwin 25.5.0 (Darwin Kernel Version 25.5.0: Mon Apr 27 20:41:06 PDT 2026; root:xnu-12377.121.6~2/RELEASE_ARM64_T6030)
- arch: arm64 | accel: metal | cpus: 12 | python: 3.12.13
- engine: omlx
- build: lmforge 0.1.5 (0d045c8-dirty 2026-07-04) | git: 0d045c8-dirty
- hostname: 192
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 192

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | dup | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 7/12 | 0 | 0 | 4 | 0 | 4 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 11/12 | 1 | 0 | 3 | 0 | 1 | 0 |
| phi4:4b:reasoning:4bit | on | 12 | 9/12 | 2 | 0 | 2 | 0 | 3 | 0 |
| qwen2.5:7b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 11/12 | 0 | 0 | 2 | 0 | 4 | 0 |
| qwen3.5:2b:4bit | on | 12 | 11/12 | 0 | 0 | 11 | 0 | 1 | 0 |
| qwen3.5:4b:6bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 0 | 0 | 2 | 0 | 1 | 0 |
| qwen3:1.7b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | on | 12 | 12/12 | 0 | 0 | 8 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 12/12 | 0 | 0 | 2 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 8/12 | 4 | 0 | 5 | 0 | 4 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:8b:4bit | on | 12 | 12/12 | 0 | 0 | 6 | 0 | 0 | 0 |
