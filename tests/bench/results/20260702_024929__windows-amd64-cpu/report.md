# think_bench report

- when: 20260702_024929
- machine: **windows-amd64-cpu**
- os: Windows 11 (10.0.26200)
- arch: AMD64 | accel: cpu | cpus: 8 | python: 3.12.13
- engine: llamacpp
- build: lmforge 0.1.5 (82d6672-dirty 2026-07-01) | git: 82d6672-dirty
- hostname: DESKTOP-O34TAVR
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

`correct` = real answers the user saw (blank/length runs score as fail). `blank` = produced no answer content (e.g. thinking budget exhausted).

| model | mode | n | correct | blank | dup | looped | leak | length | err |
|---|---|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 10/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 8 | 0 |
| qwen2.5:7b:4bit | off | 12 | 10/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3.5:4b:6bit | on | 12 | 9/12 | 0 | 0 | 0 | 0 | 6 | 0 |
| qwen3:1.7b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:1.7b:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 1 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 3/12 | 6 | 0 | 0 | 0 | 10 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 10/12 | 2 | 0 | 0 | 0 | 3 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
| qwen3:8b:4bit | on | 12 | 12/12 | 0 | 0 | 0 | 0 | 0 | 0 |
