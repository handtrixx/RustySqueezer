// Small-model assist: map-reduce condensation of the key-points section.
// The extractive pre-pass selects the important sentences; each chunk of
// bullets is condensed by a local llama.cpp server (Qwen3.5-2B), and the
// results are merged back deterministically. Everything else (tail,
// commands, files, errors) stays verbatim. On any chunk failure - timeout,
// connection error, unusable output - that chunk keeps its original bullets,
// so the summary is never worse than the pure extractive build.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::time::timeout;

use crate::config::Config;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .http1_only()
            .build()
            .expect("reqwest client")
    })
}

/// Pack bullet lines into at most `max_chunks` chunks of <= `max_chars`
/// chars, preserving order. Lines that fall outside the chunk budget are
/// returned as `rest` and keep their original wording.
pub fn chunk_lines(lines: &[String], max_chars: usize, max_chunks: usize) -> (Vec<Vec<String>>, Vec<String>) {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut size = 0usize;
    let mut rest: Vec<String> = Vec::new();
    for line in lines {
        let cost = line.chars().count() + 1;
        if size + cost > max_chars {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                size = 0;
            }
            if chunks.len() >= max_chunks {
                rest.push(line.clone());
                continue;
            }
        }
        if chunks.len() >= max_chunks {
            rest.push(line.clone());
            continue;
        }
        current.push(line.clone());
        size += cost;
    }
    if !current.is_empty() && chunks.len() < max_chunks {
        chunks.push(current);
    } else {
        rest.extend(current);
    }
    (chunks, rest)
}

/// Turn model output into bullet lines: trim, drop empty/comment lines,
/// normalise bullets to "-" and strip echoed leading dashes (Qwen sometimes
/// re-emits the input bullets as "- - fact"). Returns None if nothing usable
/// came back.
fn parse_bullets(text: &str) -> Option<Vec<String>> {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with('-') || l.starts_with('*'))
        .map(|l| {
            let body = l[1..].trim().trim_start_matches(['-', '*']).trim();
            if body.is_empty() {
                String::new()
            } else {
                format!("- {body}")
            }
        })
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

const CONDENSE_PROMPT: &str = "Rewrite these bullet notes into shorter bullet lines for an AI agent's conversation summary.\n\
Rules: keep every number, path, command, version, IP and error wording verbatim; merge near-duplicates; one line per distinct fact; \
every line starts with '-'; no preamble, no commentary, no thinking.\n\nNotes:\n";

/// Condense one chunk. Returns None on any failure (timeout, HTTP error,
/// unusable body) so the caller falls back to the original bullets.
async fn condense_chunk(cfg: &Config, bullets: &[String]) -> Option<Vec<String>> {
    let base = cfg.assist_base_url.trim_end_matches('/');
    let body = json!({
        "model": cfg.assist_model,
        "messages": [
            {"role": "user", "content": format!("{CONDENSE_PROMPT}{}", bullets.join("\n"))}
        ],
        // Greedy decoding keeps the condensed output reproducible across runs.
        "temperature": 0.0,
        "seed": 0,
        "max_tokens": cfg.assist_max_tokens,
        "stream": false,
    });
    let resp = timeout(
        Duration::from_millis(cfg.assist_timeout_ms),
        client()
            .post(format!("{base}/chat/completions"))
            .json(&body)
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    let v: Value = timeout(Duration::from_millis(cfg.assist_timeout_ms), resp.json())
        .await
        .ok()?
        .ok()?;
    let text = v.get("choices")?.get(0)?.get("message")?.get("content")?.as_str()?;
    parse_bullets(text)
}

pub struct AssistOutcome {
    pub chunks: usize,
    pub ok: usize,
    pub ms: u128,
}

/// Condense the selected key-point bullets chunk by chunk (concurrently),
/// merging condensed lines for successful chunks and original bullets for
/// failed ones; lines beyond the chunk budget keep their original wording.
pub async fn condense_points(lines: &[String], cfg: &Config) -> (Vec<String>, AssistOutcome) {
    let started = Instant::now();
    let (chunks, rest) = chunk_lines(lines, cfg.assist_chunk_chars, cfg.assist_max_chunks);
    let n = chunks.len();

    // Spawn each chunk on the multi-thread runtime so the llama.cpp server
    // serves them in parallel slots instead of one after another.
    let handles: Vec<tokio::task::JoinHandle<Option<Vec<String>>>> = chunks
        .iter()
        .map(|c| {
            let bullets = c.clone();
            let cfg = cfg.clone();
            tokio::spawn(async move { condense_chunk(&cfg, &bullets).await })
        })
        .collect();

    let mut merged: Vec<String> = Vec::new();
    let mut ok = 0usize;
    for (i, h) in handles.into_iter().enumerate() {
        let res = h.await.unwrap_or(None);
        match res {
            Some(condensed) => {
                ok += 1;
                merged.extend(condensed);
            }
            None => merged.extend(chunks[i].clone()),
        }
    }
    merged.extend(rest);
    (
        merged,
        AssistOutcome {
            chunks: n,
            ok,
            ms: started.elapsed().as_millis(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_preserve_order_and_respect_budget() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("- fact {i} with some padding to make it long"))
            .collect();
        let (chunks, rest) = chunk_lines(&lines, 200, 2);
        assert!(chunks.len() <= 2);
        let covered: usize = chunks.iter().map(|c| c.len()).sum::<usize>() + rest.len();
        assert_eq!(covered, lines.len());
        // Order is preserved across chunks and the rest.
        let all: Vec<&String> = chunks.iter().flatten().chain(rest.iter()).collect();
        for (a, b) in all.iter().zip(lines.iter()) {
            assert_eq!(a.as_str(), b.as_str());
        }
    }

    #[test]
    fn parse_bullets_normalises_and_rejects_noise() {
        assert_eq!(
            parse_bullets("* kept one\n- kept two\nThinking Process:\n\n"),
            Some(vec!["- kept one".to_string(), "- kept two".to_string()])
        );
        // Qwen echoes the input bullets with a doubled dash; collapse it.
        assert_eq!(
            parse_bullets("- - fact one\n- -* fact two"),
            Some(vec!["- fact one".to_string(), "- fact two".to_string()])
        );
        assert_eq!(parse_bullets("no bullets here"), None);
        assert_eq!(parse_bullets("- \n-"), None);
    }
}
