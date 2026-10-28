# local-compact

**Status: proof of concept.** Not hardened, no tests beyond a smoke script, no production deployment guarantees.

`local-compact` is a *fake LLM*: an OpenAI-compatible HTTP endpoint that answers
context-compaction requests **deterministically**, without calling any language
model. It exists so IDE/agent workflows (Zed agent, Claude Code-style `/compact`,
any agent that compacts by asking a model to summarise the thread) do not need a
large-context external API just to shrink a conversation.

## Why

Agents compact a long thread by sending the *whole* thread to a summarising model.
That request has to fit in that model's context window, so local models with a
modest window (e.g. 131k tokens) cannot compact their own threads - you end up
paying for a 1M-context cloud model for every compaction event.

The insight: **compaction does not need a model.** It needs a good extractive
summary. `local-compact` implements the chat-completion surface, recognises the
compaction prompt, and builds the summary from the thread itself.

## How it works

For every `POST /v1/chat/completions` it:

1. detects a compaction-style instruction (last user message contains
   "summarize / compact / condense the conversation so far");
2. parses the thread and emits a structured markdown summary:
   - original task + follow-up user messages
   - commands run and files/paths touched (extracted from `tool_calls`)
   - tools used, error/failure lines
   - key assistant sentences, scored by task-term overlap + recency (TF-IDF-ish)
   - the last `TAIL_TURNS` turns verbatim (clipped) so the agent can continue
3. optionally appends "highlights" from a small local model if
   `ASSIST_ENABLED=true` (any OpenAI-compatible endpoint: llama.cpp, Ollama, vLLM).

Same input always produces byte-identical output, which keeps prefix caches warm.

```text
agent ──► local-compact ──► structured summary (~5-10% of the original thread)
```

## Quick start

```bash
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
uvicorn app:app --host 0.0.0.0 --port 8000

curl -s localhost:8000/health
curl -s localhost:8000/v1/models
```

Or with Docker:

```bash
docker compose up -d
```

Smoke test (synthetic ~11k-character thread, checks determinism):

```bash
python test_thread.py
```

## Configuration

Everything is environment driven (see `.env.example`):

| Variable | Default | Meaning |
|---|---|---|
| `MODEL_NAME` | `local-compact` | Model id exposed on `/v1/models` |
| `MAX_SUMMARY_CHARS` | `12000` | Hard cap on the summary length |
| `TAIL_TURNS` / `TAIL_CHARS` | `6` / `600` | Verbatim tail: how many turns, how long each |
| `MAX_COMMANDS` / `MAX_FILES` / `MAX_ERRORS` / `MAX_KEY_POINTS` | `25` / `60` / `20` / `40` | Per-section caps |
| `ASSIST_ENABLED` | `false` | Use a small local model for extra highlights |
| `ASSIST_MODEL_URL` | empty | OpenAI-compatible base URL of the assist model |
| `ASSIST_MODEL` | `local-small` | Assist model id |
| `ASSIST_MAX_INPUT_CHARS` / `ASSIST_MAX_TOKENS` | `2500` / `500` | Budget for the assist call |

## Wiring it into an agent

**LiteLLM** (`litellm_config.yaml`):

```yaml
- model_name: local-compact
  litellm_params:
    model: openai/local-compact
    api_base: http://localhost:8000/v1
    api_key: none
  model_info:
    max_input_tokens: 1000000
```

**Zed** (`~/.config/zed/settings.json`):

```json
{
  "agent": {
    "compaction_model": { "provider": "openai_compatible", "model": "local-compact" },
    "thread_summary_model": { "provider": "openai_compatible", "model": "local-compact" }
  }
}
```

Declare the model under the provider with a large `max_tokens` so Zed never
refuses the oversized compaction request.

## Limitations (PoC)

- Extractive, not abstractive: it can lose nuance in very long, meandering threads.
- Heuristics (regex for errors, sentence splitting) are tuned for code-agent traffic.
- Token counts in `usage` are `chars / 4` estimates, not real tokenisation.
- Only the OpenAI chat-completion shape is implemented (no Anthropic `/messages`, no embeddings).

## Roadmap

1. **Make it generic** - one config file (or `.env`) covering the detection rules,
   section templates and output format, so it works out of the box for other agent
   harnesses (Claude Code, OpenClaw, custom pipelines) instead of being tied to
   Zed's compaction prompt.
2. **Rewrite in Rust** - `axum`/`actix` + a proper tokeniser, single static binary,
   negligible startup and memory footprint, easy sidecar deployment.
3. Real token counting (tiktoken-equivalent) instead of the chars/4 estimate.
4. Evaluation harness (needle-in-thread recall, SWE-bench-style fidelity) so
   compression quality is measurable.
5. Anthropic Messages API + Ollama native surface.

## License

MIT
