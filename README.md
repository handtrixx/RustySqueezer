# local-compact

[![build-image](https://github.com/handtrixx/local-compact/actions/workflows/build-image.yml/badge.svg)](https://github.com/handtrixx/local-compact/actions/workflows/build-image.yml)

**A fake LLM for context compaction.** `local-compact` exposes an OpenAI-compatible
HTTP endpoint that answers compaction requests with a deterministic, extractive
summary of the conversation - **without calling any language model**.

It is a working proof of concept, not a hardened product: no auth, no rate
limiting, no clustering/abstraction. The compaction itself is solid and
evaluated (see [Evaluation](#evaluation)).

```text
agent ──► POST /v1/chat/completions ──► structured summary (~5-10% of the thread)
```

## Why this exists

Agents compact a long thread by sending the *whole* thread to a summarising
model. That request must fit inside the summariser's context window, so a local
model with a modest window (e.g. 131k tokens) cannot compact its own threads -
you end up paying a 1M-context cloud model for every compaction event.

The insight: **compaction does not need a model.** It needs a good extractive
summary of what was asked, what was run, what broke and what was just said.
`local-compact` does exactly that, in a few milliseconds, for free, and always
produces byte-identical output for the same input (which keeps prefix caches
warm).

## How it works

`POST /v1/chat/completions` parses `messages` and builds a markdown summary:

1. **Detect the compaction request.** If the last user message matches
   `instruction_patterns` (Zed, Claude Code-style `/compact`, generic
   "summarize the conversation so far"), it is treated as the instruction and
   removed from the thread.
2. **Split the thread** into the original task, follow-up user messages and
   assistant turns.
3. **Extract facts from tool calls** (`tool_calls` / tool results): commands
   run, files and paths touched, tool names.
4. **Pick key assistant sentences** scored by task-term overlap plus recency
   (a TF-IDF-ish heuristic, no model).
5. **Collect error lines** with a configurable regex.
6. **Append the verbatim tail** - the last `TAIL_TURNS` turns, clipped - so the
   agent can continue exactly where it stopped.
7. *(optional)* add "highlights" from a small local model when
   `ASSIST_ENABLED=true` (any OpenAI-compatible endpoint: llama.cpp, Ollama,
   vLLM). If the assist call fails, the section is skipped - the output stays
   deterministic.

Every section has its own slice of `MAX_SUMMARY_CHARS` (`section_budgets`). The
verbatim tail gets the largest share; when a section runs out of budget its
surplus items are dropped with an explicit
`(+N more ... omitted by the budget)` note, so long threads never lose the tail
to an end-of-summary clip.

### Sections and default budgets

| section | default share | what it contains |
|---|---|---|
| `tail` | 50 % | most recent turns, verbatim (newest first, clipped) |
| `commands` | 12 % | shell commands actually run, most recent first |
| `followups` | 10 % | later user messages |
| `key_points` | 10 % | highest-scoring assistant sentences |
| `errors` | 8 % | error/failure lines |
| `files` | 8 % | file paths mentioned by tools |
| `original_task` | 6 % | the first user message |
| `tools` | 4 % | distinct tool names |
| `assist` | 2 % | optional small-model highlights |

Shares are percentages of `MAX_SUMMARY_CHARS`, normalised over the sections that
are actually enabled (disabling a section hands its budget to the rest).

## Quick start

```bash
cd rust
cargo run                                   # or: docker compose up -d
curl -s localhost:8000/health
curl -s localhost:8000/v1/models
```

## API

| method | path | behaviour |
|---|---|---|
| `GET` | `/health` | `{"status":"ok","model":"local-compact"}` |
| `GET` | `/v1/models` | the single model id (`MODEL_NAME`) |
| `POST` | `/v1/chat/completions` | returns the summary as the assistant message |

`/v1/chat/completions` accepts the usual OpenAI shape (`model`, `messages`,
`stream`) and answers non-streaming JSON or SSE (`stream: true`, 2000-char
deltas, then `[DONE]`). It is a compactor, not a general chat model: any request
is answered with a summary of the thread it was given.

```bash
curl -s localhost:8000/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"local-compact","messages":[
    {"role":"user","content":"Fix the nginx upstream timeout in docker/traefik"},
    {"role":"assistant","content":"I ran `docker compose up -d`; it failed with exit code 1."},
    {"role":"user","content":"Please summarize the conversation so far"}
  ]}'
```

`usage` reports token counts from the deterministic tokenizer described below,
so `completion_tokens / prompt_tokens` is a real compression ratio.

## Configuration

Two layers, both optional:

- **Environment variables** (see `.env.example`) - the simple knobs.
- **A JSON config file** (`CONFIG_PATH=...`, see `config.example.json`) -
  detection rules, error/file patterns, enabled sections, titles and budgets.
  Unknown keys are ignored, partial maps merge, and the `MAX_*` / `TAIL_*` env
  variables still win over the file.

| variable | default | meaning |
|---|---|---|
| `PORT` | `8000` | listen port |
| `MODEL_NAME` | `local-compact` | model id on `/v1/models` |
| `CONFIG_PATH` | unset | JSON config file (see above) |
| `MAX_SUMMARY_CHARS` | `12000` | hard cap on the summary, split per section |
| `TAIL_TURNS` / `TAIL_CHARS` | `6` / `600` | verbatim tail: how many turns, how long each |
| `MAX_COMMANDS` / `MAX_FILES` / `MAX_ERRORS` / `MAX_KEY_POINTS` | `25` / `60` / `20` / `40` | per-section item caps |
| `ASSIST_ENABLED` | `false` | add highlights from a small local model |
| `ASSIST_MODEL_URL` | empty | OpenAI-compatible base URL of the assist model |
| `ASSIST_MODEL` | `local-small` | assist model id |
| `ASSIST_MAX_INPUT_CHARS` / `ASSIST_MAX_TOKENS` | `2500` / `500` | budget for the assist call |

### Token counting

Counts are deterministic: every run of ASCII alphanumerics/apostrophes is one
token, every other non-whitespace character counts as one. Not exact cl100k BPE
- good enough for budgeting and comparable compression ratios across runs.

## Evaluation

`eval/eval.py` (stdlib only, no dependencies) replays synthetic threads with
known needles - facts that must survive compaction - against a live instance and
reports needle recall, compression ratio, determinism and budget compliance:

```bash
python3 eval/eval.py --url http://localhost:8000/v1
```

Scenarios include a ~190k-token tool-heavy thread; the harness fails (non-zero
exit) if recall drops below the threshold or a section exceeds its budget, so it
doubles as a regression gate before deploying an update.

## Deployment

CI (`.github/workflows/build-image.yml`) builds on every push to `main` and
publishes to GitHub Container Registry, so deployments never build from source:

| tag | contents |
|---|---|
| `ghcr.io/handtrixx/local-compact:latest` | Rust binary from `rust/Dockerfile` |
| `ghcr.io/handtrixx/local-compact:<sha>` | the same, pinned to the commit |

The package is private, so pulling it needs `docker login ghcr.io` once.
`rust/compose.yml` is a ready-made single-service compose file; the binary is
~3.5 MB and starts instantly.

```bash
docker run -d -p 8000:8000 -e MAX_SUMMARY_CHARS=12000 ghcr.io/handtrixx/local-compact:latest
```

## Wiring it into an agent

The point is to make the compaction step cheap, so plug it in wherever your
harness picks a summariser.

**Zed** (`~/.config/zed/settings.json`) - declare the endpoint as an
OpenAI-compatible model with a large window, then point compaction at it:

```json
{
  "language_models": {
    "openai_compatible": {
      "LocalCompact": {
        "api_url": "http://localhost:8000/v1",
        "api_key": "none",
        "available_models": [
          { "name": "local-compact", "max_tokens": 1000000, "capabilities": { "chat_completions": true } }
        ]
      }
    }
  },
  "agent": {
    "compaction_model": { "provider": "LocalCompact", "model": "local-compact" },
    "thread_summary_model": { "provider": "LocalCompact", "model": "local-compact" }
  }
}
```

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

Keep a real model as fallback: the compactor is extractive, so for threads where
nuance matters a genuine summariser can still be better.

## Development

```bash
cargo test                                   # in rust/: determinism, budgets, sections
python3 eval/eval.py --url http://localhost:8000/v1
```

Layout: `rust/src/main.rs` (axum routes), `rust/src/summary.rs` (the compactor),
`rust/src/text.rs` (tokenizing, clipping, scoring), `rust/src/config.rs`
(env + JSON config), `eval/eval.py` (harness).

## Limitations

- Extractive, not abstractive: nuance can be lost in very long, meandering threads.
- Heuristics (error regex, sentence splitting, file patterns) are tuned for
  code-agent traffic; retune them via `config.json` for other harnesses.
- Token counting is word-boundary based, not exact cl100k BPE.
- Only the OpenAI chat-completion surface is implemented - no Anthropic
  `/messages`, no embeddings, no tool calling.
- No authentication: run it on a private network or behind a reverse proxy.

## Roadmap

1. ~~Make it generic~~ - done 2026-10-28 (`CONFIG_PATH` + `config.example.json`).
2. ~~Rewrite in Rust~~ - done 2026-10-28; the Python PoC was removed 2026-10-29
   and Rust is the only implementation (Python remains only as the eval harness).
3. ~~Deterministic token counting~~ - done 2026-10-28.
4. ~~Evaluation harness~~ - done 2026-10-28, extended 2026-10-29 with ratio and
   budget assertions and wired into CI.
5. ~~Per-section budgeting~~ - done 2026-10-29.
6. Anthropic Messages API and Ollama native surface - open.
7. Quality tuning against real agent threads (the harness makes this measurable).

## License

MIT - see [LICENSE](LICENSE). History is in [CHANGELOG.md](CHANGELOG.md).
