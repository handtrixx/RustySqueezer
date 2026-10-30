---
title: Evaluation
description: How compaction quality is measured, and how to measure it on your own threads.
---

# Evaluation

Compaction quality is measured as **needle recall**: the harness plants known
facts in a synthetic thread, and the summary must still contain them. The
harness exits non-zero if any scenario drops needles, which is how CI guards
against regressions.

```bash
python3 eval/eval.py --url http://localhost:8000/v1
python3 eval/surfaces.py --url http://localhost:8000
```

## Scenarios

| Scenario | What it stresses |
| --- | --- |
| `code-agent` | Typical tool-call heavy thread |
| `generic-harness` | Plain chat turns, no tool calls |
| `long-noisy` | Lots of irrelevant chatter |
| `long-realistic` | ~190k tokens, ratio and budget assertions |
| `million-token` | ~1M tokens in a single 6.1 MB request body |

Recall threshold is 0.95 by default (`--threshold` to change it).

## Surface parity

`eval/surfaces.py` checks that the OpenAI, Anthropic (including the native
`compaction` block), and Ollama surfaces all return byte-identical summaries
and sane usage counts.

## Your own threads

`eval/real_thread.py` runs a captured transcript through the service so you can
see what the summariser actually keeps for *your* workload:

```bash
python3 eval/real_thread.py \
  --url http://localhost:8000/v1 \
  --transcript thread.json \
  --max-chars 12000 \
  --dump summary.md
```

!!! tip "Tune against a real thread, not a synthetic one"
    The defaults are tuned for code-agent traffic. Before changing
    `max_commands`, `tail_turns` or the section budgets, run a real transcript
    through `real_thread.py` and compare the summary against what you actually
    needed the agent to remember.
