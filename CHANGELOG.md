# Changelog

All notable changes to `rustysqueezer`. Dates are the commit dates on `main`.

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
