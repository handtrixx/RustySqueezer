# Changelog

All notable changes to `rustysqueezer`. Dates are commit dates on `main`.

## v1.2.3 - 2026-11-14
- **Gemini usage counts.** The Gemini REST response carries token counts under
  `usageMetadata`, not `usage`, so forwarded Google requests logged `in=0
  out=0`. `usage_tokens()` now reads the provider's container
  (`usageMetadata` for google, `usage` for OpenAI-compatible upstreams).

## v1.2.2 - 2026-11-14
- **Tail budget weights the newest turn (roadmap item 15).** The flat
  per-turn cap clipped the final assistant answer mid-sentence while the
  tail section still had headroom. The tail budget is now split by
  position: the newest turn gets half of the allocation, earlier tail
  turns share the rest; TAIL_CHARS stays a floor per turn. Verified by
  unit test (final answer survives intact, older turns clipped shorter).

## v1.2.1 - 2026-11-14
- **TLS for HTTPS upstreams.** reqwest was compiled without a TLS connector
  (default-features off, only json), so forwarding mode and the Google
  assist backend failed against any https endpoint ("error sending request")
  while plain http to the local aux server worked. Added reqwest's
  rustls-tls feature: embedded webpki roots, no system CA store needed on
  scratch. Verified live: forwarding to an https gateway upstream now returns
  the model's reply instead of falling back extractively.

## v1.2.0 - 2026-11-14
- **Google LLM backend for the assist.** `assist_provider: "google"` condenses
  the key-point bullets via the Gemini REST API (`assist_api_key`, base URL
  optional); the local llama.cpp dialect stays the default. Same fallback as
  before: any failed chunk keeps its extractive bullets.
- **Forwarding mode (roadmap item 18).** `forward_base_url` /
  `forward_provider: "google"` + `forward_model` pass the request through to a
  real model instead of answering extractively, so a narrative Google summary
  works for OpenCode/OpenChamber compaction. Off by default; on any upstream
  failure (timeout, HTTP error, unusable body) the extractive summary is
  produced and the failure is logged. Tool schemas are sanitized to the
  keyword subset Gemini accepts (`exclusiveMinimum`/`exclusiveMaximum` become
  inclusive integer bounds, `$ref`s inlined, draft-2020-12 keywords dropped) -
  OpenCode v2.3.0 sends session tool schemas with its compaction request and
  Gemini rejects them outright. For the native Google provider tools are
  dropped instead (a compaction answer never calls tools).
- **Template headings (item 16).** `template_headings: true` echoes the
  harness template's own `## ...` heading lines from the compaction
  instruction instead of the crate titles; off keeps Zed byte-identical.
- **File list drops URL tokens (item 13).** `//opencode.ai/config.json` and
  `wiki.niklas-stephan.de/page.md` have a domain-like first segment and are
  no longer listed as files.
- **Errors not duplicated in key points (item 14).** A sentence that repeats
  a line already listed under Errors is dropped from the key-point pool.
- `/health` reports `forward`, `forward_provider` and `assist_provider`.

## v1.1.3 - 2026-11-13
- **Objective recovery from contaminated checkpoints.** Real contamination
  (observed on a 14-round OpenChamber session) embeds the previous checkpoint
  in *other* sections (commands, key points), not only in the objective: the
  extraction now drills into every embedded `<conversation-checkpoint>` span
  and takes the first non-boilerplate objective found.

## v1.1.2 - 2026-11-13
- **Checkpoint-wrapper contamination guard.** Pre-v1.1.1 rounds stored the
  harness preamble (`The following is a summary and serialized record...`,
  clipped) as the objective, which hijacked every later round. Boilerplate is
  now rejected: extraction returns None instead of re-storing it, and the
  first-user-message fallback skips wrapper text. Depth cap 8 to 24 so long
  chains stay recoverable.
- **File list drops library names.** Context7 docs probes (`Express.js`,
  `Three.js`, `Next.js`, `Wiki.js`) hit the extension pattern and were listed
  as files; matches that are capitalized without a path separator are skipped.

## v1.1.1 - 2026-11-12
- **Checkpoint flattening.** Harnesses that re-inject the previous summary
  (OpenChamber/OpenCode wrap it in `<conversation-checkpoint>`) previously
  caused compaction round N+1 to embed round N verbatim as the objective, N+2
  to embed N+1, and the user's first request was lost mid-word by the clip.
  Messages matching `checkpoint_pattern` are now treated as carried state: the
  objective section is extracted and recursed to the deepest original request;
  checkpoint spans are stripped from the errors/files pools so old boilerplate
  cannot re-contaminate them; the previous checkpoint's key-point bullets are
  carried into the key-point pool (weight 0.8, below assistant prose).
- **Follow-up filter.** Short acknowledgements ("it works", "thanks", "ok")
  no longer appear in the follow-ups section, which some harnesses title as
  "Requirements". Configurable via `followup_skip_pattern`.
- **Assist timeout raised 15 s → 45 s.** rsq-aux serves `--parallel 3` but one
  GPU serialises the batches, so with 3 chunks the 2nd and 3rd always timed
  out at 15 s (observed `[assist] chunks=3 ok=0 ms=15001`); 45 s covers the
  slowest queued chunk.
- `clip` now cuts at the last word boundary instead of mid-word
  (was producing fragments like "(Co...").

## v1.1.0 - 2026-11-11
- **Small-model assist (optional, off by default).** The key-points bullets
  selected by the extractive pre-pass can be condensed chunk by chunk by a
  local llama.cpp server (`ASSIST_BASE_URL`, default model `qwen3.5-2b`):
  chunks are sent concurrently with `temperature 0` and a fixed seed, and the
  condensed bullets are merged back into the same section budget. Tail,
  commands, files, errors, follow-ups stay byte-verbatim. On any chunk
  failure - timeout (`ASSIST_TIMEOUT_MS`, default 15 s), connection error or
  unusable output - that chunk keeps its original extractive bullets, so the
  summary never degrades below v1.0.6 behaviour. `/health` reports
  `assist`/`assist_model`, `[assist]` lines log chunk count and latency.
- `reqwest` re-added (plain HTTP only, no TLS stack): the aux server lives on
  the internal docker network.
- Config keys `assist_base_url`, `assist_model`, `assist_timeout_ms`,
  `assist_max_chunks`, `assist_chunk_chars`, `assist_max_tokens` (config file
  and env; env wins).

## v1.0.6 - 2026-10-30
- **Anthropic native compaction surface.** `POST /v1/messages` requests that
  carry a `compaction` directive (exactly what LiteLLM's gateway-native
  `context_compaction` router sends) now return a single Anthropic compaction
  block - `{"type": "compaction", "content": <summary>,
  "signature": "rustysqueezer"}` with `stop_reason: "compaction"` - instead
  of a plain text block, so the gateway can splice the summary into the
  parent thread. Detected on the body key (the child request is always
  non-streaming). Covered by `eval/surfaces.py`.

## v1.0.5 - 2026-10-30
- **1M-token sessions verified.** Request body limit widened from 32 MiB to
  64 MiB; a 1,004,041-token thread (6.1 MB of JSON) compacts in ~0.1 s inside
  the container and ~17 s through the LiteLLM gateway, with needle recall and
  determinism intact. Covered by a `million-token` eval scenario and a unit
  test, both in CI.
- Roadmap item 12 (resource limits in the deployment compose) dropped: the
  compactor is stateless and stays around 4 MB RSS even while serving six
  concurrent 1M-token requests, so CPU/memory caps in the deploy compose were
  judged unnecessary.

## v1.0.4 - 2026-10-30
- **32 MiB request body limit** (`DefaultBodyLimit::max`): the Axum default
  2 MiB rejected large agent threads with `413 Failed to buffer request body`.
- **Graceful shutdown** (`with_graceful_shutdown` on SIGINT/SIGTERM): `docker
  stop` now drains in-flight compactions instead of aborting them.
- **Per-request usage logging**: every compaction logs surface, input/output
  token counts, compression ratio and latency to stdout.
- **Unit tests run in CI** (`cargo test` in the pinned `rust:1.99-alpine`
  toolchain on every push).

## v1.0.2 - 2026-10-29
Reliability pass driven by a real Zed compaction transcript
(`eval/real_thread.py --transcript test.md`):
- **Tool results feed the key-point pool.** Raw tool output is the ground
  truth of a real thread (build logs, health checks, eval numbers), but its
  lines are rarely grammatical sentences. Fact-signal lines (versions, IPs,
  token counts, `healthy`/`PASS`/`FAIL`, exit codes) are now extracted at a
  lower weight than assistant prose.
- **Near-duplicate key points filtered out.** Paraphrases of the same fact
  (three commit-log lines, repeated "surfaces PASS") no longer squeeze out
  distinct facts; a candidate is skipped when >=75 % of its content tokens are
  already covered by an accepted point.
- **Sections keep fitting line by line** instead of stopping at the first
  overlong line, so one verbose sentence cannot block several shorter ones.
- **Budgets rebalanced**: `key_points` share raised (10 -> 40 relative), tail
  reduced (50 -> 30) since it is already capped by `tail_chars`.
- **Tighter detection**: error pattern requires real failure context (no more
  "great error messages" or a quoted `timeout: 5s` reported as an error); file
  regex requires a word boundary so domains like `github.com` are not mistaken
  for `github.c`.
- New `eval/real_thread.py`: parses a real transcript into the exact message
  shape Zed sends (assistant `tool_calls` + `tool` results), checks needle
  recall, determinism, budget compliance, tail preservation and surface parity.
  Result on the real thread: 22k -> 2.9k tokens (13 %), 23/25 needles, PASS.

## v1.0.2 - 2026-10-29
- **Anthropic Messages surface** (`POST /v1/messages`): accepts the top-level
  `system` string and content-block messages, responds in Anthropic message
  format (`content: [{type: "text", ...}]`, `usage: {input_tokens, output_tokens}`),
  including proper SSE streaming (`message_start`, `content_block_delta`, `message_end`).
- **Ollama native surface** (`POST /api/chat`, `POST /api/generate`,
  `GET /api/tags`): `response` + `done` payloads, NDJSON streaming, and raw-prompt
  compaction via `/api/generate`. All surfaces return byte-identical summaries
  for the same thread; `eval/surfaces.py` verifies shape, usage counts and
  parity and runs in CI.
- Docs site rebuilt with BeerCSS (Material Design 3 layout, teal primary).

## v1.0.1 - 2026-10-29
- **Hardened image.** Builder is now a pinned `rust:1.99-alpine` toolchain
  producing a fully static musl binary (`crt-static`), so the runtime image
  is `scratch`: no shell, no package manager, no `curl`, runs as a
  non-root user. Image size drops from ~90 MB to ~10 MB.
- **Shell-free healthcheck.** The binary accepts `--health` and probes its
  own `/health` over plain TCP; compose/Docker healthchecks use it directly
  (`test: ["CMD", "/rustysqueezer", "--health"]`).
- **Automatic versioning on every build.** CI reads the crate version from
  `Cargo.toml` and publishes the image as `:latest`, `:<sha>`, `:<version>`
  and `:v<version>`, and creates the matching `v<version>` git tag if it
  does not exist yet. Deployments should pin the version tag (e.g. `1.0.1`)
  instead of `:latest`.
- **Dependabot** watches the pinned Docker base image and `Cargo.lock`
  weekly; bumps arrive as PRs that CI builds and runs the eval harness on.

## v1.0.0 - 2026-10-29
- **Generative assist removed.** The optional `ASSIST_*` path (a small local LLM
  writing "highlights") was measured and dropped: 12-24 s of CPU latency per
  compaction, non-deterministic output, and at a 2 % budget the model's output was
  mostly reasoning preamble. Embeddings (EmbeddingGemma-2) were evaluated as an
  alternative sentence selector and rejected as well (~30 s per 190k-token thread,
  and cosine similarity cannot separate a repeated fact from a contradiction).
  `reqwest` is no longer a dependency.
- **Key points are now de-duplicated** (normalised, case/whitespace/punctuation
  insensitive) so repeated tool chatter no longer eats the key-points budget.


- **Renamed**: the project, crate, container and model id are now
  **RustySqueezer** (`rustysqueezer` for ids/tags). The repo was renamed on
  GitHub (old URL redirects), CI now publishes
  `ghcr.io/handtrixx/rustysqueezer`, and the old `local-compact` GHCR package
  was deleted.
- Documentation cleanup for public release: README restructured (why / how it
  works / API / configuration / evaluation / deployment / agent wiring /
  limitations / roadmap), this CHANGELOG added. The repository is now public.
- **Rust is the only implementation.** The Python PoC (`app.py`, its
  Dockerfile and compose file, `test_thread.py`, `rust/parity_test.sh` and the
  CI `python` job) was removed; the stdlib-only eval harness in `eval/` stays.
  The old `:python` image tag is no longer published.
- Per-section budgeting: `MAX_SUMMARY_CHARS` is split across sections by
  `section_budgets` (tail 50 %, commands 12 %, follow-ups 10 %, key points
  10 %, errors 8 %, files 8 %, original task 6 %, tools 4 %),
  normalised over enabled sections. Overflow is reported as
  `(+N more ... omitted by the budget)` instead of clipping the whole summary;
  the tail is filled newest-first. Commands are clipped to 200 characters,
  paths to 120.
- Evaluation harness extended with a ~190k-token tool-heavy scenario plus
  compression-ratio and budget assertions, and a CI job that builds the Rust
  image and runs the harness on every push.

## 2026-10-28

- Generic configuration: `CONFIG_PATH` + `config.example.json` for detection
  patterns, error/file patterns, enabled sections, titles and budgets; env vars
  still override.
- Deterministic word-boundary token counting for `usage` (not exact cl100k).
- `eval/eval.py`: needle recall, compression ratio, determinism against any
  OpenAI-compatible endpoint.
- Rust port (axum + serde_json, single ~3.5 MB binary, unit tests) verified
  against the Python PoC with a parity test before the PoC was dropped.
- CI publishes prebuilt images to GHCR (`:latest` and `:<sha>`) using
  `docker/login-action` + `docker/build-push-action`.
- Initial proof of concept: OpenAI-compatible compaction endpoint that builds an
  extractive summary of a thread without calling a model.
