import json, urllib.request

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

body = json.dumps({"model": "local-compact", "messages": msgs, "max_tokens": 4000}).encode()
req = urllib.request.Request("http://localhost:8000/v1/chat/completions", data=body, headers={"Content-Type": "application/json"})
with urllib.request.urlopen(req, timeout=15) as r:
    out = json.loads(r.read())
print("prompt chars:", sum(len(str(m.get('content',''))) for m in msgs))
print("usage:", out["usage"])
print(out["choices"][0]["message"]["content"][:3500])
# determinism check
req2 = urllib.request.Request("http://localhost:8000/v1/chat/completions", data=body, headers={"Content-Type": "application/json"})
with urllib.request.urlopen(req2, timeout=15) as r:
    out2 = json.loads(r.read())
print("DETERMINISTIC:", out["choices"][0]["message"]["content"] == out2["choices"][0]["message"]["content"])
