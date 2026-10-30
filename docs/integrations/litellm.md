---
title: LiteLLM
description: Use RustySqueezer as a LiteLLM model, or let the gateway compact threads before dispatch.
---

# LiteLLM gateway

LiteLLM plays two different roles, and they solve different problems.

## 1. RustySqueezer as a model

The simplest setup - the gateway just routes to it, exactly like any other
OpenAI-compatible backend. This is the analogue of Zed's `agent.compaction_model`:
whatever client you point at `rustysqueezer` gets the deterministic summariser.

```yaml
model_list:
  - model_name: rustysqueezer
    litellm_params:
      model: openai/rustysqueezer
      api_base: http://rustysqueezer:8000/v1
      api_key: none
    model_info:
      max_input_tokens: 1000000
```

!!! warning "Declare the window honestly - but generously"
    LiteLLM rejects requests above `max_input_tokens` before they ever reach
    RustySqueezer. Set it to the real ceiling you want to support (1M), not to
    the model's nominal size.

## 2. Gateway-side automatic compaction

LiteLLM's auto-router has native context management: when an incoming thread
approaches the declared window, it compacts the older turns with a chosen
compactor model *before* dispatching the trimmed request to the target model.
This is what makes RustySqueezer useful for clients that have no compaction
setting at all (VS Code Copilot, scripts, internal tools).

```yaml
model_list:
  - model_name: claude-target
    litellm_params:
      model: anthropic/claude-sonnet-5

  - model_name: rusty-compactor
    litellm_params:
      model: openai/rustysqueezer
      api_base: http://rustysqueezer:8000/v1
      api_key: none
    model_info:
      max_input_tokens: 1000000

router:
  auto_router:
    complexity_router:
      context_compaction:
        model: rusty-compactor
        trigger_ratio: 0.9
        max_tokens: 4096
        timeout_seconds: 120
```

The compaction trigger is:

```
floor(max_input_tokens * trigger_ratio) - max_output_tokens
```

With a 200k target window and `trigger_ratio: 0.9`, anything above roughly
176k tokens is compacted before the real model sees it.

!!! important "Verified end to end (2026-10-30)"
    This path exercises RustySqueezer's **Anthropic compaction surface**: the
    auto-router sends a `{"compaction": {"type": "summarize"}}` body and expects
    a `{"type": "compaction", "content": ..., "signature": ...}` block back.
    RustySqueezer 1.0.6 implements that shape exactly. A ~15k-token thread
    through a test gateway came out as a 994-token prompt.

## The budget gotcha

If the summary plus the retained tail still exceed the router's budget, LiteLLM
returns a 400 `ContextWindowExceededError`. Keep `max_tokens` in the
`context_compaction` block comfortably below the target window, and use
RustySqueezer 1.0.6 or newer, which reliably accepts very large inbound
threads (up to 64 MiB).
