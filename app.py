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
import sys
import time
import urllib.request
from collections import Counter

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

# Behaviour is configurable through a JSON config file (CONFIG_PATH env) so the
# compactor can serve harnesses other than Zed: detection rules, error/file
# patterns, enabled sections and section titles. Environment variables (see
# .env.example) override the config file; the defaults reproduce the original
# Zed-tuned behaviour, so parity with the older builds is kept.
DEFAULT_CONFIG = {
    "model_name": "local-compact",
    "max_summary_chars": 12000,
    "tail_turns": 6,
    "tail_chars": 600,
    "max_commands": 25,
    "max_files": 60,
    "max_errors": 20,
    "max_key_points": 40,
    "assist_url": "",
    "assist_model": "local-small",
    "assist_enabled": False,
    "assist_max_input_chars": 2500,
    "assist_max_tokens": 500,
    "instruction_patterns": [
        "summariz|summarise|compact|condense|conversation so far|context window"
    ],
    "instruction_max_chars": 4000,
    "error_pattern": (
        r"(?:^|\s)(?:error|errors|failed|fails|exception|traceback|denied|exit code|cannot|unable|not found|timeout)"
    ),
    "file_extensions": [
        "py", "rs", "ts", "tsx", "js", "jsx", "json", "ya?ml", "toml", "md",
        "sh", "bash", "html", "css", "sql", "txt", "log", "lock", "go", "cpp",
        "c", "h", "Dockerfile",
    ],
    # Enabled sections in output order; drop one to omit it from the summary.
    "sections": [
        "original_task", "followups", "commands", "files", "tools",
        "errors", "key_points", "tail",
    ],
    "section_titles": {
        "header": "Conversation summary (deterministic compaction by local-compact)",
        "note": "(Compaction requested by the agent harness; the task below is the original request.)",
        "original_task": "Original task",
        "followups": "Follow-up user messages",
        "commands": "Commands run (most recent first)",
        "files": "Files/paths involved",
        "tools": "Tools used",
        "errors": "Errors / problems seen",
        "key_points": "Key points from the assistant",
        "tail": "Most recent turns (verbatim, clipped)",
        "assist": "Small-model highlights",
    },
}


def load_config():
    cfg = json.loads(json.dumps(DEFAULT_CONFIG))
    path = os.environ.get("CONFIG_PATH", "")
    if path:
        try:
            with open(path) as f:
                for key, value in json.load(f).items():
                    if isinstance(value, dict) and isinstance(cfg.get(key), dict):
                        cfg[key].update(value)  # partial overrides merge with defaults
                    else:
                        cfg[key] = value
        except (OSError, ValueError) as exc:
            print(f"local-compact: ignoring CONFIG_PATH {path}: {exc}", file=sys.stderr)
    if "MODEL_NAME" in os.environ:
        cfg["model_name"] = os.environ["MODEL_NAME"]
    for env, key in (
        ("MAX_SUMMARY_CHARS", "max_summary_chars"),
        ("TAIL_TURNS", "tail_turns"),
        ("TAIL_CHARS", "tail_chars"),
        ("MAX_COMMANDS", "max_commands"),
        ("MAX_FILES", "max_files"),
        ("MAX_ERRORS", "max_errors"),
        ("MAX_KEY_POINTS", "max_key_points"),
        ("ASSIST_MAX_INPUT_CHARS", "assist_max_input_chars"),
        ("ASSIST_MAX_TOKENS", "assist_max_tokens"),
    ):
        if env in os.environ:
            try:
                cfg[key] = int(os.environ[env])
            except ValueError:
                pass
    cfg["assist_url"] = os.environ.get("ASSIST_MODEL_URL", cfg["assist_url"])
    cfg["assist_model"] = os.environ.get("ASSIST_MODEL", cfg["assist_model"])
    cfg["assist_enabled"] = os.environ.get("ASSIST_ENABLED", "false").strip().lower() in ("1", "true", "yes")
    return cfg


CFG = load_config()

MODEL = CFG["model_name"]
MAX_SUMMARY_CHARS = int(CFG["max_summary_chars"])
TAIL_TURNS = int(CFG["tail_turns"])
TAIL_CHARS = int(CFG["tail_chars"])
MAX_COMMANDS = int(CFG["max_commands"])
MAX_FILES = int(CFG["max_files"])
MAX_ERRORS = int(CFG["max_errors"])
MAX_KEY_POINTS = int(CFG["max_key_points"])
ASSIST_URL = CFG["assist_url"]
ASSIST_MODEL = CFG["assist_model"]
ASSIST_ENABLED = bool(CFG["assist_enabled"])
ASSIST_MAX_INPUT_CHARS = int(CFG["assist_max_input_chars"])
ASSIST_MAX_TOKENS = int(CFG["assist_max_tokens"])

app = FastAPI(title=MODEL)

STOP = set(
    "the a an and or of to in for on with at by is are was were be been being "
    "it its this that these those we you i they he she from as into about "
    "then there here what which who how do does did not no yes can could should "
    "would will have has had but if so any some all more other such than also".split()
)

TOKEN_RE = re.compile(r"[A-Za-z0-9_./-]{3,}")
FILE_RE = re.compile(r"[\w./-]+\.(?:" + "|".join(CFG["file_extensions"]) + r")")
ERROR_RE = re.compile(CFG["error_pattern"], re.I)
COMPACT_RE = re.compile(
    "|".join("(?:%s)" % p for p in CFG["instruction_patterns"]), re.I
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
    return bool(COMPACT_RE.search(text.lower())) and len(text) < int(CFG["instruction_max_chars"])


def count_tokens(text):
    """Deterministic word-boundary token count used for `usage` reporting.

    Not exact cl100k BPE, but far closer to real tokenisation than the old
    chars/4 estimate: every run of ASCII alphanumerics/apostrophes is one
    token, and every other non-whitespace character (punctuation, non-ASCII)
    counts as one. Implemented identically in the Rust port so the two agree.
    """
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

    sections = CFG["sections"]
    titles = CFG["section_titles"]
    parts = []
    parts.append("## " + titles["header"])
    if instruction:
        parts.append(titles["note"])
    if "original_task" in sections and user_msgs:
        parts.append("\n## " + titles["original_task"] + "\n" + clip(msg_text(user_msgs[0]), 1500))
    if "followups" in sections and user_msgs[1:]:
        followups = "\n".join("- " + clip(msg_text(u), 300) for u in user_msgs[1:])
        parts.append("\n## " + titles["followups"] + "\n" + followups)
    if "commands" in sections and commands:
        parts.append("\n## " + titles["commands"] + "\n" + "\n".join(f"- `{c}`" for c in reversed(commands[-MAX_COMMANDS:])))
    if "files" in sections and files:
        parts.append("\n## " + titles["files"] + "\n" + "\n".join(f"- `{f}`" for f in dedup_keep_order(files, MAX_FILES)))
    if "tools" in sections and tools:
        counts = Counter(tools)
        parts.append("\n## " + titles["tools"] + "\n" + ", ".join(f"{t} ({n})" for t, n in counts.most_common(10)))
    errors = []
    for m in thread:
        for line in msg_text(m).splitlines():
            if ERROR_RE.search(line) and 10 <= len(line.strip()) <= 300:
                errors.append(line.strip())
    errors = dedup_keep_order(errors, MAX_ERRORS)
    if "errors" in sections and errors:
        parts.append("\n## " + titles["errors"] + "\n" + "\n".join("- " + e for e in errors))
    all_sents = [s for m in assistant_msgs for s in sentence_list(msg_text(m))]
    key = sorted(score_sentences(all_sents, task_terms, recency_weight=2.0), reverse=True)[:MAX_KEY_POINTS]
    if "key_points" in sections and key:
        parts.append("\n## " + titles["key_points"] + "\n" + "\n".join("- " + clip(s, 300) for _, s in key))
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
    if "tail" in sections:
        parts.append("\n## " + titles["tail"] + "\n" + "\n\n".join(tail_lines))
    if ASSIST_ENABLED:
        assist = assist_summarize("\n".join(msg_text(m) for m in thread))
        if assist:
            parts.append("\n## " + titles["assist"] + "\n" + assist)
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
    prompt_tokens = sum(count_tokens(msg_text(m)) for m in messages)
    completion_tokens = count_tokens(summary)
    usage = {
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
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
