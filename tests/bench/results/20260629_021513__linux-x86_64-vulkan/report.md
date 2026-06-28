# think_bench report

- when: 20260629_021513
- machine: **linux-x86_64-vulkan**
- os: Linux 7.0.13-200.fc44.x86_64 (#1 SMP PREEMPT_DYNAMIC Fri Jun 19 22:51:30 UTC 2026)
- arch: x86_64 | accel: vulkan | cpus: 8 | python: 3.12.13
- engine: llamacpp
- build: lmforge 0.1.5 (d85e0bd 2026-06-28) | git: d85e0bd
- hostname: fedora
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 9/12 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 7 | 0 |
| qwen2.5:7b:4bit | off | 12 | 12/12 | 0 | 1 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 4/12 | 8 | 2 | 0 | 9 | 0 |
| qwen3.5:2b:4bit | on | 12 | 3/12 | 8 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | off | 12 | 7/12 | 4 | 1 | 0 | 10 | 0 |
| qwen3.5:4b:6bit | on | 12 | 10/12 | 2 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | off | 12 | 5/12 | 7 | 2 | 0 | 7 | 0 |
| qwen3:1.7b:4bit | on | 12 | 10/12 | 2 | 0 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 6/12 | 5 | 1 | 0 | 8 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 9/12 | 2 | 2 | 0 | 3 | 0 |
| qwen3:8b:4bit | off | 12 | 5/12 | 5 | 0 | 0 | 7 | 0 |
| qwen3:8b:4bit | on | 12 | 9/12 | 3 | 0 | 0 | 0 | 0 |
