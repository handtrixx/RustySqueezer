mod config;
mod summary;
mod text;

use axum::{
    extract::{DefaultBodyLimit, Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::Value;
use std::time::{Duration, Instant};

use config::Config;
use summary::build_summary;
use text::{count_tokens, msg_text};

async fn health(State(cfg): State<Config>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "model": cfg.model,
        "max_summary_chars": cfg.max_summary_chars,
        "tail_turns": cfg.tail_turns,
    }))
}

async fn models(State(cfg): State<Config>) -> impl IntoResponse {
    Json(serde_json::json!({"object": "list", "data": [{"id": cfg.model, "object": "model"}]}))
}

// Per-request observability: surface, input/output tokens, compression
// ratio and latency, so usage and quality are visible in container logs.
fn log_usage(surface: &str, in_tokens: usize, out_tokens: usize, elapsed: Duration) {
    let ratio = if out_tokens > 0 {
        in_tokens as f64 / out_tokens as f64
    } else {
        0.0
    };
    println!(
        "[usage] {surface} in={in_tokens} out={out_tokens} ratio={ratio:.1} latency_ms={}",
        elapsed.as_millis()
    );
}

fn thread_from_body(body: &Value) -> Vec<Value> {
    body.get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
}

async fn completions(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
    let started = Instant::now();
    let messages = thread_from_body(&body);
    let stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let summary = build_summary(&messages, &cfg).await;

    let prompt_tokens: usize = messages.iter().map(|m| count_tokens(&msg_text(m))).sum();
    let completion_tokens = count_tokens(&summary);
    let usage = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
    });

    log_usage("openai", prompt_tokens, completion_tokens, started.elapsed());

    if !stream {
        return Json(serde_json::json!({
            "id": cfg.model,
            "object": "chat.completion",
            "created": chrono_like_now(),
            "model": body.get("model").and_then(|m| m.as_str()).unwrap_or(cfg.model.as_str()),
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": summary},
                "finish_reason": "stop",
            }],
            "usage": usage,
        }))
        .into_response();
    }

    // SSE: 2000-char delta chunks, then a finish delta, then [DONE].
    let chars: Vec<char> = summary.chars().collect();
    let mut sse = String::new();
    for chunk in chars.chunks(2000) {
        let text: String = chunk.iter().collect();
        sse.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": text}}]})
        ));
    }
    sse.push_str(
        "data: {\"choices\": [{\"index\": 0, \"delta\": {}, \"finish_reason\": \"stop\"}]}\n\n",
    );
    sse.push_str("data: [DONE]\n\n");
    (StatusCode::OK, [("content-type", "text/event-stream")], sse).into_response()
}

fn chrono_like_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Container healthcheck for the shell-free (scratch) image: a raw std-TCP
// probe of our own /health endpoint, so compose needs no curl in the image.
fn self_health(port: u16) -> i32 {
    use std::io::{Read, Write};
    let mut stream = match std::net::TcpStream::connect(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return 1;
    }
    let mut buf = Vec::new();
    if stream.read_to_end(&mut buf).is_err() {
        return 1;
    }
    if String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200") {
        0
    } else {
        1
    }
}

// Anthropic Messages API surface: POST /v1/messages.
// Anthropic puts the system prompt in a top-level `system` field; message
// content may be a string or an array of text blocks (already handled by
// msg_text). The compaction logic is identical to the OpenAI surface.
async fn anthropic_messages(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
    let started = Instant::now();
    let mut messages = thread_from_body(&body);
    if let Some(system) = body.get("system").and_then(|s| s.as_str()) {
        messages.insert(0, serde_json::json!({"role": "system", "content": system}));
    }
    let summary = build_summary(&messages, &cfg).await;
    let input_tokens: usize = messages.iter().map(|m| count_tokens(&msg_text(m))).sum();
    let output_tokens = count_tokens(&summary);
    log_usage("anthropic", input_tokens, output_tokens, started.elapsed());
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(cfg.model.as_str());
    let id = format!("msg_{}", chrono_like_now());

    // LiteLLM gateway-native compaction: the child request carries a
    // `compaction` directive (e.g. {"type": "summarize"}) and is always
    // sent with stream:false. Reply with an Anthropic native compaction
    // block (type "compaction", non-empty signature, stop_reason
    // "compaction") so the gateway can splice the summary into the
    // parent thread. Detect on the body key; the beta header is not
    // needed and Axum's Json extractor would hide it.
    if body.get("compaction").is_some() {
        let text = if summary.trim().is_empty() {
            "No conversation history available to summarize.".to_string()
        } else {
            summary
        };
        log_usage("anthropic-compaction", input_tokens, output_tokens, started.elapsed());
        return Json(serde_json::json!({
            "type": "message",
            "id": id,
            "role": "assistant",
            "model": model,
            "content": [{"type": "compaction", "content": text, "signature": "rustysqueezer"}],
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            },
            "stop_reason": "compaction",
        }))
        .into_response();
    }

    if body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        let mut sse = String::new();
        sse.push_str(&format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"{id}\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"{model}\",\"content\":[]}}}}\n\n"
        ));
        sse.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
        let chars: Vec<char> = summary.chars().collect();
        for chunk in chars.chunks(2000) {
            let text: String = chunk.iter().collect();
            sse.push_str(&format!(
                "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\n",
                serde_json::json!(text)
            ));
        }
        sse.push_str(
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        );
        sse.push_str(&format!(
            "event: message_end\ndata: {{\"type\":\"message_end\",\"usage\":{{\"input_tokens\":{},\"output_tokens\":{}}}}}\n\n",
            input_tokens, output_tokens
        ));
        return (StatusCode::OK, [("content-type", "text/event-stream")], sse).into_response();
    }

    Json(serde_json::json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": summary}],
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        },
        "stop_reason": "end_turn",
    }))
    .into_response()
}

// Ollama native surface: POST /api/chat (messages) and POST /api/generate
// (single prompt), plus GET /api/tags for model discovery. Streaming uses
// Ollama's NDJSON format: one JSON object per line, final line with done.
async fn ollama_chat(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
    let started = Instant::now();
    let messages = thread_from_body(&body);
    let summary = build_summary(&messages, &cfg).await;
    let prompt_tokens: usize = messages.iter().map(|m| count_tokens(&msg_text(m))).sum();
    let eval_count = count_tokens(&summary);
    log_usage("ollama-chat", prompt_tokens, eval_count, started.elapsed());
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(cfg.model.as_str());
    let created = chrono_like_now();

    if body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        let mut ndjson = String::new();
        let chars: Vec<char> = summary.chars().collect();
        for chunk in chars.chunks(2000) {
            let text: String = chunk.iter().collect();
            ndjson.push_str(&format!(
                "{{\"model\":\"{model}\",\"created_at\":{created},\"response\":{}}}\n",
                serde_json::json!(text)
            ));
        }
        ndjson.push_str(&format!(
            "{{\"model\":\"{model}\",\"created_at\":{created},\"response\":\"\",\"done\":true,\"prompt_eval_count\":{prompt_tokens},\"eval_count\":{eval_count}}}\n"
        ));
        return (
            StatusCode::OK,
            [("content-type", "application/x-ndjson")],
            ndjson,
        )
            .into_response();
    }

    Json(serde_json::json!({
        "model": model,
        "created_at": created,
        "response": summary,
        "done": true,
        "prompt_eval_count": prompt_tokens,
        "eval_count": eval_count,
    }))
    .into_response()
}

async fn ollama_generate(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
    let started = Instant::now();
    let prompt = body
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or_default();
    let messages = vec![serde_json::json!({"role": "user", "content": prompt})];
    let summary = build_summary(&messages, &cfg).await;
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(cfg.model.as_str());
    let in_tokens = count_tokens(prompt);
    let out_tokens = count_tokens(&summary);
    log_usage("ollama-generate", in_tokens, out_tokens, started.elapsed());
    Json(serde_json::json!({
        "model": model,
        "created_at": chrono_like_now(),
        "response": summary,
        "done": true,
        "prompt_eval_count": in_tokens,
        "eval_count": out_tokens,
    }))
    .into_response()
}

async fn ollama_tags(State(cfg): State<Config>) -> impl IntoResponse {
    Json(serde_json::json!({
        "models": [{"name": cfg.model, "size": 0, "modified": "now"}]
    }))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    println!("shutdown signal received, draining in-flight requests");
}

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);

    if std::env::args().any(|a| a == "--health") {
        std::process::exit(self_health(port));
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .route("/v1/messages", post(anthropic_messages))
        .route("/api/chat", post(ollama_chat))
        .route("/api/generate", post(ollama_generate))
        .route("/api/tags", get(ollama_tags))
        // Default 2 MiB is too tight: a ~190k-token Zed thread is ~1.2 MB,
        // so large agent threads 413'd. 64 MiB leaves comfortable headroom for
        // a 1M-token session (roughly 6-8 MB of thread text in practice).
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(cfg);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("rustysqueezer listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
