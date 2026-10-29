use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::config::Config;
use crate::text::{
    clip, dedup_keep_order, file_matches, has_error, is_compaction_instruction, msg_text,
    dedup_normalized, sentence_list, term_counts,
};

struct Scored {
    score: f64,
    text: String,
}

fn tool_facts(thread: &[Value], cfg: &Config) -> (Vec<String>, Vec<String>, Vec<String>) {
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
        files.extend(file_matches(&msg_text(m), cfg));
    }
    (commands, files, tools)
}

fn score_sentences(
    sents: &[String],
    task_terms: &HashSet<String>,
    recency_weight: f64,
    cfg: &Config,
) -> Vec<Scored> {
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
        if !file_matches(s, cfg).is_empty() {
            bonus += 1.0;
        }
        if has_error(s, cfg) {
            bonus += 0.5;
        }
        scored.push(Scored {
            score: hit + bonus + recency_weight * (i as f64 / n as f64),
            text: s.clone(),
        });
    }
    scored
}


/// Keep lines in order while they fit `cap` chars. Returns (kept, dropped).
/// Deterministic greedy fill: instead of clipping the assembled summary at the
/// end, every section is budgeted on its own.
fn fit_lines(lines: &[String], cap: usize) -> (Vec<String>, usize) {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let cost = line.chars().count() + if kept.is_empty() { 0 } else { 1 };
        if used + cost > cap {
            break;
        }
        used += cost;
        kept.push(line.clone());
    }
    let dropped = lines.len() - kept.len();
    (kept, dropped)
}

/// Section body capped to its budget slice, with an explicit note when items
/// had to be dropped (so the summary never silently loses content).
fn section_body(lines: &[String], cap: usize, label: &str) -> String {
    let (kept, dropped) = fit_lines(lines, cap.saturating_sub(40));
    let mut body = kept.join("\n");
    if dropped > 0 {
        body.push_str(&format!("\n(+{} more {} omitted by the budget)", dropped, label));
    }
    body
}

pub async fn build_summary(messages: &[Value], cfg: &Config) -> String {
    let mut thread: Vec<Value> = messages.to_vec();
    // Zed appends the compaction instruction as the final user message.
    let mut instruction = String::new();
    if let Some(last) = thread.last() {
        if last.get("role").and_then(|r| r.as_str()) == Some("user")
            && is_compaction_instruction(&msg_text(last), cfg)
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

    let (commands, files, tools) = tool_facts(&thread, cfg);

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
    parts.push(format!("## {}", cfg.title("header")));

    // Fixed cost of the header and every section title; the rest of the budget
    // is split per section (Config::budgets). Budgeting sections up front is
    // what keeps the verbatim tail from being clipped away on long threads.
    let enabled: Vec<String> = cfg.sections.clone();
    let mut overhead = cfg.title("header").chars().count() + 4;
    if !instruction.is_empty() {
        overhead += cfg.title("note").chars().count() + 4;
    }
    for key in &enabled {
        overhead += cfg.title(key).chars().count() + 6; // "\n## <title>\n"
    }
    let content_budget = cfg.max_summary_chars.saturating_sub(overhead);
    let total_share: usize = enabled
        .iter()
        .map(|k| cfg.budgets.get(k).copied().unwrap_or(0))
        .sum();

    if !instruction.is_empty() {
        parts.push(cfg.title("note"));
    }
    if cfg.section_enabled("original_task") && !user_msgs.is_empty() {
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("original_task"),
            clip(
                &msg_text(&user_msgs[0]),
                cfg.section_cap("original_task", content_budget, total_share).min(1500),
            )
        ));
    }
    if cfg.section_enabled("followups") && user_msgs.len() > 1 {
        let followups: Vec<String> = user_msgs[1..]
            .iter()
            .map(|u| format!("- {}", clip(&msg_text(u), 300)))
            .collect();
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("followups"),
            section_body(
                &followups,
                cfg.section_cap("followups", content_budget, total_share),
                "follow-ups",
            )
        ));
    }
    if cfg.section_enabled("commands") && !commands.is_empty() {
        let start = commands.len().saturating_sub(cfg.max_commands);
        let mut recent: Vec<String> = commands[start..].iter().cloned().collect();
        recent.reverse();
        let lines: Vec<String> = recent
            .iter()
            .map(|c| format!("- `{}`", clip(c, 200)))
            .collect();
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("commands"),
            section_body(&lines, cfg.section_cap("commands", content_budget, total_share), "commands")
        ));
    }
    if cfg.section_enabled("files") && !files.is_empty() {
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("files"),
            section_body(
                &dedup_keep_order(&files, cfg.max_files)
                    .iter()
                    .map(|f| format!("- `{}`", clip(f, 120)))
                    .collect::<Vec<_>>(),
                cfg.section_cap("files", content_budget, total_share),
                "paths",
            )
        ));
    }
    if cfg.section_enabled("tools") && !tools.is_empty() {
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
            "\n## {}\n{}",
            cfg.title("tools"),
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
            if has_error(line, cfg) && (10..=300).contains(&l.chars().count()) {
                errors.push(l.to_string());
            }
        }
    }
    let errors = dedup_keep_order(&errors, cfg.max_errors);
    if cfg.section_enabled("errors") && !errors.is_empty() {
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("errors"),
            section_body(
                &errors.iter().map(|e| format!("- {}", e)).collect::<Vec<_>>(),
                cfg.section_cap("errors", content_budget, total_share),
                "error lines",
            )
        ));
    }
    let all_sents: Vec<String> = assistant_msgs
        .iter()
        .flat_map(|m| sentence_list(&msg_text(m)))
        .collect();
    // Repeated tool chatter would otherwise fill the key-points budget with
    // the same sentence ten times.
    let all_sents = dedup_normalized(&all_sents);
    let mut key = score_sentences(&all_sents, &task_terms, 2.0, cfg);
    // Order by score descending, then text descending, so ties are stable
    // and the output is reproducible.
    key.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        .then(b.text.cmp(&a.text)));
    key.truncate(cfg.max_key_points);
    if cfg.section_enabled("key_points") && !key.is_empty() {
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("key_points"),
            section_body(
                &key.iter()
                    .map(|k| format!("- {}", clip(&k.text, 300)))
                    .collect::<Vec<_>>(),
                cfg.section_cap("key_points", content_budget, total_share),
                "key points",
            )
        ));
    }
    // Verbatim tail: the turns the agent actually needs to continue from. It is
    // budgeted last but kept first: if the tail does not fit, the oldest tail
    // turns are dropped (and reported), never the tail as a whole.
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
    if cfg.section_enabled("tail") {
        let cap = cfg.section_cap("tail", content_budget, total_share).saturating_sub(40);
        // Fill from the most recent turn backwards, then restore order.
        let mut newest_first = tail_lines.clone();
        newest_first.reverse();
        let (kept, dropped) = fit_lines(&newest_first, cap);
        let mut body = kept;
        body.reverse();
        if dropped > 0 {
            body.insert(0, format!("(+{} older tail turns dropped by the budget)", dropped));
        }
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("tail"),
            body.join("\n\n")
        ));
    }
    // Sections are budgeted individually, so this is only a safety net for a
    // misconfigured section_budgets map (shares summing to more than 100).
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
            "content": "Build a deterministic fake-LLM compactor service for Zed context compaction and wire it into LiteLLM as rustysqueezer."
        }));
        for i in 0..20 {
            msgs.push(json!({
                "role": "assistant",
                "content": format!("Step {i}: I will edit summary.rs section {i} and run tests. Decision: use TF-IDF sentence scoring. The file rust/src/summary.rs line {} needs the scoring function. Ran `cargo test` which passed 10 tests. Note: llama.cpp prefix cache requires deterministic output.", i * 13)
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
                "function": {"name": "terminal", "arguments": "{\"command\": \"docker compose ps\", \"cd\": \"/data/docker/rustysqueezer\"}"}
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

    #[tokio::test]
    async fn tail_survives_a_tight_budget() {
        let mut cfg = Config::from_env();
        cfg.max_summary_chars = 1200;
        let thread = synthetic_thread();
        let s = build_summary(&thread, &cfg).await;
        let len = s.chars().count();
        assert!(len <= cfg.max_summary_chars, "summary must fit the budget ({len})");
        assert!(
            !s.contains("[summary truncated]"),
            "per-section budgeting should make the hard clip unnecessary"
        );
        // The newest turns (including the docker compose ps tool call) are what
        // the agent needs to continue; they must not be the first thing cut.
        assert!(s.contains("Most recent turns"), "tail section must be present");
        assert!(s.contains("docker compose ps"), "verbatim tail must survive");
    }

    #[test]
    fn section_shares_normalise_over_enabled_sections() {
        let mut cfg = Config::from_env();
        cfg.sections = vec!["commands".into(), "tail".into()];
        // 12 + 50 shares: the tail still gets the bigger slice, and the two
        // together use the whole content budget.
        let tail = cfg.section_cap("tail", 1000, 62);
        let commands = cfg.section_cap("commands", 1000, 62);
        assert!(tail > commands);
        // Integer division may leave at most one char of slack.
        assert!(tail + commands >= 999, "shares should use the whole budget");
    }

    #[test]
    fn fit_lines_reports_drops() {
        let lines: Vec<String> = (0..5).map(|_| "a".repeat(30)).collect();
        let (kept, dropped) = fit_lines(&lines, 100);
        assert_eq!(kept.len(), 3); // 30 + 31 + 31 = 92, the fourth would be 123
        assert_eq!(dropped, 2);
    }

    #[test]
    fn sentence_splitting_keeps_only_full_sentences() {
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
