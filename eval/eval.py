#!/usr/bin/env python3
"""Evaluation harness for local-compact.

Measures compression quality against synthetic threads with known "needles"
(facts that must survive compaction), plus compression ratio and
determinism. Run against any local-compact instance:

    python eval/eval.py --url http://localhost:8000/v1
    python eval/eval.py --url https://ai.handtrixxx.com/v1 --model local-compact

Exits non-zero if any scenario drops needles (recall < threshold), so it
can be used as a regression check in CI or before deploying an update.
"""
import argparse
import json
import sys
import urllib.request


def count_tokens(text):
    """Mirror the service's deterministic tokenizer (see app.py)."""
    tokens = 0
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch.isascii() and (ch.isalnum() or ch == "'"):
            while i < n and text[i].isascii() and (text[i].isalnum() or text[i] == "'"):
                i += 1
            tokens += 1
        elif not ch.isspace():
            tokens += 1
            i += 1
        else:
            i += 1
    return tokens


def call_compactor(url, model, messages):
    payload = {"model": model, "messages": messages, "max_tokens": 4000}
    req = urllib.request.Request(
        url.rstrip("/") + "/chat/completions",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = json.loads(resp.read())
    return data["choices"][0]["message"]["content"], data.get("usage", {})


def assistant_turn(text, command=None, path=None):
    msg = {"role": "assistant", "content": text}
    if command or path:
        args = {}
        if command:
            args["command"] = command
        if path:
            args["path"] = path
        msg["tool_calls"] = [
            {
                "id": "call",
                "type": "function",
                "function": {"name": "terminal", "arguments": json.dumps(args)},
            }
        ]
    return msg


def scenario_code_agent():
    """Typical Zed-style code-agent thread."""
    msgs = [
        {"role": "system", "content": "You are the Zed coding agent. Follow AGENTS.md."},
        {
            "role": "user",
            "content": (
                "Wire the local-compact compactor into the LiteLLM gateway and "
                "deploy it on handtrixxx.com. Decision: use the Rust binary image "
                "pulled from GHCR, never build on the host."
            ),
        },
    ]
    for i in range(12):
        msgs.append(
            assistant_turn(
                f"Step {i}: editing the compactor and running checks; the scoring "
                "function needs a recency bonus for sentences with digits.",
                command=f"pytest -q test_thread_{i}.py",
                path=f"/data/github/local-compact/eval/case_{i}.json",
            )
        )
        msgs.append(
            assistant_turn(
                f"Error: needle-err-{i} connection refused on port 800{i}; fixed "
                "by restarting the container and retrying once."
            )
        )
        if i % 4 == 0:
            msgs.append(
                {"role": "user", "content": f"Also keep the tail verbatim in section {i}."}
            )
    msgs.append(assistant_turn("Final state: compactor deployed, health endpoint ok."))
    msgs.append(
        {
            "role": "user",
            "content": "Please summarize the conversation so far so the agent can "
            "continue with the work. Include the task, decisions, files, commands.",
        }
    )
    needles = [
        "Rust binary image",  # original task (first user message)
        "pytest -q test_thread_11.py",  # most recent command
        "eval/case_7.json",  # file path from a tool call
        "needle-err-9",  # error line mid-thread
        "compactor deployed, health endpoint ok",  # verbatim tail
    ]
    return "code-agent", msgs, needles


def scenario_generic_harness():
    """Other harnesses: different compaction wording and tool shapes."""
    msgs = [
        {"role": "system", "content": "You are a research agent."},
        {"role": "user", "content": "Compare three reverse proxies for the homelab and pick one."},
        assistant_turn(
            "Reading the Caddy docs first.",
            command=None,
        ),
    ]
    msgs[-1]["tool_calls"] = [
        {
            "id": "c1",
            "type": "function",
            "function": {"name": "read_file", "arguments": "{\"file_path\": \"/etc/caddy/Caddyfile\"}"},
        }
    ]
    msgs.append(
        assistant_turn(
            "Traefik failed the health probe twice: needle-traefik-timeout on startup.",
            command="traefik check",
        )
    )
    msgs.append(assistant_turn("Decision: Caddy, needle-caddy-choice, for simplicity."))
    msgs.append(
        {
            "role": "user",
            "content": "Condense this transcript for the next agent run. Keep decisions and paths.",
        }
    )
    needles = [
        "/etc/caddy/Caddyfile",
        "needle-traefik-timeout",
        "needle-caddy-choice",  # final assistant turn kept verbatim in tail
        "traefik check",
    ]
    return "generic-harness", msgs, needles


def scenario_long_noisy():
    """60 turns of filler; needles buried mid-thread in tool facts."""
    msgs = [
        {"role": "user", "content": "Migrate the mail server to Stalwart, needle-stalwart-task."},
    ]
    for i in range(60):
        msgs.append(
            assistant_turn(
                f"Working on subtask {i}. Lots of routine probing and retrying here.",
                command=f"run-check-{i}",
                path=f"/tmp/probe-{i}.log",
            )
        )
        if i == 37:
            msgs.append(
                assistant_turn(
                    "Error: needle-migration-key at line 42 of stalwart.toml blocked startup."
                )
            )
    msgs.append(assistant_turn("Done: migration verified with a test send."))
    needles = [
        "needle-stalwart-task",  # original task
        "run-check-59",  # commands cap keeps the most recent 25
        "run-check-35",  # still within the 25-command window
        "needle-migration-key",  # error line
        "/tmp/probe-30.log",  # within the files cap (60)
    ]
    return "long-noisy", msgs, needles


def run_scenario(url, model, name, messages, needles, threshold):
    summary, usage = call_compactor(url, model, messages)
    summary2, _ = call_compactor(url, model, messages)
    deterministic = summary == summary2
    hits = [n for n in needles if n in summary]
    recall = len(hits) / len(needles)
    prompt_tokens = sum(count_tokens(m.get("content", "")) for m in messages)
    summary_tokens = count_tokens(summary)
    ratio = summary_tokens / max(1, prompt_tokens)
    ok = recall >= threshold and deterministic
    print(f"[{name}] recall {recall:.0%} ({len(hits)}/{len(needles)}) | "
          f"{prompt_tokens} -> {summary_tokens} tokens ({ratio:.1%} kept) | "
          f"deterministic: {deterministic}")
    missing = [n for n in needles if n not in hits]
    if missing:
        print(f"  missing needles: {missing}")
    if usage:
        print(f"  service usage: prompt={usage.get('prompt_tokens')} "
              f"completion={usage.get('completion_tokens')}")
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", default="http://localhost:8000/v1")
    ap.add_argument("--model", default="local-compact")
    ap.add_argument("--threshold", type=float, default=0.95)
    args = ap.parse_args()

    all_ok = True
    for name, messages, needles in (
        scenario_code_agent(),
        scenario_generic_harness(),
        scenario_long_noisy(),
    ):
        all_ok &= run_scenario(args.url, args.model, name, messages, needles, args.threshold)

    print("RESULT:", "PASS" if all_ok else "FAIL")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
