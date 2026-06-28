# think_bench report

- when: 20260629_021649
- machine: **windows-amd64-cuda**
- os: Windows 11 (10.0.26200)
- arch: AMD64 | accel: cuda | cpus: 6 | python: 3.13.13
- engine: llamacpp
- build: lmforge 0.1.5 (d85e0bd 2026-06-28) | git: d85e0bd
- hostname: aist1-win-0
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 7 | 0 |
| qwen2.5:7b:4bit | off | 12 | 9/12 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 2/12 | 10 | 1 | 0 | 10 | 0 |
| qwen3.5:2b:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 1 | 0 |
| qwen3.5:4b:6bit | off | 12 | 4/12 | 5 | 1 | 0 | 9 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | off | 12 | 5/12 | 7 | 4 | 0 | 7 | 0 |
| qwen3:1.7b:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 3/12 | 7 | 1 | 0 | 10 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 9/12 | 2 | 2 | 0 | 5 | 0 |
| qwen3:8b:4bit | off | 12 | 5/12 | 7 | 1 | 0 | 7 | 0 |
| qwen3:8b:4bit | on | 12 | 11/12 | 1 | 0 | 0 | 0 | 0 |
