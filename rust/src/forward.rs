// Forwarding mode: pass a compaction request through to a real model instead
// of answering extractively. Optional - off by default, so the extractive
// path stays the local default. On any upstream failure the caller falls back
// to the extractive summary.
//
// Roadmap item 18: OpenCode sends the session's tool schemas with its
// compaction request. Google's Gemini rejects draft-2020-12 JSON Schema
// keywords ("Unknown name \"exclusiveMinimum\" at
// tools[...].function_declarations"), so forwarded OpenAI-compatible tool
// schemas are sanitized to the keyword subset Gemini accepts. For the native
// Google provider the tool list is dropped outright - a compaction answer
// never needs to call tools - which sidesteps the schema dialect entirely.

use serde_json::{json, Value};

use crate::config::Config;
use crate::text::msg_text;

pub struct Forwarded {
    pub text: String,
    pub in_tokens: Option<usize>,
    pub out_tokens: Option<usize>,
}

/// Pointer lookup for local `$ref` values like `#/$defs/Foo`.
fn resolve_local_ref(root: &Value, ref_str: &str) -> Option<Value> {
    let path = ref_str.strip_prefix('#')?;
    let mut node = root;
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        let key = seg.replace("~1", "/").replace("~0", "~");
        node = node.get(&key)?;
    }
    Some(node.clone())
}

/// Rewrite one schema object to the keyword subset Gemini accepts:
/// exclusive bounds become inclusive ones (integers step by 1, floats drop),
/// local `$ref`s are inlined, and draft-2020-12 keywords Gemini does not
/// model (`$schema`, `additionalProperties`, `patternProperties`, `not`,
/// `prefixItems`) are removed. `defs_root` is the schema the `$ref`s point
/// into; it stays unmutated so pointers stay resolvable.
fn sanitize_schema(v: &mut Value, defs_root: &Value, depth: usize) {
    if depth > 8 {
        return;
    }
    match v {
        Value::Array(items) => {
            for it in items {
                sanitize_schema(it, defs_root, depth + 1);
            }
        }
        Value::Object(obj) => {
            for k in ["$schema", "$id", "$comment", "patternProperties", "not", "prefixItems"] {
                obj.remove(k);
            }
            // Gemini models bounds inclusively; an exclusive integer bound
            // becomes the next inclusive one. A float exclusive bound has no
            // Gemini equivalent and is simply dropped.
            if let Some(n) = obj.remove("exclusiveMinimum").and_then(|x| x.as_i64()) {
                obj.insert("minimum".to_string(), json!(n + 1));
            }
            if let Some(n) = obj.remove("exclusiveMaximum").and_then(|x| x.as_i64()) {
                obj.insert("maximum".to_string(), json!(n - 1));
            }
            obj.remove("additionalProperties");
            obj.remove("definitions");
            obj.remove("$defs");
            if let Some(r) = obj.remove("$ref") {
                if let Some(resolved) = r.as_str().and_then(|s| resolve_local_ref(defs_root, s)) {
                    if let Some(ro) = resolved.as_object() {
                        for (k, val) in ro {
                            obj.entry(k.clone()).or_insert(val.clone());
                        }
                    }
                }
            }
            // Recurse into every child: schemas nest arbitrarily deep
            // (`properties -> opts -> properties`), and only recursing into
            // the named keys left nested subschemas untouched.
            for (_, child) in obj.iter_mut() {
                sanitize_schema(child, defs_root, depth + 1);
            }
        }
        _ => {}
    }
}

/// Sanitize the `tools` array of a request body. OpenAI-shaped tools keep
/// their own `$defs` as the pointer root in `function.parameters`, Anthropic
///-shaped tools in `input_schema`; both are sanitized, then the emitted
/// schema drops the defs (everything is inlined by then).
pub fn sanitize_tools(body: &Value) -> Value {
    let mut out = body.clone();
    if let Some(tools) = out.get_mut("tools").and_then(|t| t.as_array_mut()) {
        for tc in tools {
            if let Some(params) = tc.get_mut("function").and_then(|f| f.get_mut("parameters")) {
                let defs_root = params.clone();
                sanitize_schema(params, &defs_root, 0);
            }
            // Anthropic-shaped tools carry the schema in `input_schema`.
            if let Some(params) = tc.get_mut("input_schema") {
                let defs_root = params.clone();
                sanitize_schema(params, &defs_root, 0);
            }
        }
    }
    out
}

/// Convert an OpenAI-shaped message list into Gemini `contents` plus
/// `systemInstruction`. Tool results become user parts (the ground truth the
// summary must see); assistant tool calls are rendered as short model parts.
fn gemini_body(messages: &[Value], cfg: &Config) -> Value {
    let mut system: String = String::new();
    let mut contents: Vec<Value> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let text = msg_text(m);
        match role {
            "system" => system.push_str(&text),
            "assistant" => {
                let mut parts: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
                if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in calls {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        let args = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("");
                        parts.push(json!({"text": format!("[tool call: {name} {args}]")}));
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            // user and tool results both feed the model as user parts.
            _ => {
                let prefix = if role == "tool" { "[tool result]\n" } else { "" };
                contents.push(json!({"role": "user", "parts": [{"text": format!("{prefix}{text}")}]}));
            }
        }
    }
    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "temperature": 0.0,
            "maxOutputTokens": cfg.forward_max_tokens,
            "seed": 0,
        },
    });
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    // The model id goes in the URL path of the REST API, not in the body.
    body
}

fn gemini_url(base: &str, model: &str) -> String {
    format!("{}/models/{}:generateContent", base.trim_end_matches('/'), model)
}

/// POST a Gemini generateContent request with the API key header. Shared by
/// forwarding mode and the Google assist backend.
pub(crate) fn gemini_request(base: &str, model: &str, api_key: &str, body: &Value) -> reqwest::RequestBuilder {
    crate::assist::client()
        .post(gemini_url(base, model))
        .header("x-goog-api-key", api_key)
        .json(body)
}

pub(crate) fn gemini_text(v: &Value) -> Option<String> {
    let parts = v.get("candidates")?.get(0)?.get("content")?.get("parts")?.as_array()?;
    let text: String = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn usage_tokens(v: &Value, container: &str, keys: [&str; 2]) -> (Option<usize>, Option<usize>) {
    // Gemini reports counts under `usageMetadata`, OpenAI under `usage`.
    let u = v.get(container);
    (
        u.and_then(|x| x.get(keys[0])).and_then(|n| n.as_u64()).map(|n| n as usize),
        u.and_then(|x| x.get(keys[1])).and_then(|n| n.as_u64()).map(|n| n as usize),
    )
}

/// Send the thread to the configured upstream and return its answer text.
/// Errors (connection, timeout, unusable body) are reported so the caller can
/// fall back to the extractive summary.
pub async fn forward(messages: &[Value], body: &Value, cfg: &Config) -> Result<Forwarded, String> {
    use std::time::Duration;
    use tokio::time::timeout;

    let dur = Duration::from_millis(cfg.forward_timeout_ms);
    let model = cfg.forward_model.clone();
    let req = if cfg.forward_provider == "google" {
        let base = if cfg.forward_base_url.is_empty() {
            crate::assist::google_default_base()
        } else {
            cfg.forward_base_url.clone()
        };
        crate::assist::client()
            .post(gemini_url(&base, &model))
            .header("x-goog-api-key", cfg.forward_api_key.as_str())
            .json(&gemini_body(messages, cfg))
    } else {
        let base = cfg.forward_base_url.trim_end_matches('/');
        let mut payload = sanitize_tools(body);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("model".to_string(), json!(model));
            obj.insert("stream".to_string(), json!(false));
        }
        let mut req = crate::assist::client().post(format!("{base}/chat/completions"));
        if !cfg.forward_api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", cfg.forward_api_key));
        }
        req.json(&payload)
    };

    let resp = timeout(dur, req.send())
        .await
        .map_err(|_| format!("upstream timeout after {} ms", cfg.forward_timeout_ms))?
        .map_err(|e| format!("upstream connection error: {e}"))?;
    let status = resp.status();
    let v: Value = timeout(dur, resp.json())
        .await
        .map_err(|_| "upstream timeout while reading the body".to_string())?
        .map_err(|e| format!("upstream returned no JSON body: {e}"))?;
    if !status.is_success() {
        let snippet: String = v.to_string().chars().take(300).collect();
        return Err(format!("upstream HTTP {status}: {snippet}"));
    }
    let text = if cfg.forward_provider == "google" {
        gemini_text(&v)
    } else {
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
    };
    let text = text.ok_or_else(|| "upstream returned an empty completion".to_string())?;
    let (in_tokens, out_tokens) = if cfg.forward_provider == "google" {
        usage_tokens(&v, "usageMetadata", ["promptTokenCount", "candidatesTokenCount"])
    } else {
        usage_tokens(&v, "usage", ["prompt_tokens", "completion_tokens"])
    };
    Ok(Forwarded {
        text,
        in_tokens,
        out_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults;
    use serde_json::json;

    #[test]
    fn usage_counts_read_gemini_and_openai_containers() {
        let gemini = json!({"candidates": [], "usageMetadata": {"promptTokenCount": 42, "candidatesTokenCount": 7}});
        assert_eq!(usage_tokens(&gemini, "usageMetadata", ["promptTokenCount", "candidatesTokenCount"]), (Some(42), Some(7)));
        let openai = json!({"choices": [], "usage": {"prompt_tokens": 18, "completion_tokens": 4}});
        assert_eq!(usage_tokens(&openai, "usage", ["prompt_tokens", "completion_tokens"]), (Some(18), Some(4)));
        assert_eq!(usage_tokens(&json!({}), "usageMetadata", ["promptTokenCount", "candidatesTokenCount"]), (None, None));
    }

    #[test]
    fn sanitize_converts_exclusive_bounds_and_drops_unsupported_keywords() {
        let body = json!({
            "model": "rustysqueezer",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "edit",
                    "parameters": {
                        "type": "object",
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "properties": {
                            "offset": { "type": "integer", "exclusiveMinimum": 0 },
                            "limit": { "type": "integer", "exclusiveMaximum": 2000 },
                            "ratio": { "type": "number", "exclusiveMinimum": 0.5 },
                            "extra": { "type": "string", "additionalProperties": false },
                        },
                        "patternProperties": { "^x": { "type": "string" } },
                    },
                },
            }],
        });
        let out = sanitize_tools(&body);
        let p = &out["tools"][0]["function"]["parameters"];
        assert!(p.get("$schema").is_none());
        assert!(p.get("patternProperties").is_none());
        assert_eq!(p["properties"]["offset"]["minimum"], json!(1));
        assert!(p["properties"]["offset"].get("exclusiveMinimum").is_none());
        assert_eq!(p["properties"]["limit"]["maximum"], json!(1999));
        // A float exclusive bound has no Gemini equivalent: dropped entirely.
        assert!(p["properties"]["ratio"].get("minimum").is_none());
        assert!(p["properties"]["extra"].get("additionalProperties").is_none());
    }

    #[test]
    fn sanitize_inlines_local_refs() {
        let body = json!({
            "tools": [{
                "function": {
                    "name": "shell",
                    "parameters": {
                        "type": "object",
                        "$defs": {
                            "Opts": {
                                "type": "object",
                                "properties": { "timeout": { "type": "integer", "exclusiveMinimum": 0 } },
                                "required": ["timeout"],
                            },
                        },
                        "properties": { "opts": { "$ref": "#/$defs/Opts" } },
                    },
                },
            }],
        });
        let out = sanitize_tools(&body);
        let p = &out["tools"][0]["function"]["parameters"];
        assert!(p.get("$defs").is_none());
        assert!(p["properties"]["opts"].get("$ref").is_none());
        assert_eq!(p["properties"]["opts"]["properties"]["timeout"]["minimum"], json!(1));
        assert_eq!(p["properties"]["opts"]["required"], json!(["timeout"]));
    }

    #[test]
    fn sanitize_leaves_schema_without_tools_untouched() {
        let body = json!({"model": "x", "messages": []});
        assert_eq!(sanitize_tools(&body), body);
    }

    #[test]
    fn gemini_body_maps_roles_and_keeps_tool_results() {
        let cfg = defaults();
        let msgs = vec![
            json!({"role": "system", "content": "be terse"}),
            json!({"role": "user", "content": "fix the compose file"}),
            json!({
                "role": "assistant",
                "content": "running docker now",
                "tool_calls": [{ "function": { "name": "terminal", "arguments": "{\"command\":\"docker ps\"}" } }],
            }),
            json!({"role": "tool", "content": "rustysqueezer up healthy"}),
        ];
        let b = gemini_body(&msgs, &cfg);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], json!("be terse"));
        assert_eq!(b["contents"][0]["role"], json!("user"));
        assert_eq!(b["contents"][1]["role"], json!("model"));
        assert!(b["contents"][1]["parts"][1]["text"]
            .as_str()
            .unwrap()
            .contains("tool call: terminal"));
        assert_eq!(b["contents"][2]["role"], json!("user"));
        assert!(b["contents"][2]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[tool result]"));
        assert_eq!(b["generationConfig"]["temperature"], json!(0.0));
    }

    #[test]
    fn gemini_text_reads_candidates_and_rejects_empty() {
        let ok = json!({"candidates": [{"content": {"parts": [{"text": "## Summary\n- fact"}]}}]});
        assert_eq!(gemini_text(&ok).as_deref(), Some("## Summary\n- fact"));
        assert_eq!(gemini_text(&json!({"candidates": []})), None);
        assert_eq!(
            gemini_text(&json!({"candidates": [{"content": {"parts": [{"text": "  "}]}}]})),
            None
        );
    }
}
