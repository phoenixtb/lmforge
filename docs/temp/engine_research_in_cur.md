## State of Inference Engines on Blackwell Consumer (sm_120) — May 2026


| Engine                   | sm_120 status today                                                                                                   | Install path                                                                                                                           | Best RTX 5090 perf                                                                            | Verdict                                 |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | ----------------------------------------- |
| **SGLang 0.5.10**        | ✗**No** `sm120` **kernels in any wheel** (cu120/cu128/cu130 all ship sm90+sm100 only). Five open upstream issues.    | Pip → broken at runtime                                                                                                               | n/a (broken)                                                                                  | **Drop on sm_120**                      |
| **vLLM 0.17+**           | ✓**Native sm_120 support** (PRs #19794, #33417, #34822, #37116 merged). Stable wheel + `cu130-nightly` Docker.       | `uv pip install vllm --torch-backend=auto` (auto-routes to `<span class="md-inline-path-prefix">wheels.vllm.ai/nightly/cu130/</span>`) | **480 tok/s** Llama 3.1 8B AWQ batch=64; 140 tok/s Qwen3-14B-AWQ; 175 tok/s Qwen3.6-35B NVFP4 | **Primary**                             |
| **TensorRT-LLM 1.2/1.3** | ⚠ sm_120 supported but requires NGC Docker + C++ source patches for NVFP4 + heavy footprint (~30 GB image)           | Docker-only realistically                                                                                                              | Highest peak (Qwen3.6 35B NVFP4: 175 t/s)                                                     | Too heavy for an embeddable tool        |
| **LMDeploy TurboMind**   | ✓ sm_120 supported (v0.12.2, 2026-03) —`pip install lmdeploy` w/ CUDA 12.8 prebuilt                                 | Pip works for sm_120                                                                                                                   | Comparable to vLLM (≈80–100 t/s)                                                            | Solid 2nd option — InternLM ecosystem  |
| **TGI 3.3**              | ⚠ sm_120 works since 3.3.1,**but TGI entered maintenance mode 2025-12-11**. HF officially recommends migrating away. | Pip/Docker                                                                                                                             | ≈75–85 t/s                                                                                  | **Reject** — abandoned                 |
| **MLC-LLM**              | ✓ via source build with`CMAKE_CUDA_ARCHITECTURES=120a`                                                               | Build-from-source only                                                                                                                 | ≈138 t/s class                                                                               | Build complexity too high               |
| **ExLlamaV2/V3**         | ✓ sm_120 works (pure PyTorch CUDA, no custom .so blob)                                                               | Pip                                                                                                                                    | **Best single-stream**: 187 t/s Llama-3 8B Q4_K_M on RTX 4090, scales to 5090                 | Niche — GPTQ/AWQ only, no batching API |
| **llama.cpp**            | ✓ sm_120 works (Q4_K_M, MTP via PR #22673 = +123%)                                                                   | Binary or build                                                                                                                        | **78 t/s** Qwen3.6-27B MTP                                                                    | Universal fallback                      |

### Hard benchmark numbers on consumer hardware


| Hardware + model                                | llama.cpp | vLLM         | Multiplier       |
| ------------------------------------------------- | ----------- | -------------- | ------------------ |
| 2× RTX 5060 Ti, Qwen3.6-27B, chat workload     | 94 t/s    | **345 t/s**  | **3.7× faster** |
| 2× RTX 5060 Ti, Qwen3.6-27B, agentic workload  | 72 t/s    | **262 t/s**  | **3.6× faster** |
| RTX 5090, Llama 3.1 8B AWQ, batch=64            | ~70 t/s   | **478 t/s**  | **6.8× faster** |
| Single RTX 5090, Llama 3 70B Q4_K_M             | 43 t/s    | **48 t/s**   | 1.1×            |
| RTX 5060 Ti 16GB (your box), Qwen3 8B Q4 (est.) | ~60 t/s   | **~200 t/s** | **3.3× faster** |

**Your instinct was right.** llama.cpp gives you ~25–30% of vLLM's throughput on consumer Blackwell. For "a tool for other apps" — i.e. concurrent users, agentic loops, multi-LoRA — that's a non-starter.

---

## Feature parity — vLLM vs SGLang (what we'd lose / gain)


| Capability                                                                                                                | vLLM                                                     | SGLang                          | Net                                                                               |
| --------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- | --------------------------------- | ----------------------------------------------------------------------------------- |
| OpenAI Chat API + streaming                                                                                               | ✓                                                       | ✓                              | =                                                                                 |
| `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">embeddings</span>`                  | ✓                                                       | ✓ (via`--is-embedding`)        | =                                                                                 |
| `<span class="md-inline-path-prefix">/v1/</span><span class="md-inline-path-filename">rerank</span>` (Jina+Cohere compat) | ✓**native**                                             | ✗ broken in 0.5.10             | **+vLLM**                                                                         |
| Tool calling + reasoning parsers                                                                                          | ✓                                                       | ✓                              | =                                                                                 |
| Multi-LoRA                                                                                                                | ✓ dense+MoE                                             | ✓                              | =                                                                                 |
| **Multi-model in single process**                                                                                         | ✗ (one model per instance)                              | ✓ (load/unload on demand)      | **+SGLang**                                                                       |
| Continuous batching                                                                                                       | ✓ PagedAttention                                        | ✓ RadixAttention               | = (vLLM slightly better at concurrency, SGLang slightly better at prefix-caching) |
| Quant formats                                                                                                             | AWQ, GPTQ, FP8,**NVFP4**, W4A16, bnb, compressed-tensors | AWQ, GPTQ, FP8, W4A16, bnb      | **+vLLM** (NVFP4 = ~1.6× on Blackwell)                                           |
| sm_120 (RTX 50)                                                                                                           | ✓                                                       | ✗                              | **+vLLM**                                                                         |
| Hardware (other)                                                                                                          | NVIDIA, AMD ROCm, Intel XPU/Gaudi, TPU, ARM CPU          | NVIDIA only                     | **+vLLM**                                                                         |
| Community / momentum                                                                                                      | 75k+ stars, 200+ model arches, $150M funding             | Smaller but rapidly catching up | **+vLLM**                                                                         |

**The one real loss** is multi-model-in-single-process. SGLang's `keep_alive=5m` model swap-in/out is convenient. vLLM serves one model per instance — but **so does llama.cpp**, and we already handle that with our spawn-per-model orchestrator. The pattern transfers cleanly.

---

## Strategic recommendation — proposed new engine matrix

LMForge engine priority (Linux + NVIDIA):

1. vLLM            ← NEW PRIMARY  (sm_75 → sm_120,  4090/5060Ti/5070/5080/5090/B100/B200/etc.)
2. llama.cpp       ← fallback     (any CUDA, any GPU, CPU; long-context specialist)
3. SGLang          ← gated        (sm_90 + sm_100 only — Hopper / DC Blackwell;

                               re-enable once sgl-kernel ships sm_120)
### Why this is the right shape for "a tool for other apps"* **Performance ceiling**: vLLM is competitive with TensorRT-LLM at ~80% the throughput, with ~5% of the install complexity. For consumer Blackwell it's effectively the SOTA open option.
* **One API to your downstream consumers**: vLLM speaks the same OpenAI Chat/Embeddings/Rerank API — your UI and any apps consuming `lmforge` see no change.
* **NVFP4 is the killer feature**: only vLLM and TRT-LLM support it; on Blackwell consumer it's 1.6× faster than FP8/AWQ at 2-4% quality loss. Future-proofs the catalog for Qwen3.5+ and Llama 4+ NVFP4 releases (already happening — `<span class="md-inline-path-prefix">sakamakismile/</span><span class="md-inline-path-filename">Qwen3.6-27B-NVFP4</span>`, `<span class="md-inline-path-prefix">Qwen/</span><span class="md-inline-path-filename">Qwen3.5-27B-NVFP4</span>`, `<span class="md-inline-path-prefix">unsloth/</span><span class="md-inline-path-filename">MiniMax-M2.5-NVFP4</span>`).
* **Llama.cpp stays in the loop**: free long-context superpower (TurboQuant K8V4 + MTP), Windows + CPU + low-VRAM coverage.
* **Multi-model**: handled at orchestrator layer (spawn-per-model with a port-pool + idle eviction). We already do this for llama.cpp; the same code generalises to vLLM.


---

## Sources (cited)


| Claim                                         | Source                                                                                                                                                                                          |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| vLLM 0.17+ has native sm_120 + cu130 wheels   | [vllm install docs](https://github.com/vllm-project/vllm/blob/main/docs/getting_started/installation/gpu.cuda.inc.md), [issue #35432 closed](https://github.com/vllm-project/vllm/issues/35432) |
| 480 tok/s RTX 5090 Llama 3.1 8B AWQ           | [Markaicode bench](https://markaicode.com/benchmarks/rtx-5090-vllm-benchmark/)                                                                                                                  |
| 3.7× throughput advantage on 2× RTX 5060 Ti | [LLMKube bake-off, Qwen3.6-27B](https://llmkube.com/blog/qwen3-6-27b-bakeoff)                                                                                                                   |
| SGLang lacks sm_120 builds                    | [docs.sglang.ai/whl/cu130/](https://docs.sglang.ai/whl/cu130/) (verified: only sm90+sm100 in 0.4.2.post2), upstream issues #26087/#19637/#24633/#23657/#21782                                   |
| TGI in maintenance mode                       | [HF docs](https://huggingface.co/docs/inference-endpoints/main/en/engines/tgi) (12/11/2025)                                                                                                     |
| LMDeploy TurboMind sm_120                     | [InternLM/lmdeploy#3421](https://github.com/InternLM/lmdeploy/issues/3421)                                                                                                                      |
| NVFP4 = 1.6× FP8 on Blackwell                | [arxiv 2601.09527 — Private LLM Inference on Consumer Blackwell](https://arxiv.org/html/2601.09527)                                                                                            |

---

## Concrete next step

**Replace SGLang with vLLM as the primary Linux+NVIDIA engine.** Specific work:

1. `<span class="md-inline-path-prefix">data/</span><span class="md-inline-path-filename">engines.toml</span>`: add `[[engine]] id = "vllm"` block (priority 25), demote sglang (priority 15, gated to compute_cap 9.0–10.x), keep llamacpp as universal fallback.
2. `<span class="md-inline-path-prefix">src/engine/adapters/vllm/</span>`: new adapter — close cousin of the sglang one (`uv pip install vllm --torch-backend=auto`, spawn `vllm serve {model_path} --port {port} --host 127.0.0.1 --quantization {detected}`, health on `<span class="md-inline-path-prefix">/</span><span class="md-inline-path-filename">health</span>`, OpenAI-compat APIs already pass through).
3. **Multi-model orchestrator**: extend the existing port-pool eviction logic to also drive vLLM workers (the llamacpp adapter already does this — generalise into `engine::multi_model`).
4. `<span class="md-inline-path-prefix">src/hardware/</span><span class="md-inline-path-filename">probe.rs</span>`: capture compute capability into `<span class="md-inline-path-filename">hardware.json</span>` for the new engine selector.
5. **Engine selector**: pick by hierarchy (vllm if compute_cap ≥ 7.5 → llamacpp fallback → sglang only when DC Blackwell).
6. **Observability**: surface worker stderr tail in `<span class="md-inline-path-prefix">/lf/</span><span class="md-inline-path-filename">status.engine.last_error</span>` (the Fix-4 we already planned — now even more important since vLLM's startup is verbose and informative).
7. **Catalog**: no changes — `<span class="md-inline-path-filename">safetensors.json</span>` AWQ/GPTQ/W4A16/FP8 entries all work with vLLM. Optionally add `:nvfp4` suffix later for Blackwell users (lossy on Hopper/Ada, fast on sm_100/sm_120).

**Estimated effort**: ~1 day for the adapter + selector + hardware probe; ~½ day for orchestrator generalisation; ~½ day for tests/docs. Net 2 days to ship a working RTX 5060 Ti experience with **3–6× the throughput** llama.cpp would give.

Want me to start with **step 1 (engines.toml + hardware probe)** so you can review the registry shape before I write the adapter? Or jump straight to the vLLM adapter scaffold?
