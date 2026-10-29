# Changelog

All notable changes to `local-compact`. Dates are the commit dates on `main`.

## 2026-10-29

- Documentation cleanup for public release: README restructured (why / how it
  works / API / configuration / evaluation / deployment / agent wiring /
  limitations / roadmap), this CHANGELOG added. The repository is now public.
- **Rust is the only implementation.** The Python PoC (`app.py`, its
  Dockerfile and compose file, `test_thread.py`, `rust/parity_test.sh` and the
  CI `python` job) was removed; the stdlib-only eval harness in `eval/` stays.
  The old `:python` image tag is no longer published.
- Per-section budgeting: `MAX_SUMMARY_CHARS` is split across sections by
  `section_budgets` (tail 50 %, commands 12 %, follow-ups 10 %, key points
  10 %, errors 8 %, files 8 %, original task 6 %, tools 4 %, assist 2 %),
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
