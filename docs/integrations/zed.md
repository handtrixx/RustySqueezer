---
title: Zed
description: Point Zed's agent compaction and thread-summary models at RustySqueezer.
---

# Zed

Zed is the reference integration: the agent has an explicit compaction model
setting, so RustySqueezer slots in directly, client-side, with no gateway.

## Declare the endpoint

In `settings.json`, declare RustySqueezer as an OpenAI-compatible provider with
a deliberately large context window:

```jsonc
{
  "language_models": {
    "openai_compatible": {
      "RustySqueezer": {
        "api_url": "http://localhost:8000/v1",
        "available_models": [
          {
            "name": "rustysqueezer",
            "display_name": "RustySqueezer (1M ctx)",
            "max_tokens": 1000000,
            "capabilities": {
              "tools": false,
              "images": false,
              "chat_completions": true
            }
          }
        ]
      }
    }
  }
}
```

## Point the compaction settings at it

```jsonc
{
  "agent": {
    "compaction_model":     { "provider": "RustySqueezer", "model": "rustysqueezer" },
    "thread_summary_model": { "provider": "RustySqueezer", "model": "rustysqueezer" },
    "auto_compact": {
      "enabled": true,
      "threshold": -30000
    }
  }
}
```

!!! note "Why the window must be big"
    Compaction re-sends the *entire* thread as one prompt to the compaction
    model. If the compaction model's declared window is smaller than the
    thread, compaction is mathematically impossible and Zed just appears to
    hang. Declaring `max_tokens: 1000000` lets whole long sessions reach
    RustySqueezer in a single request - it accepts bodies up to 64 MiB and
    compacts a ~1M-token thread in about 0.1 s at ~4 MB RSS.

## Threshold: use an absolute value

`"threshold": -30000` means "compact when fewer than 30k tokens remain",
anchored to the live ceiling of the primary model. A percentage (`"80%"`) is
evaluated only on message boundaries, so one giant tool result or `@`-mention
can jump straight over the trigger in a single turn. The absolute form is
robust to that, and to the gap between Zed's token estimate and the real
tokenizer.

## Recommended fallback

RustySqueezer is extractive: it keeps commands, files, errors, key sentences
and the verbatim tail, but it does not abstract. For long, meandering threads
where nuance matters, keep a real small model with a large window as the
compaction model and use RustySqueezer for the bulk of sessions.

!!! tip "Verify it is live"
    Trigger `/compact` and watch the container log - every request emits
    `[usage] <surface> in= out= ratio= latency_ms=`:

    ```bash
    docker logs -f rustysqueezer
    ```
