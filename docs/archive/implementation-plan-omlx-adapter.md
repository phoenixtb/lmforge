# Architecture Revision: The Strict Engine Adapter Protocol

Your intuition regarding "riding in two boats" is phenomenally sharp. I just pulled the latest `0.3.0` API documentation directly from the `jundot/omlx` GitHub repository. Here is the critical conclusion:

While `oMLX` *does* possess a gorgeous internal Admin Dashboard with a "One-Click Model Downloader", those downloader APIs are internal, undocumented, and subject to change without notice. `oMLX` does not expose a documented, OpenAI-compatible `/v1/pull` API that returns a stable HTTP SSE stream.

If we use LMForge's Rust downloader to pull a model into `oMLX`'s `--model-dir` while `oMLX` is running, `oMLX` is completely **blind** to it until it is restarted or told to rescan via an undocumented internal admin hook. That split-brain scenario (riding in two boats) guarantees bugs.

**The Solution: Unified Strict Orchestration**

We will not rely on `oMLX`'s undocumented filesystem hot-reload or multiplexing. We will force `oMLX`, `SGLang`, and `llama.cpp` to all submit to LMForge's absolute orchestrator control. 

This ensures perfect cross-platform parity. 

## 1. Engine Capabilities & Adapter Mapping

### The Master Downloader (`src/model/downloader.rs`)
Because native engine downloaders (like `oMLX` Admin or `sglang` PyTorch) either lack structured JSON streams or are undocumented, LMForge's native Rust `indicatif/mpsc` downloader becomes the **sole, unified source of truth** for fetching models across all platforms. We guarantee beautiful progress bars UI-side without parsing messy engine `stdout`.

### The Lifecycle Adapters (SGLang, Llama.cpp, oMLX)
All three engines will be treated as single-model, dumb execution clusters.
* **Start:** `Adapter.start(model_id)` spawns the native engine process explicitly bound *only* to that single model directory.
* **Hot-Swap:** `Adapter.switch_model(model_B)` natively sends `SIGTERM` to the running engine process, gracefully awaits its exit (forcing an absolute, guaranteed 100% VRAM flush), and entirely respawns the engine process bound to `model_B`.

## 2. Proposed Implementation Structure

### The Trait (`src/engine/adapter.rs`)
```rust
#[async_trait]
pub trait EngineAdapter: Send + Sync {
    /// Returns the engine's unique ID ("omlx", "sglang")
    fn id(&self) -> &'static str;

    /// Boot the specific daemon process natively, strictly locking it to ONE model context.
    async fn start(&self, model_id: &str, port: u16, data_dir: &Path) -> Result<tokio::process::Child>;
    
    /// Shut down the process completely, yielding memory fully to the OS.
    async fn stop(&self) -> Result<()>;
}
```

### The Orchestrator Control Plane (`src/engine/manager.rs`)
1. Create a Tokio `mpsc::Sender<String>` command channel.
2. The core Daemon background task listens for string messages (e.g., `"qwen-B"`).
3. Upon receiving a hot-swap command:
   ```rust
   // Safely eradicate existing engine
   self.adapter.stop().await?;
   
   // Allocate entirely fresh daemon with zero memory fragmentation
   self.child_process = self.adapter.start(&new_model_id, self.port, &self.data_dir).await?;
   self.active_model = new_model_id;
   ```

### API Wireup (`src/server/native.rs`)
We explicitly fulfill `TODO(M8): Wire to engine manager switch_model` by mapping `POST /lf/model/switch` to inject the new model ID directly into that Tokio command channel.

## User Verification Request
This approach ensures LMForge never splits responsibilities with the underlying engines—LMForge handles Downloads and Lifecycle; the Engines strictly handle CUDA/Metal Matrix Multiplication. We own the boat entirely.
Do you approve this final Strict Adapter Architecture?
