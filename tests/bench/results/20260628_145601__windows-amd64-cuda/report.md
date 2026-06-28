# think_bench report

- when: 20260628_145601
- machine: **windows-amd64-cuda**
- os: Windows 11 (10.0.26200)
- arch: AMD64 | accel: cuda | cpus: 6 | python: 3.13.13
- engine: llamacpp
- hostname: aist1-win-0
- base: http://127.0.0.1:11430
- models: 10 | prompts: 6 | runs: 180

## Aggregate (model x mode)

| model | mode | n | correct | looped | leak | length | err |
|---|---|---|---|---|---|---|---|
| gemma3:4b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 |
| llama3.1:8b:4bit | off | 12 | 12/12 | 0 | 0 | 0 | 0 |
| phi4:4b:4bit | off | 12 | 11/12 | 0 | 0 | 0 | 0 |
| phi4:4b:reasoning:4bit | off | 12 | 12/12 | 0 | 0 | 8 | 0 |
| qwen2.5:7b:4bit | off | 12 | 12/12 | 1 | 0 | 0 | 0 |
| qwen3.5:2b:4bit | off | 12 | 12/12 | 3 | 0 | 10 | 0 |
| qwen3.5:2b:4bit | on | 12 | 12/12 | 5 | 0 | 8 | 0 |
| qwen3.5:4b:6bit | off | 12 | 9/12 | 1 | 0 | 10 | 0 |
| qwen3.5:4b:6bit | on | 12 | 12/12 | 1 | 0 | 1 | 0 |
| qwen3:1.7b:4bit | off | 12 | 11/12 | 2 | 0 | 8 | 0 |
| qwen3:1.7b:4bit | on | 12 | 12/12 | 3 | 0 | 4 | 0 |
| qwen3:4b:thinking:4bit | off | 12 | 10/12 | 0 | 0 | 10 | 0 |
| qwen3:4b:thinking:4bit | on | 12 | 12/12 | 0 | 0 | 3 | 0 |
| qwen3:8b:4bit | off | 12 | 12/12 | 2 | 0 | 8 | 0 |
| qwen3:8b:4bit | on | 12 | 12/12 | 3 | 0 | 2 | 0 |
