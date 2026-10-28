#!/usr/bin/env bash
# Parity check: byte-identical summaries between app.py (:8001) and the Rust
# binary (:8002) on the synthetic test_thread.py payload.
# Start both first, e.g.:
#   uvicorn app:app --port 8001
#   PORT=8002 ./target/release/local-compact
set -euo pipefail

python3 - <<'EOF'
import json
msgs = [{"role": "system", "content": "You are the Zed agent. Follow AGENTS.md rules."}]
msgs.append({"role": "user", "content": "Build a deterministic fake-LLM compactor service for Zed context compaction and wire it into LiteLLM as local-compact."})
for i in range(20):
    msgs.append({"role": "assistant", "content": f"Step {i}: I will edit app.py section {i} and run tests. Decision: use TF-IDF sentence scoring. The file /data/docker/local-compact/app.py line {i*13} needs the scoring function. Ran `pytest -q` which passed 12 tests. Note: llama.cpp prefix cache requires deterministic output."})
    msgs.append({"role": "tool", "content": f"output of command {i}: 340 lines, exit 0. warning: DeprecationWarning at line {i}\ncompiled ok"})
    msgs.append({"role": "assistant", "content": f"Error: connection refused on port {8000+i} during probe; retried and it worked. Also error: 'docker compose config' failed due to a typo; fixed by correcting the yaml."})
    if i % 5 == 0:
        msgs.append({"role": "user", "content": f"Also make sure section {i} handles streaming responses and keeps the tail verbatim."})
msgs.append({"role": "assistant", "content": "Final: service works, summary below.", "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "terminal", "arguments": "{\"command\": \"docker compose ps\", \"cd\": \"/data/docker/local-compact\"}"}}]})
msgs.append({"role": "user", "content": "Please summarize the conversation so far so the agent can continue with the work. Include the task, decisions made, files touched, and current state."})
open("/tmp/lc-payload.json", "w").write(json.dumps({"model": "local-compact", "messages": msgs, "max_tokens": 4000}))
EOF

for port in 8001 8002; do
    curl -s "localhost:$port/v1/chat/completions" -H 'Content-Type: application/json' -d @/tmp/lc-payload.json \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["choices"][0]["message"]["content"])' > "/tmp/lc-$port.out"
done
diff /tmp/lc-8001.out /tmp/lc-8002.out >/dev/null \
    && echo "PARITY: byte-identical" \
    || { echo "PARITY: differs"; diff /tmp/lc-8001.out /tmp/lc-8002.out; exit 1; }
