use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// Stopwords used by the task-term scoring.
pub fn stop_words() -> std::collections::HashSet<String> {
    "the an and or to in for on with by been being \
     it that we you i he into about \
     then here what which who how do does did not no yes can could should \
     would will but if so any some all more other also"
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_./-]{3,}").unwrap());

/// Deterministic word-boundary token count:
/// each run of ASCII alphanumerics/apostrophes is one token, every other
/// non-whitespace character (punctuation, non-ASCII) counts as one. Not exact
/// cl100k BPE, but far closer to real tokenisation than chars/4.
pub fn count_tokens(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_alphanumeric() || ch == '\'' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '\'') {
                i += 1;
            }
            tokens += 1;
        } else if !ch.is_whitespace() {
            tokens += 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    tokens
}

/// Plain text of a message, handling string and multipart content.
pub fn msg_text(msg: &Value) -> String {
    match msg.get("content") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.is_object() && p.get("type") == Some(&Value::String("text".into())) {
                    Some(
                        p.get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
    }
}

pub fn is_compaction_instruction(text: &str, cfg: &crate::config::Config) -> bool {
    cfg.compact_re.is_match(&text.to_lowercase()) && text.chars().count() < cfg.instruction_max_chars
}

/// Split on newlines, then on sentence ends; keep order.
/// The regex crate has no lookbehind, so
/// sentence ends are detected manually.)
pub fn sentence_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for s in split_sentences(line) {
            let s = s.trim();
            if s.chars().count() >= 20 {
                out.push(s.to_string());
            }
        }
    }
    out
}

fn split_sentences(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if ".!?".contains(chars[i]) {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j > i + 1 {
                out.push(chars[start..=i].iter().collect());
                start = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect());
    }
    out
}

pub fn tokenize(text: &str) -> Vec<String> {
    TOKEN_RE
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

pub fn term_counts(text: &str) -> std::collections::HashMap<String, usize> {
    let stop = stop_words();
    let mut counts = std::collections::HashMap::new();
    for t in tokenize(text) {
        if !stop.contains(&t) {
            *counts.entry(t).or_insert(0) += 1;
        }
    }
    counts
}

pub fn file_matches(text: &str, cfg: &crate::config::Config) -> Vec<String> {
    cfg.file_re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

pub fn has_error(text: &str, cfg: &crate::config::Config) -> bool {
    cfg.error_re.is_match(text)
}

pub fn clip(text: &str, n: usize) -> String {
    let t = text.trim();
    let len = t.chars().count();
    if len <= n {
        return t.to_string();
    }
    if n < 3 {
        return "...".to_string();
    }
    let kept: String = t.chars().take(n - 3).collect();
    format!("{}...", kept)
}

pub fn dedup_keep_order(items: &[String], cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        if seen.insert(it.clone()) {
            out.push(it.clone());
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counting() {
        assert_eq!(count_tokens("Hello, world!"), 4);
        assert_eq!(count_tokens("docker compose up -d"), 5); // - and d count separately
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("don't"), 1);
        assert_eq!(count_tokens("caf\u{e9} status"), 3); // non-ASCII char = 1 token
    }
}
