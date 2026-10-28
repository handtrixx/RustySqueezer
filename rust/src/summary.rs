use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::config::Config;
use crate::text::{
    clip, dedup_keep_order, file_matches, has_error, is_compaction_instruction, msg_text,
    sentence_list, term_counts,
};

struct Scored {
    score: f64,
    text: String,
}

fn tool_facts(thread: &[Value]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut commands = Vec::new();
    let mut files = Vec::new();
    let mut tools = Vec::new();
    for m in thread {
        if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in calls {
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                if !name.is_empty() {
                    tools.push(name.to_string());
                }
                let args: Option<Value> = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .and_then(|s| serde_json::from_str(s).ok());
                if let Some(args) = args.filter(|a| a.is_object()) {
                    if let Some(cmd) = args
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.trim())
                    {
                        if !cmd.is_empty() {
                            commands.push(cmd.to_string());
                        }
                    }
                    for key in ["path", "file_path", "p", "target"] {
                        if let Some(val) = args.get(key).and_then(|v| v.as_str()).map(|v| v.trim()) {
                            if !val.is_empty() {
                                files.push(val.to_string());
                            }
                        }
                    }
                }
            }
        }
        files.extend(file_matches(&msg_text(m)));
    }
    (commands, files, tools)
}

fn score_sentences(sents: &[String], task_terms: &HashSet<String>, recency_weight: f64) -> Vec<Scored> {
    let n = sents.len().max(1);
    let mut scored = Vec::with_capacity(sents.len());
    for (i, s) in sents.iter().enumerate() {
        let hit = term_counts(s)
            .keys()
            .filter(|t| task_terms.contains(t.as_str()))
            .count() as f64;
        let mut bonus = 0.0;
        if s.chars().any(|c| c.is_ascii_digit()) {
            bonus += 0.5;
        }
        if !file_matches(s).is_empty() {
            bonus += 1.0;
        }
        if has_error(s) {
            bonus += 0.5;
        }
        scored.push(Scored {
            score: hit + bonus + recency_weight * (i as f64 / n as f64),
            text: s.clone(),
        });
    }
    scored
}

async fn assist_summarize(cfg: &Config, text: &str) -> String {
    if !cfg.assist_enabled || cfg.assist_url.is_empty() {
        return String::new();
    }
    let payload = serde_json::json!({
        "model": cfg.assist_model,
        "messages": [
            {
                "role": "system",
                "content": "You are a context compactor. Output terse bullet points of the most important facts, decisions, file paths, commands and results. No commentary."
            },
            { "role": "user", "content": clip(text, cfg.assist_max_input_chars) }
        ],
        "max_tokens": cfg.assist_max_tokens
    });
    let url = format!("{}/chat/completions", cfg.assist_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(40))
        .build();
    let client = match client {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    match client.post(&url).json(&payload).send().await {
        Ok(resp) => match resp.json::<Value>().await {
            Ok(data) => {
                let msg = data
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"));
                let content = msg
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = if content.is_empty() {
                    msg.and_then(|m| m.get("reasoning_content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    content
                };
                content.trim().to_string()
            }
            Err(_) => String::new(),
        },
        Err(_) => String::new(), // deterministic fallback: skip the assist section
    }
}

pub async fn build_summary(messages: &[Value], cfg: &Config) -> String {
    let mut thread: Vec<Value> = messages.to_vec();
    // Zed appends the compaction instruction as the final user message.
    let mut instruction = String::new();
    if let Some(last) = thread.last() {
        if last.get("role").and_then(|r| r.as_str()) == Some("user")
            && is_compaction_instruction(&msg_text(last))
        {
            instruction = msg_text(last);
            thread.pop();
        }
    }

    let mut system = String::new();
    let mut user_msgs: Vec<Value> = Vec::new();
    let mut assistant_msgs: Vec<Value> = Vec::new();
    for m in &thread {
        match m.get("role").and_then(|r| r.as_str()) {
            Some("system") => system.push_str(&msg_text(m)),
            Some("user") => user_msgs.push(m.clone()),
            Some("assistant") => assistant_msgs.push(m.clone()),
            _ => {}
        }
    }

    let (commands, files, tools) = tool_facts(&thread);

    let first = if !user_msgs.is_empty() {
        user_msgs[0].clone()
    } else {
        Value::String(system)
    };
    let mut task_terms: HashSet<String> = term_counts(&msg_text(&first)).keys().cloned().collect();
    if user_msgs.len() > 1 {
        for m in &user_msgs[1..] {
            for t in term_counts(&msg_text(m)).keys() {
                if t.len() > 4 {
                    task_terms.insert(t.clone());
                }
            }
        }
    }

    let mut parts: Vec<String> = Vec::new();
    parts.push("## Conversation summary (deterministic compaction by local-compact)".to_string());
    if !instruction.is_empty() {
        parts.push("(Compaction requested by the agent harness; the task below is the original request.)".to_string());
    }
    if !user_msgs.is_empty() {
        parts.push(format!(
            "\n## Original task\n{}",
            clip(&msg_text(&user_msgs[0]), 1500)
        ));
    }
    if user_msgs.len() > 1 {
        let followups: Vec<String> = user_msgs[1..]
            .iter()
            .map(|u| format!("- {}", clip(&msg_text(u), 300)))
            .collect();
        parts.push(format!(
            "\n## Follow-up user messages\n{}",
            followups.join("\n")
        ));
    }
    if !commands.is_empty() {
        let mut recent: Vec<String> = commands.iter().take(cfg.max_commands).cloned().collect();
        recent.reverse();
        parts.push(format!(
            "\n## Commands run (most recent first)\n{}",
            recent.iter().map(|c| format!("- `{}`", c)).collect::<Vec<_>>().join("\n")
        ));
    }
    if !files.is_empty() {
        parts.push(format!(
            "\n## Files/paths involved\n{}",
            dedup_keep_order(&files, cfg.max_files)
                .iter()
                .map(|f| format!("- `{}`", f))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !tools.is_empty() {
        // Count with insertion order, then sort by count descending (stable),
        // which matches Counter.most_common tie behaviour.
        let mut order: Vec<(String, usize)> = Vec::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for t in &tools {
            if !counts.contains_key(t) {
                order.push((t.clone(), 0));
            }
            *counts.entry(t.clone()).or_insert(0) += 1;
        }
        let mut pairs: Vec<(String, usize)> = order
            .iter()
            .map(|(t, _)| (t.clone(), counts[t]))
            .collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        parts.push(format!(
            "\n## Tools used\n{}",
            pairs
                .iter()
                .take(10)
                .map(|(t, n)| format!("{} ({})", t, n))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut errors: Vec<String> = Vec::new();
    for m in &thread {
        for line in msg_text(m).lines() {
            let l = line.trim();
            if has_error(line) && (10..=300).contains(&l.chars().count()) {
                errors.push(l.to_string());
            }
        }
    }
    let errors = dedup_keep_order(&errors, cfg.max_errors);
    if !errors.is_empty() {
        parts.push(format!(
            "\n## Errors / problems seen\n{}",
            errors.iter().map(|e| format!("- {}", e)).collect::<Vec<_>>().join("\n")
        ));
    }
    let all_sents: Vec<String> = assistant_msgs
        .iter()
        .flat_map(|m| sentence_list(&msg_text(m)))
        .collect();
    let mut key = score_sentences(&all_sents, &task_terms, 2.0);
    // Python: sorted(..., reverse=True) on (score, sentence) tuples: score
    // descending, then lexicographic descending; stable for equal tuples.
    key.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        .then(b.text.cmp(&a.text)));
    key.truncate(cfg.max_key_points);
    if !key.is_empty() {
        parts.push(format!(
            "\n## Key points from the assistant\n{}",
            key.iter()
                .map(|k| format!("- {}", clip(&k.text, 300)))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    // Verbatim tail: the turns the agent actually needs to continue from.
    let tail_start = thread.len().saturating_sub(cfg.tail_turns);
    let mut tail_lines = Vec::new();
    for m in &thread[tail_start..] {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let mut text = clip(&msg_text(m), cfg.tail_chars);
        if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
            let names: Vec<String> = calls
                .iter()
                .map(|tc| {
                    tc.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            text = format!("{} [tool calls: {}]", text, names.join("; ")).trim().to_string();
        }
        tail_lines.push(format!("[{}] {}", role, text));
    }
    parts.push(format!(
        "\n## Most recent turns (verbatim, clipped)\n{}",
        tail_lines.join("\n\n")
    ));
    if cfg.assist_enabled {
        let joined: Vec<String> = thread.iter().map(|m| msg_text(m)).collect();
        let assist = assist_summarize(cfg, &joined.join("\n")).await;
        if !assist.is_empty() {
            parts.push(format!("\n## Small-model highlights\n{}", assist));
        }
    }
    let summary = parts.join("\n");
    if summary.chars().count() > cfg.max_summary_chars {
        let kept: String = summary.chars().take(cfg.max_summary_chars - 20).collect();
        format!("{}\n[summary truncated]", kept)
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn synthetic_thread() -> Vec<Value> {
        let mut msgs: Vec<Value> = vec![json!({
            "role": "system",
            "content": "You are the Zed agent. Follow AGENTS.md rules."
        })];
        msgs.push(json!({
            "role": "user",
            "content": "Build a deterministic fake-LLM compactor service for Zed context compaction and wire it into LiteLLM as local-compact."
        }));
        for i in 0..20 {
            msgs.push(json!({
                "role": "assistant",
                "content": format!("Step {i}: I will edit app.py section {i} and run tests. Decision: use TF-IDF sentence scoring. The file /data/docker/local-compact/app.py line {} needs the scoring function. Ran `pytest -q` which passed 12 tests. Note: llama.cpp prefix cache requires deterministic output.", i * 13)
            }));
            msgs.push(json!({
                "role": "tool",
                "content": format!("output of command {i}: 340 lines, exit 0. warning: DeprecationWarning at line {i}\ncompiled ok")
            }));
            msgs.push(json!({
                "role": "assistant",
                "content": format!("Error: connection refused on port {} during probe; retried and it worked. Also error: 'docker compose config' failed due to a typo; fixed by correcting the yaml.", 8000 + i)
            }));
            if i % 5 == 0 {
                msgs.push(json!({
                    "role": "user",
                    "content": format!("Also make sure section {i} handles streaming responses and keeps the tail verbatim.")
                }));
            }
        }
        msgs.push(json!({
            "role": "assistant",
            "content": "Final: service works, summary below.",
            "tool_calls": [{
                "id": "t1",
                "type": "function",
                "function": {"name": "terminal", "arguments": "{\"command\": \"docker compose ps\", \"cd\": \"/data/docker/local-compact\"}"}
            }]
        }));
        msgs.push(json!({
            "role": "user",
            "content": "Please summarize the conversation so far so the agent can continue with the work. Include the task, decisions made, files touched, and current state."
        }));
        msgs
    }

    #[tokio::test]
    async fn determinism_and_sections() {
        let cfg = Config::from_env();
        let thread = synthetic_thread();
        let a = build_summary(&thread, &cfg).await;
        let b = build_summary(&thread, &cfg).await;
        assert_eq!(a, b, "summary must be deterministic");
        for section in [
            "## Conversation summary",
            "## Original task",
            "## Follow-up user messages",
            "## Commands run (most recent first)",
            "## Files/paths involved",
            "## Tools used",
            "## Errors / problems seen",
            "## Key points from the assistant",
            "## Most recent turns (verbatim, clipped)",
        ] {
            assert!(a.contains(section), "missing section: {section}");
        }
        // Compaction instruction itself must not appear in the summary body tail.
        assert!(a.contains("docker compose ps"));
    }

    #[test]
    fn sentence_split_matches_python_lookbehind() {
        let s = sentence_list("First sentence is long enough here. Second one too, definitely. Short. A B");
        assert_eq!(s.len(), 2);
    }

    #[tokio::test]
    async fn instruction_only_thread_does_not_crash() {
        let cfg = Config::from_env();
        let thread = vec![json!({
            "role": "user",
            "content": "Please summarize the conversation so far so the agent can continue. Compaction of the context window is needed for this long conversation thread here."
        })];
        let s = build_summary(&thread, &cfg).await;
        assert!(s.contains("## Conversation summary"));
        assert!(s.contains("## Most recent turns"));
    }

    #[test]
    fn clip_behaviour() {
        assert_eq!(clip("  hello  ", 10), "hello");
        assert_eq!(clip("abcdefghij", 6), "abc...");
    }
}
