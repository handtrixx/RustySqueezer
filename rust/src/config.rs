use std::collections::HashMap;
use std::env;

use regex::Regex;
use serde_json::Value;

const DEFAULT_COMPACT_PATTERN: &str =
    "summariz|summarise|compact|condense|conversation so far|context window";
const DEFAULT_ERROR_PATTERN: &str =
    r"(?:^|\s)(?:error|errors|failed|fails|exception|traceback|denied|exit code|cannot|unable|not found|timeout)";
const DEFAULT_FILE_EXTENSIONS: &[&str] = &[
    "py", "rs", "ts", "tsx", "js", "jsx", "json", "ya?ml", "toml", "md", "sh", "bash", "html",
    "css", "sql", "txt", "log", "lock", "go", "cpp", "c", "h", "Dockerfile",
];

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub max_summary_chars: usize,
    pub tail_turns: usize,
    pub tail_chars: usize,
    pub max_commands: usize,
    pub max_files: usize,
    pub max_errors: usize,
    pub max_key_points: usize,
    pub assist_url: String,
    pub assist_model: String,
    pub assist_enabled: bool,
    pub assist_max_input_chars: usize,
    pub assist_max_tokens: usize,
    // Config-file driven behaviour (see config.example.json): detection rules,
    // error/file patterns, enabled sections and section titles.
    pub instruction_max_chars: usize,
    pub compact_re: Regex,
    pub error_re: Regex,
    pub file_re: Regex,
    pub sections: Vec<String>,
    pub titles: HashMap<String, String>,
    // Budget share (percent of max_summary_chars) reserved for each section.
    // The tail gets the largest slice: it is the part the agent needs verbatim
    // to continue, so it is budgeted before everything else.
    pub budgets: HashMap<String, usize>,
}

fn env_int(name: &str, default: i64) -> usize {
    match env::var(name) {
        Ok(v) => v.parse::<i64>().unwrap_or(default).max(0) as usize,
        Err(_) => default as usize,
    }
}

fn default_titles() -> HashMap<String, String> {
    let mut t = HashMap::new();
    t.insert("header".to_string(), "Conversation summary (deterministic compaction by rustysqueezer)".to_string());
    t.insert("note".to_string(), "(Compaction requested by the agent harness; the task below is the original request.)".to_string());
    t.insert("original_task".to_string(), "Original task".to_string());
    t.insert("followups".to_string(), "Follow-up user messages".to_string());
    t.insert("commands".to_string(), "Commands run (most recent first)".to_string());
    t.insert("files".to_string(), "Files/paths involved".to_string());
    t.insert("tools".to_string(), "Tools used".to_string());
    t.insert("errors".to_string(), "Errors / problems seen".to_string());
    t.insert("key_points".to_string(), "Key points from the assistant".to_string());
    t.insert("tail".to_string(), "Most recent turns (verbatim, clipped)".to_string());
    t.insert("assist".to_string(), "Small-model highlights".to_string());
    t
}

fn default_budgets() -> HashMap<String, usize> {
    let mut b = HashMap::new();
    b.insert("tail".to_string(), 50);
    b.insert("commands".to_string(), 12);
    b.insert("followups".to_string(), 10);
    b.insert("key_points".to_string(), 10);
    b.insert("errors".to_string(), 8);
    b.insert("files".to_string(), 8);
    b.insert("original_task".to_string(), 6);
    b.insert("tools".to_string(), 4);
    b.insert("assist".to_string(), 2);
    b
}

fn defaults() -> Config {
    Config {
        model: "rustysqueezer".to_string(),
        max_summary_chars: 12000,
        tail_turns: 6,
        tail_chars: 600,
        max_commands: 25,
        max_files: 60,
        max_errors: 20,
        max_key_points: 40,
        assist_url: String::new(),
        assist_model: "local-small".to_string(),
        assist_enabled: false,
        assist_max_input_chars: 2500,
        assist_max_tokens: 500,
        instruction_max_chars: 4000,
        compact_re: Regex::new(DEFAULT_COMPACT_PATTERN).unwrap(),
        // Error detection is case-insensitive.
        error_re: Regex::new(&format!("(?i){}", DEFAULT_ERROR_PATTERN)).unwrap(),
        file_re: Regex::new(&format!(
            r"[\w./-]+\.({})",
            DEFAULT_FILE_EXTENSIONS.join("|")
        ))
        .unwrap(),
        budgets: default_budgets(),
        sections: vec![
            "original_task".into(),
            "followups".into(),
            "commands".into(),
            "files".into(),
            "tools".into(),
            "errors".into(),
            "key_points".into(),
            "tail".into(),
        ],
        titles: default_titles(),
    }
}

// Merge a config-file JSON object over the defaults. Unknown keys are ignored.
fn apply_json(cfg: &mut Config, v: &Value) {
    if let Some(s) = v.get("model_name").and_then(|x| x.as_str()) {
        cfg.model = s.to_string();
    }
    if let Some(n) = v.get("instruction_max_chars").and_then(|x| x.as_u64()) {
        cfg.instruction_max_chars = n as usize;
    }
    if let Some(p) = v.get("error_pattern").and_then(|x| x.as_str()) {
        if let Ok(re) = Regex::new(&format!("(?i){}", p)) {
            cfg.error_re = re;
        }
    }
    if let Some(exts) = v.get("file_extensions").and_then(|x| x.as_array()) {
        let parts: Vec<String> = exts
            .iter()
            .filter_map(|e| e.as_str().map(|s| s.to_string()))
            .collect();
        if !parts.is_empty() {
            if let Ok(re) = Regex::new(&format!(r"[\w./-]+\.({})", parts.join("|"))) {
                cfg.file_re = re;
            }
        }
    }
    if let Some(patterns) = v.get("instruction_patterns").and_then(|x| x.as_array()) {
        let parts: Vec<String> = patterns
            .iter()
            .filter_map(|p| p.as_str().map(|s| format!("(?:{})", s)))
            .collect();
        if !parts.is_empty() {
            if let Ok(re) = Regex::new(&parts.join("|")) {
                cfg.compact_re = re;
            }
        }
    }
    if let Some(sections) = v.get("sections").and_then(|x| x.as_array()) {
        cfg.sections = sections
            .iter()
            .filter_map(|s| s.as_str().map(|x| x.to_string()))
            .collect();
    }
    if let Some(titles) = v.get("section_titles").and_then(|x| x.as_object()) {
        for (key, value) in titles {
            if let Some(s) = value.as_str() {
                cfg.titles.insert(key.clone(), s.to_string());
            }
        }
    }
    if let Some(shares) = v.get("section_budgets").and_then(|x| x.as_object()) {
        for (key, share) in shares {
            if let Some(n) = share.as_u64() {
                cfg.budgets.insert(key.clone(), n.min(100) as usize);
            }
        }
    }
}

impl Config {
    pub fn from_env() -> Config {
        let mut cfg = defaults();
        if let Ok(path) = env::var("CONFIG_PATH") {
            match std::fs::read_to_string(&path) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) => apply_json(&mut cfg, &v),
                    Err(e) => eprintln!("rustysqueezer: ignoring CONFIG_PATH {path}: {e}"),
                },
                Err(e) => eprintln!("rustysqueezer: ignoring CONFIG_PATH {path}: {e}"),
            }
        }
        // Environment variables win over the config file.
        if let Ok(v) = env::var("MODEL_NAME") {
            cfg.model = v;
        }
        cfg.max_summary_chars = env_int("MAX_SUMMARY_CHARS", cfg.max_summary_chars as i64);
        cfg.tail_turns = env_int("TAIL_TURNS", cfg.tail_turns as i64);
        cfg.tail_chars = env_int("TAIL_CHARS", cfg.tail_chars as i64);
        cfg.max_commands = env_int("MAX_COMMANDS", cfg.max_commands as i64);
        cfg.max_files = env_int("MAX_FILES", cfg.max_files as i64);
        cfg.max_errors = env_int("MAX_ERRORS", cfg.max_errors as i64);
        cfg.max_key_points = env_int("MAX_KEY_POINTS", cfg.max_key_points as i64);
        cfg.assist_max_input_chars =
            env_int("ASSIST_MAX_INPUT_CHARS", cfg.assist_max_input_chars as i64);
        cfg.assist_max_tokens = env_int("ASSIST_MAX_TOKENS", cfg.assist_max_tokens as i64);
        cfg.assist_url = env::var("ASSIST_MODEL_URL").unwrap_or(cfg.assist_url.clone());
        cfg.assist_model = env::var("ASSIST_MODEL").unwrap_or(cfg.assist_model.clone());
        let assist = env::var("ASSIST_ENABLED")
            .unwrap_or_else(|_| if cfg.assist_enabled { "1".to_string() } else { "false".to_string() })
            .to_lowercase();
        cfg.assist_enabled = assist == "1" || assist == "true" || assist == "yes";
        cfg
    }

    /// Chars a section's body may use: its share of the content budget (the
    /// part of max_summary_chars left after headers and section titles),
    /// normalised over the shares of the sections that are actually enabled so
    /// disabling a section hands its budget to the others.
    pub fn section_cap(&self, key: &str, content_budget: usize, total_share: usize) -> usize {
        let share = self.budgets.get(key).copied().unwrap_or(0);
        content_budget * share / total_share.max(1)
    }

    pub fn title(&self, key: &str) -> String {
        self.titles
            .get(key)
            .cloned()
            .unwrap_or_else(|| "Section".to_string())
    }

    pub fn section_enabled(&self, key: &str) -> bool {
        self.sections.iter().any(|s| s == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_file_overrides_defaults() {
        let mut cfg = defaults();
        apply_json(
            &mut cfg,
            &json!({
                "instruction_patterns": ["wrap up", "shrink the thread"],
                "sections": ["original_task", "commands", "tail"],
                "section_titles": {"commands": "Commands executed"},
                "file_extensions": ["py", "rs"],
            }),
        );
        assert!(cfg.compact_re.is_match("please wrap up the thread"));
        assert!(!cfg.compact_re.is_match("summarize the conversation so far"));
        assert_eq!(cfg.sections, vec!["original_task", "commands", "tail"]);
        assert_eq!(cfg.title("commands"), "Commands executed");
        assert!(cfg.file_re.is_match("main.rs"));
        assert!(!cfg.file_re.is_match("notes.md"));
    }

    #[test]
    fn defaults_match_config_example() {
        let cfg = defaults();
        assert!(cfg.compact_re.is_match("please summarize the conversation so far"));
        assert!(cfg.error_re.is_match("Error: connection refused"));
        assert!(cfg.file_re.is_match("compose.yml"));
        assert_eq!(cfg.title("key_points"), "Key points from the assistant");
        assert_eq!(cfg.sections.len(), 8);
    }
}
