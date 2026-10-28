"""local-compact: a deterministic "fake LLM" that answers Zed agent context
compaction requests without any real model.

Zed compaction is just one chat completion whose prompt says "summarize this
conversation so work can continue". This service implements the OpenAI
chat-completion surface (/v1/chat/completions, /v1/models, /health), detects
such requests, and returns a structured extractive summary built from the
thread itself: task statement, timeline, tool facts (commands/paths), errors,
key assistant sentences (TF-IDF-style selection), and the most recent turns
verbatim. Optional ASSIST_ENABLED delegates the "what is relevant" judgement
to a tiny local llama.cpp model.

Deterministic: same input -> byte-identical output (helps llama.cpp prefix
caching when Zed re-compacts).
"""
import json
import os
import re
import time
import urllib.request
from collections import Counter

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

# All behaviour is driven by environment variables (see .env.example).
def _int(name, default):
    try:
        return int(os.environ.get(name, default))
    except ValueError:
        return default


MODEL = os.environ.get("MODEL_NAME", "local-compact")
MAX_SUMMARY_CHARS = _int("MAX_SUMMARY_CHARS", 12000)
TAIL_TURNS = _int("TAIL_TURNS", 6)
TAIL_CHARS = _int("TAIL_CHARS", 600)
MAX_COMMANDS = _int("MAX_COMMANDS", 25)
MAX_FILES = _int("MAX_FILES", 60)
MAX_ERRORS = _int("MAX_ERRORS", 20)
MAX_KEY_POINTS = _int("MAX_KEY_POINTS", 40)
# Optional generative assist: any OpenAI-compatible endpoint (llama.cpp, ollama,
# vLLM, ...). Disabled by default so the service is pure and deterministic.
ASSIST_URL = os.environ.get("ASSIST_MODEL_URL", "")
ASSIST_MODEL = os.environ.get("ASSIST_MODEL", "local-small")
ASSIST_ENABLED = os.environ.get("ASSIST_ENABLED", "false").lower() in ("1", "true")
ASSIST_MAX_INPUT_CHARS = _int("ASSIST_MAX_INPUT_CHARS", 2500)
ASSIST_MAX_TOKENS = _int("ASSIST_MAX_TOKENS", 500)

app = FastAPI(title=MODEL)

STOP = set(
    "the a an and or of to in for on with at by is are was were be been being "
    "it its this that these those we you i they he she from as into about "
    "then there here what which who how do does did not no yes can could should "
    "would will have has had but if so any some all more other such than also".split()
)

TOKEN_RE = re.compile(r"[A-Za-z0-9_./-]{3,}")
FILE_RE = re.compile(
    r"[\w./-]+\.(?:py|rs|ts|tsx|js|jsx|json|ya?ml|toml|md|sh|bash|html|css|sql|txt|log|lock|go|cpp|c|h|Dockerfile)"
)
ERROR_RE = re.compile(
    r"(?:^|\s)(?:error|errors|failed|fails|exception|traceback|denied|exit code|cannot|unable|not found|timeout)",
    re.I,
)


def msg_text(msg):
    """Return plain text of a message, handling string and multipart content."""
    c = msg.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return "\n".join(
            p.get("text", "") for p in c if isinstance(p, dict) and p.get("type") == "text"
        )
    return "" if c is None else str(c)


def is_compaction_instruction(text):
    t = text.lower()
    return bool(
        re.search(r"summariz|summarise|compact|condense|conversation so far|context window", t)
    ) and len(text) < 4000


def sentence_list(text):
    # Split on newlines first, then on sentence ends; keep order.
    out = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        for s in re.split(r"(?<=[.!?])\s+", line):
            s = s.strip()
            if len(s) >= 20:
                out.append(s)
    return out


def term_counts(text):
    return Counter(t for t in TOKEN_RE.findall(text.lower()) if t not in STOP)


def tool_facts(thread):
    """Extract commands, file paths and tool names from assistant tool_calls."""
    commands, files, tools = [], [], []
    for m in thread:
        for tc in m.get("tool_calls") or []:
            fn = tc.get("function", {})
            name = fn.get("name", "")
            if name:
                tools.append(name)
            try:
                args = json.loads(fn.get("arguments", "{}"))
            except Exception:
                args = {}
            if isinstance(args, dict):
                cmd = args.get("command")
                if isinstance(cmd, str) and cmd.strip():
                    commands.append(cmd.strip())
                for key in ("path", "file_path", "p", "target"):
                    val = args.get(key)
                    if isinstance(val, str) and val.strip():
                        files.append(val.strip())
        for m2 in FILE_RE.findall(msg_text(m)):
            files.append(m2)
    return commands, files, tools


def dedup_keep_order(items, cap):
    seen, out = set(), []
    for it in items:
        if it not in seen:
            seen.add(it)
            out.append(it)
            if len(out) >= cap:
                break
    return out


def score_sentences(sents, task_terms, recency_weight=0.0):
    scored = []
    for i, s in enumerate(sents):
        toks = term_counts(s)
        hit = sum(1 for t in toks if t in task_terms)
        bonus = 0.0
        if re.search(r"\d", s):
            bonus += 0.5
        if FILE_RE.search(s):
            bonus += 1.0
        if ERROR_RE.search(s):
            bonus += 0.5
        scored.append((hit + bonus + recency_weight * (i / max(1, len(sents))), s))
    return scored


def clip(text, n):
    text = text.strip()
    return text if len(text) <= n else text[: n - 3] + "..."


def assist_summarize(text):
    """Optional: ask the tiny local llama.cpp model for bullet points.
    Input is clipped so the 16k-window aux model can always handle it."""
    if not ASSIST_ENABLED or not ASSIST_URL:
        return ""
    payload = {
        "model": ASSIST_MODEL,
        "messages": [
            {
                "role": "system",
                "content": "You are a context compactor. Output terse bullet points of the most important facts, decisions, file paths, commands and results. No commentary.",
            },
            {"role": "user", "content": clip(text, ASSIST_MAX_INPUT_CHARS)},
        ],
        # Keep the budget small so a CPU-hosted assist model stays fast.
        "max_tokens": ASSIST_MAX_TOKENS,
    }
    try:
        req = urllib.request.Request(
            ASSIST_URL.rstrip("/") + "/chat/completions",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=40) as resp:
            data = json.loads(resp.read())
        msg = data["choices"][0]["message"]
        # Small thinking models can run out of tokens inside reasoning and leave
        # content empty; fall back to reasoning_content.
        text = (msg.get("content") or "") or msg.get("reasoning_content") or ""
        return text.strip()
    except Exception:
        return ""  # deterministic fallback: skip the assist section


def build_summary(messages):
    thread = list(messages)
    # Zed appends the compaction instruction as a final user message.
    instruction = ""
    if thread and thread[-1].get("role") == "user" and is_compaction_instruction(msg_text(thread[-1])):
        instruction = msg_text(thread.pop())

    system = ""
    for m in thread:
        if m.get("role") == "system":
            system += msg_text(m)
    user_msgs = [m for m in thread if m.get("role") == "user"]
    assistant_msgs = [m for m in thread if m.get("role") == "assistant"]

    commands, files, tools = tool_facts(thread)
    task_terms = set(term_counts(msg_text(user_msgs[0]) if user_msgs else system))
    for m in user_msgs[1:]:
        task_terms.update(t for t in term_counts(msg_text(m)) if len(t) > 4)

    parts = []
    parts.append("## Conversation summary (deterministic compaction by local-compact)")
    if instruction:
        parts.append("(Compaction requested by the agent harness; the task below is the original request.)")
    if user_msgs:
        parts.append("\n## Original task\n" + clip(msg_text(user_msgs[0]), 1500))
    if user_msgs[1:]:
        followups = "\n".join("- " + clip(msg_text(u), 300) for u in user_msgs[1:])
        parts.append("\n## Follow-up user messages\n" + followups)
    if commands:
        parts.append("\n## Commands run (most recent first)\n" + "\n".join(f"- `{c}`" for c in reversed(commands[:MAX_COMMANDS])))
    if files:
        parts.append("\n## Files/paths involved\n" + "\n".join(f"- `{f}`" for f in dedup_keep_order(files, MAX_FILES)))
    if tools:
        counts = Counter(tools)
        parts.append("\n## Tools used\n" + ", ".join(f"{t} ({n})" for t, n in counts.most_common(10)))
    errors = []
    for m in thread:
        for line in msg_text(m).splitlines():
            if ERROR_RE.search(line) and 10 <= len(line.strip()) <= 300:
                errors.append(line.strip())
    errors = dedup_keep_order(errors, MAX_ERRORS)
    if errors:
        parts.append("\n## Errors / problems seen\n" + "\n".join("- " + e for e in errors))
    all_sents = [s for m in assistant_msgs for s in sentence_list(msg_text(m))]
    key = sorted(score_sentences(all_sents, task_terms, recency_weight=2.0), reverse=True)[:MAX_KEY_POINTS]
    if key:
        parts.append("\n## Key points from the assistant\n" + "\n".join("- " + clip(s, 300) for _, s in key))
    # Verbatim tail: the turns the agent actually needs to continue from.
    tail = thread[-TAIL_TURNS:]
    tail_lines = []
    for m in tail:
        role = m.get("role", "?")
        text = clip(msg_text(m), TAIL_CHARS)
        if m.get("tool_calls"):
            calls = "; ".join(tc.get("function", {}).get("name", "") for tc in m["tool_calls"])
            text = (text + f" [tool calls: {calls}]").strip()
        tail_lines.append(f"[{role}] {text}")
    parts.append("\n## Most recent turns (verbatim, clipped)\n" + "\n\n".join(tail_lines))
    if ASSIST_ENABLED:
        assist = assist_summarize("\n".join(msg_text(m) for m in thread))
        if assist:
            parts.append("\n## Small-model highlights\n" + assist)
    summary = "\n".join(parts)
    if len(summary) > MAX_SUMMARY_CHARS:
        summary = summary[: MAX_SUMMARY_CHARS - 20] + "\n[summary truncated]"
    return summary


@app.get("/health")
def health():
    return {
        "status": "ok",
        "model": MODEL,
        "assist_enabled": ASSIST_ENABLED,
        "max_summary_chars": MAX_SUMMARY_CHARS,
        "tail_turns": TAIL_TURNS,
    }


@app.get("/v1/models")
def models():
    return {"object": "list", "data": [{"id": MODEL, "object": "model"}]}


@app.post("/v1/chat/completions")
async def completions(request: Request):
    body = await request.json()
    messages = body.get("messages", [])
    stream = bool(body.get("stream"))
    summary = build_summary(messages)
    usage = {
        "prompt_tokens": sum(len(msg_text(m)) for m in messages) // 4,
        "completion_tokens": len(summary) // 4,
        "total_tokens": (sum(len(msg_text(m)) for m in messages) + len(summary)) // 4,
    }
    if not stream:
        return JSONResponse(
            {
                "id": MODEL,
                "object": "chat.completion",
                "created": int(time.time()),
                "model": body.get("model", MODEL),
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": summary},
                        "finish_reason": "stop",
                    }
                ],
                "usage": usage,
            }
        )
    def sse():
        chunk = 2000
        for i in range(0, len(summary), chunk):
            yield "data: " + json.dumps(
                {"choices": [{"index": 0, "delta": {"role": "assistant", "content": summary[i : i + chunk]}}]}
            ) + "\n\n"
        yield "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}) + "\n\n"
        yield "data: [DONE]\n\n"
    return StreamingResponse(sse(), media_type="text/event-stream")
