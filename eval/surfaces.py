#!/usr/bin/env python3
"""Anthropic Messages and Ollama surface tests (stdlib only).

Checks that /v1/messages and /api/chat(+generate, tags) produce the same
deterministic summary as /v1/chat/completions for the same thread, with the
correct response shape. Run against a live instance:

    python3 eval/surfaces.py --url http://localhost:8000
"""
import argparse
import json
import sys
import urllib.request

THREAD = [
    {"role": "system", "content": "You are a coding agent."},
    {"role": "user", "content": "Fix the nginx upstream timeout in docker/traefik and verify health."},
    {"role": "assistant", "content": "I ran docker compose up -d; failed with exit code 1: connection refused on port 8000."},
    {"role": "user", "content": "Also pin the image tag and rerun the eval."},
    {"role": "assistant", "content": "Pinned to 1.0.1, eval passed: RESULT: PASS, needle recall 100%."},
    {"role": "user", "content": "Please summarize the conversation so far so the agent can continue."},
]


def post(url, payload):
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode(), headers={"content-type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        raw = resp.read().decode()
    return raw


def openai_summary(base):
    raw = json.loads(post(base + "/v1/chat/completions", {"model": "rustysqueezer", "messages": THREAD}))
    return raw["choices"][0]["message"]["content"], raw["usage"]


def check(name, ok, detail=""):
    print(f"[{name}] {'OK' if ok else 'FAIL'} {detail}")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://localhost:8000", help="base URL of the service")
    args = ap.parse_args()
    base = args.url.rstrip("/")
    if base.endswith("/v1"):
        base = base[:-3]

    ref, ref_usage = openai_summary(base)
    results = []

    # --- Anthropic Messages API ---
    anth_msgs = [
        {"role": m["role"], "content": [{"type": "text", "text": m["content"]}]} for m in THREAD[1:]
    ]
    raw = json.loads(post(base + "/v1/messages", {
        "model": "claude-sonnet-4-5",
        "max_tokens": 4096,
        "system": "You are a coding agent.",
        "messages": anth_msgs,
    }))
    results.append(check("anthropic-shape", raw.get("type") == "message" and raw.get("role") == "assistant"))
    text = raw.get("content", [{}])[0].get("text", "")
    results.append(check("anthropic-parity", text == ref))
    results.append(check(
        "anthropic-usage",
        raw.get("usage", {}).get("input_tokens") == ref_usage["prompt_tokens"]
        and raw.get("usage", {}).get("output_tokens") == ref_usage["completion_tokens"],
    ))

    # Anthropic streaming: SSE with message_start/content_block_delta/message_end.
    raw = post(base + "/v1/messages", {
        "model": "claude-sonnet-4-5", "stream": True,
        "system": "You are a coding agent.", "messages": anth_msgs,
    })
    deltas = [json.loads(line[6:]) for line in raw.splitlines() if line.startswith("data:")]
    streamed = "".join(d.get("delta", {}).get("text", "") for d in deltas if "delta" in d)
    results.append(check("anthropic-stream-parity", streamed == ref and deltas[0].get("type") == "message_start"))

    # Anthropic native compaction (LiteLLM gateway context_compaction):
    # body carries compaction {"type": "summarize"}, child is always
    # non-streaming; response must be a single compaction block with a
    # non-empty signature and stop_reason "compaction".
    raw = json.loads(post(base + "/v1/messages", {
        "model": "claude-sonnet-5",
        "max_tokens": 4096,
        "compaction": {"type": "summarize"},
        "system": "You are a coding agent.",
        "messages": anth_msgs,
    }))
    blocks = raw.get("content", [])
    shape_ok = (
        raw.get("stop_reason") == "compaction"
        and len(blocks) == 1
        and blocks[0].get("type") == "compaction"
        and isinstance(blocks[0].get("signature"), str) and blocks[0].get("signature")
        and isinstance(blocks[0].get("content"), str) and blocks[0].get("content").strip()
    )
    results.append(check("anthropic-compaction-shape", shape_ok, raw.get("stop_reason", "")))
    results.append(check("anthropic-compaction-parity", shape_ok and blocks[0].get("content") == ref))

    # --- Ollama native surface ---
    ollama_msgs = [{"role": m["role"], "content": m["content"]} for m in THREAD]
    raw = json.loads(post(base + "/api/chat", {"model": "rustysqueezer", "messages": ollama_msgs, "stream": False}))
    results.append(check("ollama-shape", raw.get("done") is True and "response" in raw))
    results.append(check("ollama-parity", raw.get("response") == ref))
    results.append(check(
        "ollama-usage",
        raw.get("prompt_eval_count") == ref_usage["prompt_tokens"] and raw.get("eval_count") == ref_usage["completion_tokens"],
    ))

    # Ollama streaming: NDJSON lines, final done line.
    raw = post(base + "/api/chat", {"model": "rustysqueezer", "messages": ollama_msgs, "stream": True})
    lines = [json.loads(line) for line in raw.splitlines() if line.strip()]
    streamed = "".join(l.get("response", "") for l in lines)
    results.append(check("ollama-stream-parity", streamed == ref and lines[-1].get("done") is True))

    # /api/generate accepts a raw prompt string.
    prompt = "\n".join(f"{m['role']}: {m['content']}" for m in THREAD)
    raw = json.loads(post(base + "/api/generate", {"model": "rustysqueezer", "prompt": prompt}))
    results.append(check("ollama-generate", raw.get("done") is True and len(raw.get("response", "")) > 0))

    raw = json.loads(urllib.request.urlopen(base + "/api/tags", timeout=10).read().decode())
    results.append(check("ollama-tags", any(m.get("name") == "rustysqueezer" for m in raw.get("models", []))))

    print("RESULT:", "PASS" if all(results) else "FAIL")
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
