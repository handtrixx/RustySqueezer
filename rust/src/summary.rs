use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::config::Config;
use crate::text::{
    clip, dedup_keep_order, file_matches, has_error, has_fact_signal, is_compaction_instruction,
    msg_text, normalize, salient_lines, sentence_list, term_counts,
};

struct Scored {
    score: f64,
    text: String,
}

const CP_OPEN: &str = "<conversation-checkpoint>";
const CP_CLOSE: &str = "</conversation-checkpoint>";

/// Remove every `<conversation-checkpoint>…</conversation-checkpoint>` span.
/// An unterminated span runs to the end of the text (a clipped embed is not
/// closed). Used so an embedded checkpoint cannot re-contaminate the errors,
/// files and key-point pools with its own boilerplate.
fn strip_checkpoint_spans(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find(CP_OPEN) {
        out.push_str(&rest[..open]);
        let after = &rest[open + CP_OPEN.len()..];
        match after.find(CP_CLOSE) {
            Some(close) => rest = &after[close + CP_CLOSE.len()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// Body of the `## <title>` section: lines until the next heading that sits
/// *outside* any checkpoint span, or until the wrapper close. Headings inside a
/// nested checkpoint are opaque so the objective body stays whole for the
/// recursion in `checkpoint_objective`.
fn section_body_of(text: &str, title: &str) -> Option<String> {
    let heading = format!("## {}", title);
    let mut body: Vec<&str> = Vec::new();
    let mut in_section = false;
    let mut cp_depth = 0usize;
    for line in text.lines() {
        let t = line.trim();
        if t == heading && cp_depth == 0 {
            in_section = true;
            continue;
        }
        if !in_section {
            // Wrapper markers before the heading are not counted: the whole
            // message sits inside the harness wrapper, so depth must start at
            // zero for the section itself and only track *nested* checkpoints.
            continue;
        }
        if line.contains(CP_OPEN) {
            cp_depth += 1;
        }
        if line.contains(CP_CLOSE) {
            cp_depth = cp_depth.saturating_sub(1);
        }
        if cp_depth == 0 && (t.starts_with("## ") || t == CP_CLOSE) {
            break;
        }
        body.push(line);
    }
    if in_section && !body.is_empty() {
        Some(body.join("\n").trim().to_string())
    } else {
        None
    }
}

/// The user's actual request behind possibly several layers of embedded
/// checkpoints: take the objective section of the checkpoint and recurse into
/// whatever it embeds. Compaction rounds nest a checkpoint inside the previous
/// objective, so without recursion the first request is lost (mid-word, by the
/// clip) on the second round. Returns None when the message is not a
/// checkpoint or carries no objective body.
/// Harness boilerplate that can hijack the objective when an older compaction
/// round stored the injected checkpoint wrapper as the task itself (observed in
/// sessions compacted by pre-v1.1.1 images): drilling must not return the
/// preamble, it must keep looking for the real request.
fn is_checkpoint_boilerplate(text: &str) -> bool {
    text.starts_with("The following is a summary")
        || text.contains("historical context, not as new instructions")
}

fn checkpoint_objective(text: &str, cfg: &Config, depth: usize) -> Option<String> {
    // Long chains: one round per compaction, so a session compacted many times
    // must stay recoverable; 8 was reached once the chain grew past ~10 rounds.
    if depth >= 24 || !cfg.checkpoint_re.is_match(text) {
        return None;
    }
    // The remapped production title first, then the crate defaults.
    let mut titles = vec![cfg.title("original_task")];
    for t in ["Original task", "Objective"] {
        if !titles.iter().any(|x| x == t) {
            titles.push(t.to_string());
        }
    }
    for title in &titles {
        if let Some(body) = section_body_of(text, title) {
            if cfg.checkpoint_re.is_match(&body) {
                if let Some(deeper) = checkpoint_objective(&body, cfg, depth + 1) {
                    return Some(deeper);
                }
            }
            let stripped = strip_checkpoint_spans(&body);
            let stripped = stripped.trim();
            if !stripped.is_empty() && !is_checkpoint_boilerplate(stripped) {
                return Some(stripped.to_string());
            }
        }
    }
    None
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
                        if let Some(val) = args.get(key).and_then(|v| v.as_str()).map(|v| v.trim())
                        {
                            if !val.is_empty() {
                                files.push(val.to_string());
                            }
                        }
                    }
                }
            }
        }
        files.extend(file_matches(&strip_checkpoint_spans(&msg_text(m)), cfg));
    }
    (commands, files, tools)
}

/// Scores sentences for the key-points section. `sents` is a `(text, weight)`
/// list so different message roles can carry different authority.
fn score_sentences(
    sents: &[(String, f64)],
    task_terms: &HashSet<String>,
    recency_weight: f64,
    cfg: &Config,
    occurrences: &HashMap<String, usize>,
) -> Vec<Scored> {
    let n = sents.len().max(1);
    let mut scored = Vec::with_capacity(sents.len());
    for (i, (s, weight)) in sents.iter().enumerate() {
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
        // Concrete facts (versions, IPs, counts, status words) are exactly what a
        // compaction must not lose, so they outrank generic prose.
        if has_fact_signal(s) {
            bonus += 1.5;
        }
        // A sentence repeated across the thread (tool retries, follow-up quotes)
        // is more central than a one-off mention.
        let reps = occurrences.get(&normalize(s)).copied().unwrap_or(1);
        if reps > 1 {
            bonus += 0.4 * (reps - 1).min(6) as f64;
        }
        scored.push(Scored {
            score: (hit + bonus + recency_weight * (i as f64 / n as f64)) * weight,
            text: s.clone(),
        });
    }
    scored
}

/// Keep lines in order while they fit `cap` chars. Returns (kept, dropped).
/// Deterministic greedy fill: instead of clipping the assembled summary at the
/// end, every section is budgeted on its own. A line that is too long for the
/// remaining budget is skipped rather than ending the section, so a single
/// verbose line cannot block several shorter, still-relevant ones.
fn fit_lines(lines: &[String], cap: usize) -> (Vec<String>, usize) {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let cost = line.chars().count() + if kept.is_empty() { 0 } else { 1 };
        if used + cost > cap {
            continue;
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
        body.push_str(&format!(
            "\n(+{} more {} omitted by the budget)",
            dropped, label
        ));
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
    let mut checkpoint_carried: Vec<String> = Vec::new();
    let mut assistant_msgs: Vec<Value> = Vec::new();
    for m in &thread {
        match m.get("role").and_then(|r| r.as_str()) {
            Some("system") => system.push_str(&msg_text(m)),
            Some("user") => {
                let text = msg_text(m);
                // A checkpoint is harness-injected state, not user intent.
                if cfg.checkpoint_re.is_match(&text) {
                    checkpoint_carried.push(text);
                } else {
                    user_msgs.push(m.clone());
                }
            }
            Some("assistant") => assistant_msgs.push(m.clone()),
            _ => {}
        }
    }

    let (commands, files, tools) = tool_facts(&thread, cfg);

    // Objective: the deepest original request behind any embedded checkpoints,
    // else the first real user message, else the system prompt. Flattening
    // here is what keeps the user's first request through repeated rounds -
    // embedding the previous checkpoint verbatim clipped it mid-word instead.
    let task_text = checkpoint_carried
        .iter()
        .find_map(|t| checkpoint_objective(t, cfg, 0))
        .or_else(|| {
            // Pre-v1.1.1 contamination put the checkpoint wrapper itself into
            // the user list; never let harness boilerplate become the task.
            user_msgs
                .iter()
                .find(|m| !is_checkpoint_boilerplate(msg_text(m).trim()))
                .map(|m| msg_text(m))
                .or_else(|| if system.is_empty() { None } else { Some(system.clone()) })
        })
        .unwrap_or_default();
    let mut task_terms: HashSet<String> = term_counts(&task_text).keys().cloned().collect();
    // Later user messages widen the task vocabulary; when a checkpoint supplied
    // the objective every real message counts (the first real message is not
    // the task then).
    for m in &user_msgs {
        for t in term_counts(&msg_text(m)).keys() {
            if t.len() > 4 {
                task_terms.insert(t.clone());
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
    if cfg.section_enabled("original_task") && !task_text.is_empty() {
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("original_task"),
            clip(
                &task_text,
                cfg.section_cap("original_task", content_budget, total_share)
                    .min(1500),
            )
        ));
    }
    // Follow-ups: real user messages only - checkpoints are injected state, and
    // when a checkpoint supplied the objective every real message is a
    // follow-up. Short acknowledgements ("it works", "thanks") are not new
    // requirements and are filtered out.
    let followup_src: &[Value] = if checkpoint_carried.is_empty() && !user_msgs.is_empty() {
        &user_msgs[1..]
    } else {
        &user_msgs[..]
    };
    let followups: Vec<String> = followup_src
        .iter()
        .filter(|u| !cfg.followup_skip_re.is_match(msg_text(u).trim()))
        .map(|u| format!("- {}", clip(&msg_text(u), 300)))
        .collect();
    if cfg.section_enabled("followups") && !followups.is_empty() {
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
        // Most recent first, but collapse repeats: a retried ssh/git one-liner
        // listed 5 times crowds out distinct commands within the budget.
        let mut recent: Vec<String> = Vec::new();
        for c in commands.iter().rev() {
            if !recent.contains(c) {
                recent.push(c.clone());
                if recent.len() >= cfg.max_commands {
                    break;
                }
            }
        }
        let lines: Vec<String> = recent
            .iter()
            .map(|c| format!("- `{}`", clip(c, 200)))
            .collect();
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("commands"),
            section_body(
                &lines,
                cfg.section_cap("commands", content_budget, total_share),
                "commands"
            )
        ));
    }
    if cfg.section_enabled("files") && !files.is_empty() {
        // Rank by how often a path actually appears (first-seen order breaks
        // ties), so heavily used files survive the budget while incidental
        // mentions get dropped.
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for f in &files {
            if !counts.contains_key(f) {
                order.push(f.clone());
            }
            *counts.entry(f.clone()).or_insert(0) += 1;
        }
        let mut ranked: Vec<(String, usize)> =
            order.iter().map(|f| (f.clone(), counts[f])).collect();
        // sort_by is stable: equal counts keep first-seen order.
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let lines: Vec<String> = ranked
            .into_iter()
            .take(cfg.max_files)
            .map(|(f, _)| format!("- `{}`", clip(&f, 120)))
            .collect();
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("files"),
            section_body(
                &lines,
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
        let mut pairs: Vec<(String, usize)> =
            order.iter().map(|(t, _)| (t.clone(), counts[t])).collect();
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
        // The embedded checkpoint carries its own error lines; the fresh ones
        // in this thread are what the agent still has to deal with.
        for line in strip_checkpoint_spans(&msg_text(m)).lines() {
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
                &errors
                    .iter()
                    .map(|e| format!("- {}", e))
                    .collect::<Vec<_>>(),
                cfg.section_cap("errors", content_budget, total_share),
                "error lines",
            )
        ));
    }
    // Tool results are where the ground truth of a real thread lives (build
    // logs, health output, eval results), so they feed the key-point pool too.
    // They are weighted below the assistant's own prose so raw log lines cannot
    // crowd out the assistant's conclusions.
    const TOOL_WEIGHT: f64 = 0.6;
    let mut seen: HashSet<String> = HashSet::new();
    let mut pool: Vec<(String, f64)> = assistant_msgs
        .iter()
        .flat_map(|m| sentence_list(&msg_text(m)).into_iter().map(|s| (s, 1.0)))
        .collect();
    for m in &thread {
        if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
            // Raw tool logs are mostly non-sentential (docker ps rows, CI
            // output); sentence_list would filter out the very lines that
            // contain the facts, so take fact-signal lines instead.
            for s in salient_lines(&msg_text(m)) {
                pool.push((s, TOOL_WEIGHT));
            }
        }
    }
    // Facts from the previous compaction must survive the next one: the old
    // checkpoint's key-point bullets are the recall surface of everything the
    // harness already dropped from the context. Weighted below assistant prose;
    // the near-duplicate filter removes any restated later in this thread.
    const CHECKPOINT_WEIGHT: f64 = 0.8;
    for cp in &checkpoint_carried {
        let mut titles = vec![cfg.title("key_points")];
        for t in ["Key points from the assistant", "Important Context"] {
            if !titles.iter().any(|x| x == t) {
                titles.push(t.to_string());
            }
        }
        for title in &titles {
            if let Some(body) = section_body_of(cp, title) {
                for line in body.lines() {
                    let l = line.trim().trim_start_matches('-').trim();
                    // Skip markup (headings, wrapper tags, budget notes).
                    if l.starts_with('#') || l.starts_with('<') || l.starts_with('(') {
                        continue;
                    }
                    if (8..=320).contains(&l.chars().count()) {
                        pool.push((l.to_string(), CHECKPOINT_WEIGHT));
                    }
                }
            }
        }
    }
    // Repeated tool chatter would otherwise fill the key-points budget with
    // the same sentence ten times; first occurrence wins.
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    for (s, _) in &pool {
        *occurrences.entry(normalize(s)).or_insert(0) += 1;
    }
    pool.retain(|(s, _)| seen.insert(normalize(s)));
    let mut key = score_sentences(&pool, &task_terms, 2.0, cfg, &occurrences);
    // Order by score descending, then text descending, so ties are stable
    // and the output is reproducible.
    key.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.text.cmp(&a.text))
    });
    // Near-duplicate filter: paraphrases of the same fact (three commit-log
    // lines, repeated "surfaces PASS") each consume a line of the budget and
    // squeeze out distinct facts. Skip a candidate when >=75 % of its content
    // tokens are already covered by an accepted point. Deterministic.
    let mut selected: Vec<Scored> = Vec::new();
    let mut used_tokens: Vec<HashSet<String>> = Vec::new();
    for k in key {
        let toks: HashSet<String> = term_counts(&k.text).keys().cloned().collect();
        let dup = used_tokens.iter().any(|u| {
            let inter = toks.iter().filter(|t| u.contains(t.as_str())).count();
            !toks.is_empty() && inter * 4 >= 3 * toks.len().min(u.len())
        });
        if dup {
            continue;
        }
        used_tokens.push(toks);
        selected.push(k);
    }
    selected.truncate(cfg.max_key_points);
    let key = selected;
    if cfg.section_enabled("key_points") && !key.is_empty() {
        let bullets: Vec<String> = key
            .iter()
            .map(|k| format!("- {}", clip(&k.text, 240)))
            .collect();
        // Small-model assist: condense the bullets with the local aux model.
        // On any failure the original extractive bullets are kept, so the
        // summary never degrades below the v1.0.6 behaviour.
        let body_lines = if cfg.assist_enabled() {
            let (condensed, outcome) = crate::assist::condense_points(&bullets, cfg).await;
            println!(
                "[assist] key_points chunks={} ok={} ms={} (fallback keeps extractive bullets)",
                outcome.chunks, outcome.ok, outcome.ms
            );
            condensed
        } else {
            bullets
        };
        parts.push(format!(
            "\n## {}\n{}",
            cfg.title("key_points"),
            section_body(
                &body_lines,
                cfg.section_cap("key_points", content_budget, total_share),
                "key points",
            )
        ));
    }
    // Verbatim tail: the turns the agent actually needs to continue from. It is
    // budgeted last but kept first: if the tail does not fit, the oldest tail
    // turns are dropped (and reported), never the tail as a whole.
    let tail_start = thread.len().saturating_sub(cfg.tail_turns);
    let tail_cap = cfg
        .section_cap("tail", content_budget, total_share)
        .saturating_sub(40);
    // TAIL_CHARS is a floor, not a ceiling: if the tail's budget allows more per
    // turn (e.g. 50 % of 12000 over 6 turns = ~950), use it. Otherwise the final
    // assistant summary gets clipped while the section still has headroom.
    let per_turn = cfg.tail_chars.max(tail_cap / cfg.tail_turns.max(1));
    let mut tail_lines = Vec::new();
    for m in &thread[tail_start..] {
        // An injected checkpoint is not a turn the agent continues from; the
        // flattening above already carries its content forward.
        if cfg.checkpoint_re.is_match(&msg_text(m)) {
            continue;
        }
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let mut text = clip(&msg_text(m), per_turn);
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
            text = format!("{} [tool calls: {}]", text, names.join("; "))
                .trim()
                .to_string();
        }
        tail_lines.push(format!("[{}] {}", role, text));
    }
    if cfg.section_enabled("tail") {
        let cap = tail_cap;
        // Fill from the most recent turn backwards, then restore order.
        let mut newest_first = tail_lines.clone();
        newest_first.reverse();
        let (kept, dropped) = fit_lines(&newest_first, cap);
        let mut body = kept;
        body.reverse();
        if dropped > 0 {
            body.insert(
                0,
                format!("(+{} older tail turns dropped by the budget)", dropped),
            );
        }
        parts.push(format!("\n## {}\n{}", cfg.title("tail"), body.join("\n\n")));
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
    async fn mid_thread_tool_output_reaches_key_points() {
        let cfg = Config::from_env();
        let mut thread: Vec<Value> = vec![json!({
            "role": "user",
            "content": "Deploy the compactor and report the measured numbers."
        })];
        // A distinctive fact that exists only in a tool result, well outside
        // the verbatim tail (which only keeps the last few turns).
        thread.push(json!({
            "role": "assistant",
            "content": "I will inspect the container and run the evaluation harness."
        }));
        thread.push(json!({
            "role": "tool",
            "content": "container 172.19.0.20:8000 is healthy; eval read 189312 prompt tokens and wrote 1284 summary tokens"
        }));
        for i in 0..12 {
            thread.push(json!({
                "role": "assistant",
                "content": format!("Step {i}: routine follow-up work on the deployment continues without new findings here."),
            }));
        }
        thread.push(json!({
            "role": "user",
            "content": "Please summarize the conversation so far so the agent can continue."
        }));
        let summary = build_summary(&thread, &cfg).await;
        assert!(
            summary.contains("172.19.0.20") && summary.contains("189312"),
            "tool-output facts lost from key points:\n{summary}"
        );
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
        assert!(
            len <= cfg.max_summary_chars,
            "summary must fit the budget ({len})"
        );
        assert!(
            !s.contains("[summary truncated]"),
            "per-section budgeting should make the hard clip unnecessary"
        );
        // The newest turns (including the docker compose ps tool call) are what
        // the agent needs to continue; they must not be the first thing cut.
        assert!(
            s.contains("Most recent turns"),
            "tail section must be present"
        );
        assert!(
            s.contains("docker compose ps"),
            "verbatim tail must survive"
        );
    }

    #[test]
    fn section_shares_normalise_over_enabled_sections() {
        let mut cfg = Config::from_env();
        cfg.sections = vec!["commands".into(), "tail".into()];
        // 12 + 30 shares: the tail still gets the bigger slice, and the two
        // together use the whole content budget.
        let tail = cfg.section_cap("tail", 1000, 42);
        let commands = cfg.section_cap("commands", 1000, 42);
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
        let s = sentence_list(
            "First sentence is long enough here. Second one too, definitely. Short. A B",
        );
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

    #[tokio::test]
    async fn million_token_thread_is_compacted() {
        use crate::text::count_tokens;
        let cfg = Config::from_env();
        // ~1M tokens, the largest input LiteLLM declares for this model. The
        // compactor is linear over the thread, so a 1M-token session must be
        // accepted (no 413), stay within the output budget and remain
        // deterministic.
        let filler = "probe heartbeat log line alpha beta gamma delta ".repeat(125);
        let mut thread: Vec<Value> = vec![json!({
            "role": "user",
            "content": "needle-million-task: verify compaction handles a one-million-token session",
        })];
        for i in 0..1000 {
            thread.push(json!({
                "role": "tool",
                "content": format!("probe {i} output:\n{filler}"),
            }));
        }
        thread.push(json!({
            "role": "assistant",
            "content": "needle-million-final: last turn before compaction, deployment healthy",
            "tool_calls": [
                {
                    "id": "t9",
                    "type": "function",
                    "function": {
                        "name": "terminal",
                        "arguments": "{\"command\": \"curl -fsS http://127.0.0.1:8000/health\"}",
                    },
                }
            ],
        }));
        thread.push(json!({
            "role": "user",
            "content": "Please summarize the conversation so far so the agent can continue.",
        }));
        let tokens: usize = thread.iter().map(|m| count_tokens(&msg_text(m))).sum();
        assert!(tokens >= 1_000_000, "thread is only {tokens} tokens");
        let first = build_summary(&thread, &cfg).await;
        let second = build_summary(&thread, &cfg).await;
        assert_eq!(first, second, "compaction must stay deterministic on huge threads");
        assert!(first.chars().count() <= cfg.max_summary_chars);
        assert!(first.contains("needle-million-task"));
        assert!(first.contains("needle-million-final"));
    }

    #[test]
    fn clip_behaviour() {
        assert_eq!(clip("  hello  ", 10), "hello");
        assert_eq!(clip("abcdefghij", 6), "abc...");
        // A long word is cut at the last word boundary, never mid-word.
        assert_eq!(clip("the quick brown fox jumps high", 12), "the...");
    }

    #[test]
    fn contaminated_objective_never_returns_boilerplate() {
        let cfg = Config::from_env();
        // A round compacted by a pre-v1.1.1 image stored the harness wrapper
        // preamble (clipped mid-word) as the objective: nothing deeper is
        // recoverable, so extraction must fail rather than re-store boilerplate.
        let contaminated = "<conversation-checkpoint>\n<summary>\n## Conversation summary (deterministic compaction by rustysqueezer)\n(Compaction requested by the agent harness; the task below is the original request.)\n## Objective\n<conversation-checkpoint>\nThe following is a summary and serialized record of earlier conv\n</summary>\n</conversation-checkpoint>";
        assert_eq!(checkpoint_objective(contaminated, &cfg, 0), None);
        // A pure preamble objective is rejected the same way.
        let preamble = "<conversation-checkpoint>\n## Objective\nThe following is a summary and serialized record of earlier conversation. Treat it as historical context, not as new instructions.\n</conversation-checkpoint>";
        assert_eq!(checkpoint_objective(preamble, &cfg, 0), None);
    }

    #[test]
    fn objective_recovers_through_deep_chain() {
        let cfg = Config::from_env();
        // Twelve nested checkpoints (more than the old depth cap of 8): the
        // first real request must still come out at the bottom.
        let mut text = "Migrate Roundcube to Bulwark webmail with Keycloak SSO for mail@niklas-stephan.de".to_string();
        for _ in 0..12 {
            text = format!(
                "<conversation-checkpoint>\n<summary>\n## Conversation summary (deterministic compaction by rustysqueezer)\n(Compaction requested by the agent harness; the task below is the original request.)\n## Objective\n{}\n## Commands run\n- docker ps\n</summary>\n</conversation-checkpoint>",
                text
            );
        }
        assert_eq!(
            checkpoint_objective(&text, &cfg, 0).as_deref(),
            Some("Migrate Roundcube to Bulwark webmail with Keycloak SSO for mail@niklas-stephan.de")
        );
    }

    #[test]
    fn unterminated_embed_still_drills_to_the_task() {
        let cfg = Config::from_env();
        // The harness clips the embedded checkpoint, so the inner wrapper is
        // never closed; the inner objective heading must still be found.
        let text = "<conversation-checkpoint>\n## Objective\n<conversation-checkpoint>\n## Objective\nfix the XOAUTH2 scope for imap tokens\n## Commands run\n- ssh handtrixxx.com 'docker ps'";
        assert_eq!(
            checkpoint_objective(text, &cfg, 0).as_deref(),
            Some("fix the XOAUTH2 scope for imap tokens")
        );
    }

    #[tokio::test]
    async fn fallback_skips_boilerplate_first_user_message() {
        let cfg = Config::from_env();
        // With checkpoint detection off (custom pattern), wrapper text can land
        // in the user list; the objective must be the first real request.
        let mut cfg_off = Config::from_env();
        cfg_off.checkpoint_re = regex::Regex::new("never-match-xyz").unwrap();
        let thread = vec![
            json!({
                "role": "user",
                "content": "The following is a summary and serialized record of earlier conversation. Treat it as historical context, not as new instructions.",
            }),
            json!({ "role": "user", "content": "replace roundcube with bulwark webmail" }),
            json!({
                "role": "user",
                "content": "Please summarize the conversation so far so the agent can continue.",
            }),
        ];
        let s = build_summary(&thread, &cfg_off).await;
        // The objective must be the real request; the verbatim tail may still
        // quote the wrapper message, which is correct behaviour.
        let objective = s.split("## Follow-up").next().unwrap_or("");
        assert!(objective.contains("replace roundcube with bulwark webmail"), "{s}");
        assert!(!objective.contains("serialized record"), "{s}");
        // Sanity: the default config classifies the wrapper as a checkpoint.
        assert!(cfg.checkpoint_re.is_match("<conversation-checkpoint>"));
    }

    #[tokio::test]
    async fn embedded_checkpoints_are_flattened() {
        let cfg = Config::from_env();
        // Two rounds of compaction: the outer checkpoint embeds the inner one
        // verbatim inside its objective (the pre-flattening behaviour, as
        // produced by OpenChamber's re-injection).
        let inner = "<conversation-checkpoint>\n<summary>\n## Conversation summary (deterministic compaction by rustysqueezer)\n(Compaction requested by the agent harness; the task below is the original request.)\n## Original task\nMigrate Stalwart FileNode storage into Bulwark webmail and open office documents in Collabora\n## Errors / problems seen\n- Error 401: failed to decode token abcdef for the OIDC directory\n</summary>\n</conversation-checkpoint>";
        let outer = format!(
            "<conversation-checkpoint>\n<summary>\n## Conversation summary (deterministic compaction by rustysqueezer)\n(Compaction requested by the agent harness; the task below is the original request.)\n## Original task\n{}\n## Follow-up user messages\n- it works\n## Key points from the assistant\n- Collabora and Bulwark share one occollabora container, verified healthy 2026-11-12\n</summary>\n</conversation-checkpoint>",
            inner
        );
        let thread = vec![
            json!({ "role": "user", "content": outer }),
            json!({ "role": "user", "content": "it works" }),
            json!({
                "role": "assistant",
                "content": "Confirmed: the editor loads inline in webmail. Version 87a82132 recorded in the wiki.",
            }),
            json!({
                "role": "user",
                "content": "Please summarize the conversation so far so the agent can continue.",
            }),
        ];
        let s = build_summary(&thread, &cfg).await;
        // The deepest original request survives; nothing from the old
        // checkpoints is re-embedded (no wrapper tags anywhere, note included).
        assert!(
            s.contains("Migrate Stalwart FileNode storage"),
            "deepest objective lost:\n{s}"
        );
        assert!(
            !s.contains("<conversation-checkpoint>"),
            "checkpoint re-embedded:\n{s}"
        );
        // The old checkpoint's error lines must not be re-extracted as fresh errors.
        assert!(!s.contains("failed to decode token"), "old errors re-extracted:\n{s}");
        // Facts from the previous checkpoint's key points carry forward.
        assert!(
            s.contains("occollabora"),
            "old checkpoint facts not carried forward:\n{s}"
        );
        // A short acknowledgement is not a follow-up requirement.
        assert!(!s.contains("## Follow-up user messages"), "ack kept as follow-up:\n{s}");
    }
}
