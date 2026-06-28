# think_bench report

- when: 20260628_145101
- machine: **linux-x86_64-vulkan**
- os: Linux 7.0.13-200.fc44.x86_64 (#1 SMP PREEMPT_DYNAMIC Fri Jun 19 22:51:30 UTC 2026)
- arch: x86_64 | accel: vulkan | cpus: 8 | python: 3.12.13
- engine: llamacpp
- hostname: 192
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

| model | mode | n | correct | looped | leak | length | err |
|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 10 | 0 |
| qwen2.5:7b:4bit | off | 12 | 10/12 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 12/12 | 2 | 0 | 11 | 0 |
| qwen3.5:2b:4bit | on | 12 | 12/12 | 5 | 0 | 8 | 0 |
| qwen3.5:4b:6bit | off | 12 | 10/12 | 2 | 0 | 8 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 1 | 0 | 2 | 0 |
| qwen3:1.7b:4bit | off | 12 | 11/12 | 2 | 0 | 8 | 0 |
| qwen3:1.7b:4bit | on | 12 | 11/12 | 5 | 0 | 5 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 12/12 | 1 | 0 | 7 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 12/12 | 1 | 0 | 3 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 0 | 0 | 9 | 0 |
| qwen3:8b:4bit | on | 12 | 11/12 | 4 | 0 | 5 | 0 |
