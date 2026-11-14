use std::collections::HashMap;
use std::env;

use regex::Regex;
use serde_json::Value;

const DEFAULT_COMPACT_PATTERN: &str =
    "summariz|summarise|compact|condense|conversation so far|context window";
// A message that embeds a previously emitted compaction checkpoint (the harness
// re-injects the last summary into the next compaction input). Without this,
// compaction #2 embeds compaction #1 verbatim as the objective, #3 embeds #2,
// and the user's actual first request is lost mid-word by the clip.
const DEFAULT_CHECKPOINT_PATTERN: &str =
    "conversation-checkpoint|deterministic compaction by rustysqueezer";
// Short acknowledgements ("it works", "thanks") are not requirements; they used
// to fill the follow-ups section as if they were new instructions.
const DEFAULT_FOLLOWUP_SKIP_PATTERN: &str =
    "^(it|that|this|everything|all) works?$|^(ok|okay|thanks|thank you|nice|great|perfect|done|lgtm|yep|yes|no|cool|wow)[!.]*$";
// A bare "error"/"cannot" also occurs in ordinary prose ("great error messages",
// a quoted `timeout: 5s`), which filled the errors section with noise on real
// threads. Require an actual failure context.
const DEFAULT_ERROR_PATTERN: &str = r"error[:\)]|errors? (?:occurred|during|on|in )|failed to|failed with|failed \(exit|exit code \d|traceback|exception|connection refused|permission denied|unable to|not found|no such file|timed out|status: failed|fatal|command not found";
const DEFAULT_FILE_EXTENSIONS: &[&str] = &[
    "py",
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "json",
    "ya?ml",
    "toml",
    "md",
    "sh",
    "bash",
    "html",
    "css",
    "sql",
    "txt",
    "log",
    "lock",
    "go",
    "cpp",
    "c",
    "h",
    "Dockerfile",
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
    // Config-file driven behaviour (see config.example.json): detection rules,
    // error/file patterns, enabled sections and section titles.
    pub instruction_max_chars: usize,
    pub compact_re: Regex,
    pub error_re: Regex,
    pub file_re: Regex,
    // Matches messages that embed a previous compaction checkpoint (see
    // DEFAULT_CHECKPOINT_PATTERN).
    pub checkpoint_re: Regex,
    // Matches short acknowledgements that must not be listed as follow-ups.
    pub followup_skip_re: Regex,
    pub sections: Vec<String>,
    pub titles: HashMap<String, String>,
    // Budget share (percent of max_summary_chars) reserved for each section.
    // The tail gets the largest slice: it is the part the agent needs verbatim
    // to continue, so it is budgeted before everything else.
    pub budgets: HashMap<String, usize>,
    // Small-model assist: condense the key-points bullets with a local
    // llama.cpp server (map-reduce over chunks). Empty base_url = off, and
    // the extractive summary stays byte-identical to v1.0.6. Only key_points
    // is condensed; tail, commands, files and errors stay verbatim.
    pub assist_base_url: String,
    pub assist_model: String,
    pub assist_timeout_ms: u64,
    pub assist_max_chunks: usize,
    pub assist_chunk_chars: usize,
    pub assist_max_tokens: usize,
    // Assist backend dialect: "openai" (OpenAI-compatible server such as
    // llama.cpp; needs assist_base_url) or "google" (Gemini REST API; needs
    // assist_api_key, base URL optional). Keys are never logged or echoed.
    pub assist_provider: String,
    pub assist_api_key: String,
    // Forwarding mode (roadmap item 18): instead of answering extractively,
    // pass the request through to a real model. Off by default; any upstream
    // failure falls back to the extractive summary. Same provider dialect as
    // assist: "google" needs forward_api_key, "openai" needs forward_base_url.
    pub forward_base_url: String,
    pub forward_provider: String,
    pub forward_model: String,
    pub forward_api_key: String,
    pub forward_timeout_ms: u64,
    pub forward_max_tokens: usize,
    // Emit the harness template's own headings when the compaction prompt
    // embeds one (OpenCode validates exact heading lines); off keeps the
    // Zed-default titles regardless.
    pub template_headings: bool,
}

fn env_int(name: &str, default: i64) -> usize {
    match env::var(name) {
        Ok(v) => v.parse::<i64>().unwrap_or(default).max(0) as usize,
        Err(_) => default as usize,
    }
}

fn default_titles() -> HashMap<String, String> {
    let mut t = HashMap::new();
    t.insert(
        "header".to_string(),
        "Conversation summary (deterministic compaction by rustysqueezer)".to_string(),
    );
    t.insert(
        "note".to_string(),
        "(Compaction requested by the agent harness; the task below is the original request.)"
            .to_string(),
    );
    t.insert("original_task".to_string(), "Original task".to_string());
    t.insert(
        "followups".to_string(),
        "Follow-up user messages".to_string(),
    );
    t.insert(
        "commands".to_string(),
        "Commands run (most recent first)".to_string(),
    );
    t.insert("files".to_string(), "Files/paths involved".to_string());
    t.insert("tools".to_string(), "Tools used".to_string());
    t.insert("errors".to_string(), "Errors / problems seen".to_string());
    t.insert(
        "key_points".to_string(),
        "Key points from the assistant".to_string(),
    );
    t.insert(
        "tail".to_string(),
        "Most recent turns (verbatim, clipped)".to_string(),
    );
    t
}

fn default_budgets() -> HashMap<String, usize> {
    // Shares are relative, so only the ratios matter. The tail is already
    // hard-capped by tail_chars per turn, giving it a 50 % share wasted headroom
    // while the key-points section - the actual recall surface - starved.
    let mut b = HashMap::new();
    b.insert("tail".to_string(), 30);
    b.insert("commands".to_string(), 12);
    b.insert("followups".to_string(), 10);
    b.insert("key_points".to_string(), 40);
    b.insert("errors".to_string(), 8);
    b.insert("files".to_string(), 10);
    b.insert("original_task".to_string(), 6);
    b.insert("tools".to_string(), 5);
    b
}

pub(crate) fn defaults() -> Config {
    Config {
        model: "rustysqueezer".to_string(),
        max_summary_chars: 12000,
        tail_turns: 6,
        tail_chars: 600,
        max_commands: 25,
        max_files: 60,
        max_errors: 20,
        max_key_points: 40,
        instruction_max_chars: 4000,
        compact_re: Regex::new(DEFAULT_COMPACT_PATTERN).unwrap(),
        // Error detection is case-insensitive.
        error_re: Regex::new(&format!("(?i){}", DEFAULT_ERROR_PATTERN)).unwrap(),
        // `\b` after the extension prevents domains from matching: without it
        // "github.com" matches the `c` extension and yields "github.c".
        file_re: Regex::new(&format!(
            r"[\w./-]+\.({})\b",
            DEFAULT_FILE_EXTENSIONS.join("|")
        ))
        .unwrap(),
        checkpoint_re: Regex::new(&format!("(?i){}", DEFAULT_CHECKPOINT_PATTERN)).unwrap(),
        followup_skip_re: Regex::new(&format!("(?i){}", DEFAULT_FOLLOWUP_SKIP_PATTERN)).unwrap(),
        budgets: default_budgets(),
        assist_base_url: String::new(),
        assist_model: "qwen3.5-2b".to_string(),
        // ~140 tokens per chunk across up to 3 chunks. rsq-aux serves with
        // --parallel 3, but a single GPU still serialises the batches: with a
        // 15 s per-chunk latency the 2nd and 3rd concurrent chunks always hit a
        // 15 s timeout (observed ok=0/3 on long threads). 45 s covers the
        // slowest queued chunk.
        assist_timeout_ms: 45000,
        assist_max_chunks: 3,
        assist_chunk_chars: 700,
        assist_max_tokens: 140,
        assist_provider: "openai".to_string(),
        assist_api_key: String::new(),
        forward_base_url: String::new(),
        forward_provider: "openai".to_string(),
        forward_model: String::new(),
        forward_api_key: String::new(),
        // Gemini flash answers a full thread in well under a minute; 90 s
        // covers slow cold starts without hanging the harness compaction.
        forward_timeout_ms: 90000,
        // Compaction summaries are short; 2048 keeps Gemini from rambling.
        forward_max_tokens: 2048,
        template_headings: false,
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
    if let Some(p) = v.get("checkpoint_pattern").and_then(|x| x.as_str()) {
        if let Ok(re) = Regex::new(&format!("(?i){}", p)) {
            cfg.checkpoint_re = re;
        }
    }
    if let Some(p) = v.get("followup_skip_pattern").and_then(|x| x.as_str()) {
        if let Ok(re) = Regex::new(&format!("(?i){}", p)) {
            cfg.followup_skip_re = re;
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
    if let Some(s) = v.get("assist_base_url").and_then(|x| x.as_str()) {
        cfg.assist_base_url = s.to_string();
    }
    if let Some(s) = v.get("assist_model").and_then(|x| x.as_str()) {
        cfg.assist_model = s.to_string();
    }
    if let Some(n) = v.get("assist_timeout_ms").and_then(|x| x.as_u64()) {
        cfg.assist_timeout_ms = n;
    }
    if let Some(n) = v.get("assist_max_chunks").and_then(|x| x.as_u64()) {
        cfg.assist_max_chunks = n as usize;
    }
    if let Some(n) = v.get("assist_chunk_chars").and_then(|x| x.as_u64()) {
        cfg.assist_chunk_chars = n as usize;
    }
    if let Some(n) = v.get("assist_max_tokens").and_then(|x| x.as_u64()) {
        cfg.assist_max_tokens = n as usize;
    }
    if let Some(s) = v.get("assist_provider").and_then(|x| x.as_str()) {
        cfg.assist_provider = s.to_string();
    }
    if let Some(s) = v.get("assist_api_key").and_then(|x| x.as_str()) {
        cfg.assist_api_key = s.to_string();
    }
    if let Some(s) = v.get("forward_base_url").and_then(|x| x.as_str()) {
        cfg.forward_base_url = s.to_string();
    }
    if let Some(s) = v.get("forward_provider").and_then(|x| x.as_str()) {
        cfg.forward_provider = s.to_string();
    }
    if let Some(s) = v.get("forward_model").and_then(|x| x.as_str()) {
        cfg.forward_model = s.to_string();
    }
    if let Some(s) = v.get("forward_api_key").and_then(|x| x.as_str()) {
        cfg.forward_api_key = s.to_string();
    }
    if let Some(n) = v.get("forward_timeout_ms").and_then(|x| x.as_u64()) {
        cfg.forward_timeout_ms = n;
    }
    if let Some(n) = v.get("forward_max_tokens").and_then(|x| x.as_u64()) {
        cfg.forward_max_tokens = n as usize;
    }
    if let Some(b) = v.get("template_headings").and_then(|x| x.as_bool()) {
        cfg.template_headings = b;
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
        if let Ok(v) = env::var("ASSIST_BASE_URL") {
            cfg.assist_base_url = v;
        }
        if let Ok(v) = env::var("ASSIST_MODEL") {
            cfg.assist_model = v;
        }
        cfg.assist_timeout_ms = env_int("ASSIST_TIMEOUT_MS", cfg.assist_timeout_ms as i64) as u64;
        cfg.assist_max_chunks = env_int("ASSIST_MAX_CHUNKS", cfg.assist_max_chunks as i64);
        cfg.assist_chunk_chars = env_int("ASSIST_CHUNK_CHARS", cfg.assist_chunk_chars as i64);
        cfg.assist_max_tokens = env_int("ASSIST_MAX_TOKENS", cfg.assist_max_tokens as i64);
        if let Ok(v) = env::var("ASSIST_PROVIDER") {
            cfg.assist_provider = v;
        }
        if let Ok(v) = env::var("ASSIST_API_KEY") {
            cfg.assist_api_key = v;
        }
        if let Ok(v) = env::var("FORWARD_BASE_URL") {
            cfg.forward_base_url = v;
        }
        if let Ok(v) = env::var("FORWARD_PROVIDER") {
            cfg.forward_provider = v;
        }
        if let Ok(v) = env::var("FORWARD_MODEL") {
            cfg.forward_model = v;
        }
        if let Ok(v) = env::var("FORWARD_API_KEY") {
            cfg.forward_api_key = v;
        }
        cfg.forward_timeout_ms = env_int("FORWARD_TIMEOUT_MS", cfg.forward_timeout_ms as i64) as u64;
        cfg.forward_max_tokens = env_int("FORWARD_MAX_TOKENS", cfg.forward_max_tokens as i64);
        if let Ok(v) = env::var("TEMPLATE_HEADINGS") {
            cfg.template_headings = matches!(v.as_str(), "1" | "true" | "yes" | "on");
        }
        cfg
    }

    /// True when the small-model assist is wired: an OpenAI-compatible server
    /// needs a base URL, Google needs an API key; a zero timeout disables it.
    pub fn assist_enabled(&self) -> bool {
        self.assist_timeout_ms > 0
            && match self.assist_provider.as_str() {
                "google" => !self.assist_api_key.is_empty(),
                _ => !self.assist_base_url.is_empty(),
            }
    }

    /// True when forwarding mode is wired: same provider dialect as assist,
    /// plus a model name. Off by default so the extractive path stays the
    /// local default; upstream failures fall back to it per request.
    pub fn forward_enabled(&self) -> bool {
        !self.forward_model.is_empty()
            && self.forward_timeout_ms > 0
            && match self.forward_provider.as_str() {
                "google" => !self.forward_api_key.is_empty(),
                _ => !self.forward_base_url.is_empty(),
            }
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
        assert!(cfg
            .compact_re
            .is_match("please summarize the conversation so far"));
        assert!(cfg.error_re.is_match("Error: connection refused"));
        assert!(cfg.error_re.is_match("HTTP Error 404: Not Found"));
        assert!(cfg.error_re.is_match("exit code 1"));
        // Prose and quoted config must not be reported as errors.
        assert!(!cfg.error_re.is_match("Rust has great error messages"));
        assert!(!cfg.error_re.is_match("timeout: 5s"));
        assert!(!cfg
            .error_re
            .is_match("the old bug where the tail got chopped"));
        assert!(cfg.file_re.is_match("compose.yml"));
        // Domains must not be mistaken for files ("github.com" -> "github.c").
        assert!(!cfg
            .file_re
            .is_match("https://github.com/handtrixx/RustySqueezer"));
        assert!(cfg.file_re.is_match("docs/index.html"));
        assert_eq!(cfg.title("key_points"), "Key points from the assistant");
        assert_eq!(cfg.sections.len(), 8);
    }

    #[test]
    fn assist_defaults_off_and_config_enables_it() {
        let mut cfg = defaults();
        assert!(!cfg.assist_enabled());
        apply_json(
            &mut cfg,
            &json!({
                "assist_base_url": "http://rsq-aux:8080/v1",
                "assist_model": "qwen3.5-2b",
                "assist_timeout_ms": 9000,
                "assist_max_chunks": 2,
            }),
        );
        assert!(cfg.assist_enabled());
        assert_eq!(cfg.assist_timeout_ms, 9000);
        assert_eq!(cfg.assist_max_chunks, 2);
        apply_json(&mut cfg, &json!({ "assist_timeout_ms": 0 }));
        assert!(!cfg.assist_enabled());
    }

    #[test]
    fn google_assist_needs_key_not_base_url() {
        let mut cfg = defaults();
        apply_json(&mut cfg, &json!({ "assist_provider": "google", "assist_model": "gemini-3-flash" }));
        // Google without a key stays off; a key turns it on with no base URL.
        assert!(!cfg.assist_enabled());
        apply_json(&mut cfg, &json!({ "assist_api_key": "AIza-test" }));
        assert!(cfg.assist_enabled());
        // A key alone must not enable the OpenAI dialect.
        let mut openai = defaults();
        apply_json(&mut openai, &json!({ "assist_api_key": "AIza-test" }));
        assert!(!openai.assist_enabled());
    }

    #[test]
    fn forwarding_defaults_off_and_both_providers_gate_on() {
        let mut cfg = defaults();
        assert!(!cfg.forward_enabled());
        apply_json(
            &mut cfg,
            &json!({ "forward_base_url": "http://llama:8080/v1", "forward_model": "qwen3-32b" }),
        );
        assert!(cfg.forward_enabled());
        assert_eq!(cfg.forward_timeout_ms, 90000);
        assert_eq!(cfg.forward_max_tokens, 2048);
        apply_json(&mut cfg, &json!({ "forward_model": "" }));
        assert!(!cfg.forward_enabled());

        let mut g = defaults();
        apply_json(&mut g, &json!({ "forward_provider": "google", "forward_model": "gemini-3-flash" }));
        assert!(!g.forward_enabled());
        apply_json(&mut g, &json!({ "forward_api_key": "AIza-test" }));
        assert!(g.forward_enabled());
        apply_json(&mut g, &json!({ "forward_timeout_ms": 0 }));
        assert!(!g.forward_enabled());
    }

    #[test]
    fn template_headings_toggle() {
        let mut cfg = defaults();
        assert!(!cfg.template_headings);
        apply_json(&mut cfg, &json!({ "template_headings": true }));
        assert!(cfg.template_headings);
    }

    #[test]
    fn checkpoint_and_ack_patterns() {
        let cfg = defaults();
        assert!(cfg.checkpoint_re.is_match("<conversation-checkpoint>"));
        assert!(cfg
            .checkpoint_re
            .is_match("Conversation summary (deterministic compaction by rustysqueezer)"));
        assert!(!cfg.checkpoint_re.is_match("please fix the docker compose file"));
        assert!(cfg.followup_skip_re.is_match("it works"));
        assert!(cfg.followup_skip_re.is_match("Thanks!"));
        assert!(cfg.followup_skip_re.is_match("OK"));
        // A real instruction that merely starts with an acknowledgement word
        // must not be skipped.
        assert!(!cfg
            .followup_skip_re
            .is_match("it works but the editor frame is blank"));
        assert!(!cfg.followup_skip_re.is_match("Deploy the compactor to production"));
        // The timeout covers the slowest queued chunk on rsq-aux (3 chunks,
        // serialised by one GPU at ~15 s each).
        assert_eq!(cfg.assist_timeout_ms, 45000);
    }
}
