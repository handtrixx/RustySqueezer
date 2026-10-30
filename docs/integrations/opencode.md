---
title: OpenCode
description: OpenCode's native compaction model setting, pointed at RustySqueezer.
---

# OpenCode

OpenCode (anomalyco/opencode) has first-class compaction, including a model
setting that is the direct analogue of Zed's `agent.compaction_model`. Two
routes work: native client-side compaction, or gateway-side through LiteLLM.

## Route A - native compaction model (recommended)

Declare RustySqueezer through an OpenAI-compatible provider, then name it as
the compaction model:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "litellm": {
      "npm": "@ai-sdk/openai-compatible",
      "baseURL": "http://localhost:4000/v1",
      "models": {
        "rustysqueezer": {
          "name": "RustySqueezer",
          "limit": { "context": 1000000, "output": 4096 }
        }
      }
    }
  },
  "agents": {
    "compaction": { "model": "litellm/rustysqueezer" }
  },
  "compaction": {
    "auto": true,
    "keep": { "tokens": 15000 }
  }
}
```

!!! note "If the model cannot be resolved"
    OpenCode falls back to the session's own model for the summary. So a typo
    in `agents.compaction.model` degrades silently rather than failing - check
    the RustySqueezer log to confirm requests are actually arriving.

## What the knobs do

| Key | Default | Behaviour |
| --- | --- | --- |
| `compaction.auto` | `true` | Compact automatically near the context limit, and recover once when the provider rejects a request as too long |
| `compaction.keep.tokens` | `15000` | Roughly how much recent conversation is kept verbatim next to the summary |
| `compaction.buffer` | 15% of the limit, minimum 16k | Tokens kept free below the model's limit; larger means compaction starts earlier |
| `agents.compaction.model` | session model | Which model writes the summary |

## Route B - gateway side

Point OpenCode at LiteLLM and let the auto-router's `context_compaction` do the
work, as described in the [LiteLLM guide](litellm.md). This is the option when
several clients share one gateway.

!!! warning "Provider-native compaction does not apply"
    OpenCode also supports per-provider `settings.compaction: { "type": "native" }`,
    where the provider compacts the conversation on its own side. That is
    OpenAI Responses-API specific and has no effect on RustySqueezer.

!!! tip "Improve OpenCode summaries"
    OpenCode sends its own summarisation instructions with the thread.
    RustySqueezer treats recognised instruction text as the request, not as
    thread content - add OpenCode's wording to `instruction_patterns` via
    `CONFIG_PATH` (see [Configuration](../configuration.md)) so it is not
    counted as user input.
