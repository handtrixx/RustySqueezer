mod config;
mod summary;
mod text;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::Value;

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

fn thread_from_body(body: &Value) -> Vec<Value> {
    body.get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
}

async fn completions(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
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
    let mut messages = thread_from_body(&body);
    if let Some(system) = body.get("system").and_then(|s| s.as_str()) {
        messages.insert(0, serde_json::json!({"role": "system", "content": system}));
    }
    let summary = build_summary(&messages, &cfg).await;
    let input_tokens: usize = messages.iter().map(|m| count_tokens(&msg_text(m))).sum();
    let output_tokens = count_tokens(&summary);
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(cfg.model.as_str());
    let id = format!("msg_{}", chrono_like_now());

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
    let messages = thread_from_body(&body);
    let summary = build_summary(&messages, &cfg).await;
    let prompt_tokens: usize = messages.iter().map(|m| count_tokens(&msg_text(m))).sum();
    let eval_count = count_tokens(&summary);
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
    Json(serde_json::json!({
        "model": model,
        "created_at": chrono_like_now(),
        "response": summary,
        "done": true,
        "prompt_eval_count": count_tokens(prompt),
        "eval_count": count_tokens(&summary),
    }))
    .into_response()
}

async fn ollama_tags(State(cfg): State<Config>) -> impl IntoResponse {
    Json(serde_json::json!({
        "models": [{"name": cfg.model, "size": 0, "modified": "now"}]
    }))
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
        .with_state(cfg);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("rustysqueezer listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
