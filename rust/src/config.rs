use std::env;

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
}

fn env_int(name: &str, default: i64) -> usize {
    match env::var(name) {
        Ok(v) => v.parse::<i64>().unwrap_or(default).max(0) as usize,
        Err(_) => default as usize,
    }
}

impl Config {
    pub fn from_env() -> Config {
        Config {
            model: env::var("MODEL_NAME").unwrap_or_else(|_| "local-compact".to_string()),
            max_summary_chars: env_int("MAX_SUMMARY_CHARS", 12000),
            tail_turns: env_int("TAIL_TURNS", 6),
            tail_chars: env_int("TAIL_CHARS", 600),
            max_commands: env_int("MAX_COMMANDS", 25),
            max_files: env_int("MAX_FILES", 60),
            max_errors: env_int("MAX_ERRORS", 20),
            max_key_points: env_int("MAX_KEY_POINTS", 40),
            assist_url: env::var("ASSIST_MODEL_URL").unwrap_or_default(),
            assist_model: env::var("ASSIST_MODEL").unwrap_or_else(|_| "local-small".to_string()),
            assist_enabled: {
                let v = env::var("ASSIST_ENABLED")
                    .unwrap_or_else(|_| "false".to_string())
                    .to_lowercase();
                v == "1" || v == "true"
            },
            assist_max_input_chars: env_int("ASSIST_MAX_INPUT_CHARS", 2500),
            assist_max_tokens: env_int("ASSIST_MAX_TOKENS", 500),
        }
    }
}
