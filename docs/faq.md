---
title: FAQ
description: Determinism, limits, token counting and when not to use it.
---

# FAQ

??? Is the output really deterministic?
    Yes. Same thread plus same config gives byte-identical output: no sampling,
    no weights, no network calls. That is the point - deterministic summaries
    keep provider prefix caches warm instead of thrashing them.

??? How large a thread can I send?
    Request bodies up to 64 MiB. A roughly 1M-token Zed session compacts in
    about 0.1 s while the process stays near 4 MB RSS. Bodies above 64 MiB are
    rejected with 413.

??? Are the token counts exact?
    No. Counting is word-boundary based (runs of ASCII alphanumerics and
    apostrophes count as one token, other non-whitespace characters count
    individually) and is identical in the Python and Rust implementations. It
    is good enough for trigger thresholds, but do not treat it as cl100k BPE
    ground truth.

??? Why does my gateway return `ContextWindowExceededError`?
    The compacted summary plus the retained tail still exceeded the router's
    budget. Lower `max_tokens` in the gateway's compaction block, raise the
    target model's declared window, or tune `max_summary_chars`.

??? Does it call a model at all?
    Never. There is no outbound network in the summariser. It is a single
    static Rust binary.

??? Can I use it for summarising documents or RAG?
    It is designed for agent threads: task, tool calls, files, errors, key
    sentences, verbatim tail. It is not a general summariser and has no
    embeddings or retrieval.

??? Should I keep a real model as fallback?
    Recommended. RustySqueezer is extractive, so nuance in long, meandering
    threads is lost. Many setups use it for the bulk of sessions and a small
    real model for the threads that matter.

??? Is it safe to expose?
    There is no authentication and no rate limiting. Run it on a private
    Docker network and let your gateway front it, as in the
    [LiteLLM guide](integrations/litellm.md).
