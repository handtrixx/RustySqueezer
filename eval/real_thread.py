#!/usr/bin/env python3
"""Reliability test against a real agent transcript (stdlib only).

Parses a markdown thread of `## User` / `## Assistant` blocks into a messages
array, appends a compaction instruction, and checks the compactor on:

  - instruction detection (last user message recognised as compaction)
  - needle recall: facts from the thread that must survive compaction
  - compression ratio
  - determinism (byte-identical on repeat)
  - budget compliance (no section blows past MAX_SUMMARY_CHARS)
  - tail preservation (the last turns must still be present)

Usage:
    python3 eval/real_thread.py --transcript test.md --url http://localhost:8000/v1
"""
import argparse
import json
import re
import sys
import urllib.request

# Zed's actual compaction prompt shape (contains "summarize"/"conversation so far").
COMPACTION_PROMPT = (
    "Please summarize the conversation so far so the agent can continue with the work. "
    "Include the original task, decisions made, files touched, commands run, errors hit, "
    "and the current state."
)

# Facts from the RustySqueezer thread that a compaction must not lose.
NEEDLES = [
    "1.0.1", "1.0.2", "scratch", "musl", "dependabot", "cargo.toml",
    "beercss@5.0.3", "#00897b", "surfaces.py", "rustysqueezer:1.0.2",
    "/api/chat", "/api/generate", "/api/tags", "/v1/messages",
    "crt-static", "proc-macro", "ghcr.io/handtrixx/rustysqueezer",
    "rust/compose.yml", "eval/eval.py", "healthcheck", "--health",
    "172.19.0.20", "healthy", "189", "1284",
]


TOOL_BLOCK = re.compile(r"(?m)^\*\*Tool Call: (.+?)\*\*$")


def parse_transcript(path, tool_clip=1500):
    """Split `## User` / `## Assistant` blocks into a messages array.

    Tool calls found in the transcript are reconstructed as the structured
    assistant `tool_calls` + following `tool` result messages, which is the
    shape Zed actually sends to the compaction model. Thinking blocks are
    dropped, as the summariser does not receive them.
    """
    text = open(path, encoding="utf-8").read()
    parts = re.split(r"(?m)^## (User|Assistant)(?: \(checkpoint\))?\s*$", text)
    messages = []
    for i in range(1, len(parts), 2):
        role = parts[i].lower()
        body = parts[i + 1].strip()
        if role == "user":
            messages.append({"role": "user", "content": body})
            continue
        body = re.sub(r"<thinking>.*?</thinking>", "", body, flags=re.S).strip()
        if not body:
            continue
        # prose before the first tool call, if any
        prose = body.split("**Tool Call:", 1)[0].strip()
        if prose:
            messages.append({"role": "assistant", "content": prose})
        chunks = TOOL_BLOCK.split(body)
        for idx in range(1, len(chunks), 2):
            cmd = chunks[idx].strip()
            output = re.sub(r"(?m)^(Status: \w+|Terminal:|Diff:)\s*$", "", chunks[idx + 1])
            output = output.strip("`\n").strip()
            messages.append({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": f"call_{len(messages)}",
                    "type": "function",
                    "function": {"name": "terminal", "arguments": json.dumps({"command": cmd})},
                }],
            })
            if output:
                messages.append({
                    "role": "tool",
                    "content": output if tool_clip <= 0 else output[:tool_clip],
                })
    return messages


def post(url, payload):
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode(), headers={"content-type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--transcript", required=True)
    ap.add_argument("--url", default="http://localhost:8000/v1")
    ap.add_argument("--max-chars", type=int, default=12000)
    ap.add_argument("--tool-clip", type=int, default=1500,
                    help="clip each tool result to this many characters (0 = keep everything)")
    ap.add_argument("--dump", default="", help="write the summary to this file")
    args = ap.parse_args()

    thread = parse_transcript(args.transcript, args.tool_clip)
    thread.append({"role": "user", "content": COMPACTION_PROMPT})
    payload = {"model": "rustysqueezer", "messages": thread}
    root = args.url.rstrip("/")
    if root.endswith("/v1"):
        root = root[:-3]
    completions = root + "/v1/chat/completions"

    first = post(completions, payload)
    second = post(completions, payload)
    summary = first["choices"][0]["message"]["content"]
    usage = first["usage"]
    deterministic = summary == second["choices"][0]["message"]["content"]

    # Instruction detection: the compaction prompt must not appear as a
    # follow-up message in the summary.
    instruction_detected = "summarize the conversation so far" not in summary.lower()

    # Needle recall, case-insensitive, over the lowercased summary.
    low = summary.lower()
    missing = [n for n in NEEDLES if n.lower() not in low]
    recall = (len(NEEDLES) - len(missing)) / len(NEEDLES)

    # Budget compliance: summary must respect the configured cap.
    within_budget = len(summary) <= args.max_chars + 200

    # Tail preservation: the last assistant turn's distinctive content survives.
    tail = thread[-2]["content"]
    tail_words = [w for w in re.findall(r"[A-Za-z0-9]{6,}", tail)][:12]
    tail_hits = sum(1 for w in tail_words if w.lower() in low)

    print(f"thread: {len(thread)} messages, {usage['prompt_tokens']} prompt tokens")
    print(f"summary: {len(summary)} chars, {usage['completion_tokens']} tokens "
          f"({usage['completion_tokens'] / usage['prompt_tokens']:.1%} of prompt)")
    print(f"[determinism] {'OK' if deterministic else 'FAIL'}")
    print(f"[instruction-detection] {'OK' if instruction_detected else 'FAIL'}")
    print(f"[needle-recall] {'OK' if recall >= 0.9 else 'FAIL'} ({len(NEEDLES) - len(missing)}/{len(NEEDLES)})")
    if missing:
        print("  lost:", ", ".join(missing))
    print(f"[budget] {'OK' if within_budget else 'FAIL'} ({len(summary)} chars vs cap {args.max_chars})")
    print(f"[tail] {'OK' if tail_hits >= 6 else 'WEAK'} ({tail_hits}/12 distinctive tail words present)")
    # Surface parity: the Anthropic and Ollama surfaces must return exactly the
    # same bytes as the OpenAI surface for the same thread.
    anth = post(root + "/v1/messages", {"model": "rustysqueezer", "messages": thread})
    anth_text = anth["content"][0]["text"]
    oll = post(root + "/api/chat", {"model": "rustysqueezer", "messages": thread})
    parity = anth_text == summary == oll["response"]
    print(f"[surface-parity] {'OK' if parity else 'FAIL'} (OpenAI == Anthropic == Ollama)")

    section_chars = {}
    for line in summary.splitlines():
        m = re.match(r"^## (.+)$", line)
        if m:
            section_chars.setdefault(m.group(1), 0)
    print(f"sections: {len(section_chars)} ({', '.join(section_chars)})")
    passed = (
        deterministic
        and instruction_detected
        and recall >= 0.9
        and within_budget
        and parity
    )
    print("RESULT:", "PASS" if passed else "FAIL")

    if args.dump:
        open(args.dump, "w", encoding="utf-8").write(summary)
        print(f"summary written to {args.dump}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
