# local-compact

**Status: proof of concept, with a Rust port.** The `rust/` directory is a
byte-for-byte port of `app.py` (axum, single binary, parity-tested against the
Python service); both are deployed-ready and CI-built. Not hardened, no
production deployment guarantees. Generic configuration (any agent harness),
deterministic token counting and an evaluation harness landed 2026-10-28.

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
4. each section is budgeted separately (see `section_budgets`): the verbatim
   tail gets the largest slice, and if a section runs out of budget its surplus
   items are dropped with an explicit `(+N more ... omitted by the budget)`
   note instead of the whole summary being clipped at the end.
5. optionally appends "highlights" from a small local model if
   `ASSIST_ENABLED=true` (any OpenAI-compatible endpoint: llama.cpp, Ollama, vLLM).

Same input always produces byte-identical output, which keeps prefix caches warm.

## Generic configuration (for other harnesses)

Detection rules, error/file patterns, enabled sections and section titles are
configurable through a JSON file (see `config.example.json`), pointed at with
the `CONFIG_PATH` environment variable - so the compactor can be tuned for
Claude Code, OpenClaw or custom pipelines instead of Zed's prompt shape:

```json
{
  "instruction_patterns": ["wrap up", "shrink the transcript"],
  "sections": ["original_task", "commands", "tail"],
  "section_titles": {"commands": "Commands executed"}
}
```

Unknown keys are ignored, `section_titles` and `section_budgets` merge partially,
and the `MAX_*` / `TAIL_*` env variables (see below) still override the file.
`section_budgets` are percent shares of `max_summary_chars`, normalised over the
sections that are actually enabled (dropping a section hands its budget to the
rest). Defaults equal the original Zed-tuned behaviour, so parity with previous
builds is kept.

```text
agent ──► local-compact ──► structured summary (~5-10% of the original thread)
```

## Images and deployment

CI (`.github/workflows/build-image.yml`) builds the images on every push to `main`
and publishes them to GitHub Container Registry, so deployments never build from
source:

| tag | contents |
|---|---|
| `ghcr.io/handtrixx/local-compact:latest` | Rust binary from `rust/Dockerfile` |
| `ghcr.io/handtrixx/local-compact:<sha>` | the same, pinned to the commit |
| `ghcr.io/handtrixx/local-compact:python` | Python PoC from `Dockerfile` |

On the homelab host the deploy directory `/data/docker/local-compact` holds only
`compose.yml`, `.env` and `README.md` - it pulls `ghcr.io/handtrixx/local-compact:latest`
and runs container `local-compact` (internal only, no public route). The package is
private, so the host needs `docker login ghcr.io` once. The service is registered in
LiteLLM as model `local-compact` so it can be selected as a Zed `compaction_model`;
it is not enabled by default yet.

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

### Rust port (`rust/`)

Same behaviour, same environment variables, byte-identical summaries:

```bash
cd rust
cargo build --release            # or: docker compose up -d (rust/compose.yml)
./target/release/local-compact   # listens on :8000, PORT env honoured
```

Parity check against the Python service (start `app.py` on :8001, the Rust
binary on :8002, then `./rust/parity_test.sh`).

Smoke test (synthetic ~11k-character thread, checks determinism):

```bash
python test_thread.py          # against app.py
cargo test                     # in rust/: determinism + section tests
```

## Configuration

Everything is environment driven (see `.env.example`):

### Token counting

`usage` reports real deterministic token counts, not the old `chars / 4`
estimate: every run of ASCII alphanumerics/apostrophes is one token and every
other non-whitespace character counts as one (implemented identically in both
languages, so Python and Rust agree). It is an approximation, not exact cl100k
BPE - good enough for budgeting and compression-ratio reporting.

## Configuration

Everything is environment driven (see `.env.example`):
|---|---|---|
| `MODEL_NAME` | `local-compact` | Model id exposed on `/v1/models` |
| `MAX_SUMMARY_CHARS` | `12000` | Hard cap on the summary length (split per section by `section_budgets`) |
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

**Zed** (`~/.config/zed/settings.json`) - declare the model under your
OpenAI-compatible provider with a large window, then point compaction at it:

```json
{
  "language_models": {
    "openai_compatible": {
      "MyGateway": {
        "api_url": "http://localhost:8000/v1",
        "api_key": "none",
        "available_models": [
          { "name": "local-compact", "max_tokens": 1000000, "capabilities": { "chat_completions": true } }
        ]
      }
    }
  },
  "agent": {
    "compaction_model": { "provider": "MyGateway", "model": "local-compact" },
    "thread_summary_model": { "provider": "MyGateway", "model": "local-compact" }
  }
}
```

## Evaluation harness

`eval/eval.py` runs synthetic threads with known needles (facts that must
survive compaction) against a live instance and reports needle recall,
compression ratio and determinism:

```bash
python eval/eval.py --url http://localhost:8000/v1          # local instance
python eval/eval.py --url https://ai.handtrixxx.com/v1      # via LiteLLM gateway
```

Exit code is non-zero when recall drops below the threshold, so it doubles as
a regression check before deploying an update.

## Limitations (PoC)

- Extractive, not abstractive: it can lose nuance in very long, meandering threads.
- Heuristics (regex for errors, sentence splitting) are tuned for code-agent traffic.
- Token counting is word-boundary based, not exact cl100k BPE.
- Only the OpenAI chat-completion shape is implemented (no Anthropic `/messages`, no embeddings).

## Roadmap

1. ~~Make it generic~~ - done 2026-10-28: `config.example.json` + `CONFIG_PATH`
   cover detection rules, error/file patterns, sections and titles (env vars
   still override). Section *templates* per harness are a possible next step.
2. ~~Rewrite in Rust~~ - done (`rust/`, axum + serde_json, single binary,
   ~3.5 MB, parity-tested).
3. ~~Real token counting~~ - done 2026-10-28 (deterministic word-boundary
   tokenizer, identical in Python and Rust; exact cl100k remains out of scope).
4. ~~Evaluation harness~~ - done 2026-10-28 (`eval/eval.py`: needle recall,
   compression ratio, determinism against any endpoint). Extended 2026-10-29 with
   a ~130k-token tool-heavy scenario plus ratio and budget assertions, and CI now
   runs the harness on every push.
5. ~~Per-section budgeting~~ - done 2026-10-29: sections get explicit budget
   slices (tail first) so long threads no longer lose the verbatim tail to an
   end-of-summary clip.
6. Anthropic Messages API + Ollama native surface - postponed.
7. Quality tuning against real agent threads (the harness now makes this
   measurable).

## License

MIT
