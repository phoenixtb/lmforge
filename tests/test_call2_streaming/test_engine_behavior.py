import asyncio
import json
import time
import argparse
from typing import List, Dict, Any
import httpx

async def test_streaming_behavior(
    engine_url: str,
    prompt: str,
    prefill: str = "",
    max_tokens: int = 100,
    model_name: str = "test-model",
):
    """
    Test the streaming behavior of the engine, capturing token-by-token
    timing and chunk sizes.
    """
    messages = [{"role": "user", "content": prompt}]
    if prefill:
        messages.append({"role": "assistant", "content": prefill})

    payload = {
        "model": model_name,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": True,
        # Ensure thinking is disabled for the assistant prefill continuation
        "chat_template_kwargs": {"enable_thinking": False}
    }

    start_time = time.time()
    ttft = None
    chunks = []
    chunk_times = []
    total_tokens = 0
    total_chars = 0

    print(f"\\n--- Testing Stream (Prefill length: {len(prefill)}) ---")
    
    async with httpx.AsyncClient(timeout=120.0) as client:
        try:
            async with client.stream(
                "POST", 
                f"{engine_url}/v1/chat/completions",
                json=payload
            ) as response:
                
                if response.status_code != 200:
                    print(f"Error: {response.status_code}")
                    print(await response.aread())
                    return
                
                async for line in response.aiter_lines():
                    if not line.startswith("data: "):
                        continue
                        
                    data_str = line[6:].strip()
                    if data_str == "[DONE]":
                        break
                        
                    try:
                        data = json.loads(data_str)
                        if "choices" in data and len(data["choices"]) > 0:
                            delta = data["choices"][0].get("delta", {})
                            content = delta.get("content", "")
                            
                            if content:
                                current_time = time.time()
                                if ttft is None:
                                    ttft = current_time - start_time
                                
                                chunks.append(content)
                                chunk_times.append(current_time)
                                total_tokens += 1
                                total_chars += len(content)
                    except json.JSONDecodeError:
                        print(f"Failed to parse JSON: {data_str}")
        except Exception as e:
            print(f"Connection error: {e}")
            return

    end_time = time.time()
    total_time = end_time - start_time

    print(f"Results:")
    print(f"- Time to First Token (TTFT): {ttft:.3f}s" if ttft else "- TTFT: N/A")
    print(f"- Total Chunks Received: {len(chunks)}")
    print(f"- Total Characters: {total_chars}")
    print(f"- Average Chars per Chunk: {total_chars / max(1, len(chunks)):.1f}")
    
    if len(chunk_times) > 1:
        inter_token_times = [chunk_times[i] - chunk_times[i-1] for i in range(1, len(chunk_times))]
        avg_inter_token = sum(inter_token_times) / len(inter_token_times)
        print(f"- Average Inter-Token Time: {avg_inter_token * 1000:.1f}ms")
    
    print(f"- Total Request Time: {total_time:.3f}s")
    
    # Analyze if it was a bulk dump
    if len(chunks) == 1 and total_chars > 50:
        print("\\n🚨 BEHAVIOR DETECTED: BULK DUMP 🚨")
        print("The engine generated the entire response but emitted it as a single SSE event.")
    elif len(chunks) > 1 and total_chars / len(chunks) < 10:
        print("\\n✅ BEHAVIOR DETECTED: TRUE STREAMING ✅")
        print("The engine correctly emitted tokens one-by-one.")
    else:
        print("\\n⚠️ BEHAVIOR DETECTED: MIXED/CHUNKY STREAMING ⚠️")
        print("The engine emitted tokens in larger grouped chunks.")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Test engine streaming behavior")
    parser.add_argument("--url", default="http://127.0.0.1:8080", help="Engine URL (e.g. mlx_lm.server)")
    parser.add_argument("--prefill-size", type=int, default=4000, help="Size of the assistant prefill in words")
    parser.add_argument("--model-name", default="test-model", help="Model name to pass in the payload")
    args = parser.parse_args()
    
    # 1. Test normal streaming without prefill
    asyncio.run(test_streaming_behavior(
        engine_url=args.url,
        prompt="Explain the theory of relativity in exactly 3 short paragraphs.",
        prefill="",
        max_tokens=150,
        model_name=args.model_name
    ))
    
    # 2. Test streaming with massive prefill (simulating Call 2)
    massive_prefill = "<think>\\n" + "reasoning " * args.prefill_size + "\\n</think>\\n"
    asyncio.run(test_streaming_behavior(
        engine_url=args.url,
        prompt="Explain the theory of relativity in exactly 3 short paragraphs.",
        prefill=massive_prefill,
        max_tokens=150,
        model_name=args.model_name
    ))
