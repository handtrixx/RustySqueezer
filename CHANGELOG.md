# Changelog

All notable changes to `rustysqueezer`. Dates are commit dates on `main`.

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
