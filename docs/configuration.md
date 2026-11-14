---
title: Configuration
description: Tuning the extraction heuristics with CONFIG_PATH and environment variables.
---

# Configuration

RustySqueezer's extraction rules are tunable without recompiling. Mount a JSON
file with `CONFIG_PATH`, or override individual knobs with environment
variables (env wins over the file).

## Mount a config file

```bash
docker run -d -p 8000:8000 \
  -v ./my-config.json:/config.json:ro \
  -e CONFIG_PATH=/config.json \
  ghcr.io/handtrixx/rustysqueezer:1.0.6
```

The canonical set of keys is in
[`config.example.json`](https://github.com/handtrixx/RustySqueezer/blob/main/config.example.json).

## Keys

| Key | Default | Meaning |
| --- | --- | --- |
| `model_name` | `rustysqueezer` | Reported model id on discovery endpoints |
| `max_summary_chars` | 12000 | Hard cap on the generated summary |
| `tail_turns` / `tail_chars` | 6 / 600 | How many recent turns are kept verbatim; `tail_chars` is a per-turn floor, and the newest turn gets half of the tail section's budget (v1.2.2) |
| `max_commands` | 25 | Shell commands retained (most recent first) |
| `max_files` | 60 | File paths retained |
| `max_errors` | 20 | Error lines retained |
| `max_key_points` | 40 | Salient assistant sentences retained |
| `instruction_patterns` | regex list | Phrases that identify a compaction instruction (Zed, `/compact`, "summarize the conversation so far") |
| `instruction_max_chars` | 4000 | Treat the matched instruction as content if longer than this |
| `error_pattern` | regex | What counts as an error line |
| `file_extensions` | list | Extensions considered source files |
| `sections` | ordered list | Which summary sections to emit, in order |
| `section_budgets` | shares | Per-section share of `max_summary_chars` (tail first) |
| `section_titles` | strings | Headline text for each section |
| `template_headings` | false | Echo the harness template's own `##` headings from the compaction instruction (OpenCode validates them verbatim) |
| `assist_provider` | `openai` | Backend dialect for the key-point condensation: `openai` (llama.cpp etc.) or `google` (Gemini REST) |
| `assist_base_url` | `""` | OpenAI-compatible server for the assist; empty = assist off |
| `assist_api_key` | `""` | API key for provider `google`; empty = assist off (base URL then optional) |
| `assist_model` | `qwen3.5-2b` | Assist model id (`gemini-3-flash` for Google) |
| `assist_timeout_ms` / `assist_max_chunks` / `assist_chunk_chars` / `assist_max_tokens` | 45000 / 3 / 700 / 140 | Assist chunking and per-chunk limits |
| `forward_provider` | `openai` | Forwarding dialect: `openai` or `google`; forwarding is off unless the provider's credential and `forward_model` are set |
| `forward_base_url` | `""` | OpenAI-compatible upstream for forwarding |
| `forward_api_key` | `""` | API key for forwarding with provider `google` |
| `forward_model` | `""` | Upstream model to forward to; empty = forwarding off |
| `forward_timeout_ms` / `forward_max_tokens` | 90000 / 2048 | Upstream timeout and answer cap |

## Small-model assist

The key-points section may be condensed by a second, small model
(map-reduce over bullet chunks). `assist_provider` selects the dialect:

- `openai` - a local llama.cpp server, e.g. `assist_base_url:
  "http://rsq-aux:8080/v1"` with `assist_model: "qwen3.5-2b"`.
- `google` - the Gemini REST API, e.g. `assist_provider: "google"`,
  `assist_api_key: "AIza..."`, `assist_model: "gemini-3-flash"`; the base URL
  defaults to `https://generativelanguage.googleapis.com/v1`.

Keys are read from the config file or `ASSIST_API_KEY` / `ASSIST_BASE_URL`
env and are never logged. A failed or timed-out chunk keeps its original
extractive bullets, so the summary is never worse than the pure extractive
build.

## Forwarding mode

With `forward_model` plus the provider's credential (`forward_base_url` or
`forward_api_key`) set, requests are passed through to a real model instead
of being answered extractively - useful when a harness wants a *narrative*
compaction (OpenCode/OpenChamber `/compact` pointing at Gemini). Tool schemas
are sanitized to the keyword subset Gemini accepts (`exclusiveMinimum`/
`exclusiveMaximum` become inclusive integer bounds, local `$ref`s inlined,
draft-2020-12 keywords like `$schema`, `additionalProperties`,
`patternProperties` dropped); on the native Google provider tools are dropped
outright. Any upstream failure (timeout, HTTP error, unusable body) falls
back to the extractive summary, so the endpoint never fails because of the
upstream. Forwarding stays off by default: the extractive path remains the
local default.

## Environment overrides

| Variable | Overrides |
| --- | --- |
| `PORT` | Listen port (default 8000) |
| `MODEL_NAME` | `model_name` |
| `MAX_SUMMARY_CHARS` | `max_summary_chars` |
| `TAIL_TURNS` / `TAIL_CHARS` | tail settings |
| `MAX_COMMANDS` / `MAX_FILES` / `MAX_ERRORS` / `MAX_KEY_POINTS` | section caps |
| `ASSIST_BASE_URL` / `ASSIST_PROVIDER` / `ASSIST_API_KEY` / `ASSIST_MODEL` | assist backend |
| `FORWARD_BASE_URL` / `FORWARD_PROVIDER` / `FORWARD_API_KEY` / `FORWARD_MODEL` | forwarding upstream |
| `TEMPLATE_HEADINGS` | `1`/`true` enables template heading echo |

!!! note "Determinism is not affected by tuning"
    Changing budgets changes *how much* is kept, not *which* content is chosen
    at random. The same input with the same config always produces byte-identical
    output.
