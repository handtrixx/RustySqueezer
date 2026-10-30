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
| `tail_turns` / `tail_chars` | 6 / 600 | How many recent turns are kept verbatim, and how far each is clipped |
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

## Environment overrides

| Variable | Overrides |
| --- | --- |
| `PORT` | Listen port (default 8000) |
| `MODEL_NAME` | `model_name` |
| `MAX_SUMMARY_CHARS` | `max_summary_chars` |
| `TAIL_TURNS` / `TAIL_CHARS` | tail settings |
| `MAX_COMMANDS` / `MAX_FILES` / `MAX_ERRORS` / `MAX_KEY_POINTS` | section caps |

!!! note "Determinism is not affected by tuning"
    Changing budgets changes *how much* is kept, not *which* content is chosen
    at random. The same input with the same config always produces byte-identical
    output.
