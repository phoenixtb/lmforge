# think_bench report

- when: 20260704_115218
- machine: **linux-x86_64-cuda**
- os: Linux 6.17.0-35-generic (#35~24.04.1-Ubuntu SMP PREEMPT_DYNAMIC Tue May 26 19:30:42 UTC 2)
- arch: x86_64 | accel: cuda | cpus: 6 | python: 3.12.13
- engine: llamacpp
- build: lmforge 0.1.5 (8ab6260 2026-07-04) | git: 8ab6260-dirty
- hostname: aist1-ubuntu
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | dup | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 9 | 0 |
| qwen2.5:7b:4bit | off | 12 | 9/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 1 | 0 |
| qwen3.5:2b:4bit | on | 12 | 12/12 | 0 | 0 | 8 | 0 | 1 | 0 |
| qwen3.5:4b:6bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 0 | 0 | 4 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | on | 12 | 11/12 | 1 | 0 | 5 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 12/12 | 0 | 0 | 1 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 12/12 | 0 | 0 | 1 | 0 | 0 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:8b:4bit | on | 12 | 11/12 | 1 | 0 | 5 | 0 | 0 | 0 |
