#!/usr/bin/env bash
set -e

echo "⚙ Running LMForge End-to-End Integration Test"

# Ensure binary is built
cargo build

LMFORGE_BIN="./target/debug/lmforge"
export LMFORGE_DATA_DIR=$(mktemp -d)

# Cleanup on exit
function cleanup {
    echo "⚙ Cleaning up..."
    $LMFORGE_BIN stop || true
    rm -rf "$LMFORGE_DATA_DIR"
}
trap cleanup EXIT

echo "1. Initialize"
$LMFORGE_BIN init --config "$LMFORGE_DATA_DIR/config.toml"

echo "2. Check Status (Expect Down)"
! $LMFORGE_BIN status | grep "LMForge is running"

echo "3. Pull Model (Testing Aliases/Resolvers natively)"
$LMFORGE_BIN pull qwen3.5-4b

echo "4. List Models"
$LMFORGE_BIN models list | grep "qwen3.5-4b"

echo "5. Start Daemon"
# We start it natively then ping health
$LMFORGE_BIN start &
daemon_pid=$!

echo "Waiting for health check..."
for i in {1..30}; do
    if curl -s http://127.0.0.1:11430/health | grep -q "OK"; then
        echo "✓ API Health OK"
        break
    fi
    sleep 1
done

echo "6. Chat API Test"
RESPONSE=$(curl -s -X POST http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3.5-4b-optiq-4bit",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }')

if echo "$RESPONSE" | grep -q "choices"; then
    echo "✓ Chat API responded correctly"
else
    echo "❌ API Failure: $RESPONSE"
    exit 1
fi

echo "7. Stop Daemon"
$LMFORGE_BIN stop
kill $daemon_pid 2>/dev/null || true

echo "✓ End-to-end integration test passed."
