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

async fn completions(State(cfg): State<Config>, Json(body): Json<Value>) -> Response {
    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
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
        .with_state(cfg);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("rustysqueezer listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
