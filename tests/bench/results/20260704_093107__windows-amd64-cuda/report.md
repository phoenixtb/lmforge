# think_bench report

- when: 20260704_093107
- machine: **windows-amd64-cuda**
- os: Windows 11 (10.0.26200)
- arch: AMD64 | accel: cuda | cpus: 6 | python: 3.13.13
- engine: llamacpp
- build: lmforge 0.1.5 (0d045c8 2026-07-04) | git: 0d045c8-dirty
- hostname: aist1-win-0
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | dup | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 2 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 0/12 | 0 | 0 | 12 | 0 | 12 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 8 | 0 |
| qwen2.5:7b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 0/12 | 0 | 0 | 12 | 0 | 12 | 0 |
| qwen3.5:2b:4bit | on | 12 | 0/12 | 0 | 0 | 12 | 0 | 12 | 0 |
| qwen3.5:4b:6bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 0 | 0 | 2 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | on | 12 | 12/12 | 0 | 0 | 6 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:8b:4bit | on | 12 | 12/12 | 0 | 0 | 5 | 0 | 0 | 0 |
