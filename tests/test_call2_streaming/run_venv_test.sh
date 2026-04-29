#!/bin/bash
set -e

# Configuration
TEST_DIR="tests/test_call2_streaming"
VENV_DIR="$TEST_DIR/test_venv"
MODEL="$HOME/.lmforge/models/qwen3.5-4b-4bit"
PORT=8888

echo "🚀 Setting up Python virtual environment at $VENV_DIR..."
python3 -m venv "$VENV_DIR"

# Activate venv
source "$VENV_DIR/bin/activate"

echo "📦 Installing required packages (mlx-lm, httpx)..."
pip install --quiet mlx-lm httpx

echo "⚙️ Starting mlx_lm.server on port $PORT in the background..."
python -m mlx_lm.server --model "$MODEL" --port "$PORT" > "$TEST_DIR/server.log" 2>&1 &
SERVER_PID=$!

echo "⏳ Waiting for server to start (monitoring logs)..."
# Wait up to 60 seconds for server to start
for i in {1..120}; do
    if grep -q "Starting httpd at" "$TEST_DIR/server.log"; then
        echo "✅ Server is up and running!"
        break
    fi
    sleep 1
    if [ $i -eq 60 ]; then
        echo "❌ Server failed to start in time. Logs:"
        cat "$TEST_DIR/server.log"
        kill $SERVER_PID
        exit 1
    fi
done

echo "🧪 Running the engine behavior test..."
python "$TEST_DIR/test_engine_behavior.py" --url "http://127.0.0.1:$PORT" --prefill-size 4000 --model-name "$MODEL" > "$TEST_DIR/test_results.txt" 2>&1

echo "🛑 Stopping the server (PID: $SERVER_PID)..."
kill $SERVER_PID
wait $SERVER_PID 2>/dev/null || true

echo "🧹 Cleaning up virtual environment..."
deactivate
rm -rf "$VENV_DIR"

echo "🎉 Test complete! Results saved to $TEST_DIR/test_results.txt"
cat "$TEST_DIR/test_results.txt"
