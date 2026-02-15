use std::collections::HashMap;
use std::env;

/// An LLM API provider (e.g. Anthropic, OpenAI).
struct LlmProvider {
    name: &'static str,
    host: &'static str,
    env_var: &'static str,
    auth_header: &'static str,
    bearer_format: bool,
    endpoints: &'static [LlmEndpoint],
}

/// A specific API endpoint within a provider.
struct LlmEndpoint {
    name: &'static str,
    path_prefix: &'static str,
}

static PROVIDERS: &[LlmProvider] = &[
    LlmProvider {
        name: "anthropic",
        host: "api.anthropic.com",
        env_var: "CLAWPOT_ANTHROPIC_API_KEY",
        auth_header: "x-api-key",
        bearer_format: false,
        endpoints: &[LlmEndpoint {
            name: "messages",
            path_prefix: "/v1/messages",
        }],
    },
    LlmProvider {
        name: "openai",
        host: "api.openai.com",
        env_var: "CLAWPOT_OPENAI_API_KEY",
        auth_header: "authorization",
        bearer_format: true,
        endpoints: &[
            LlmEndpoint {
                name: "chat_completions",
                path_prefix: "/v1/chat/completions",
            },
            LlmEndpoint {
                name: "responses",
                path_prefix: "/v1/responses",
            },
        ],
    },
];

/// Holds server-managed API keys loaded from environment variables.
pub struct LlmKeyStore {
    keys: HashMap<String, String>,
}

impl LlmKeyStore {
    /// Load API keys from environment variables defined in the provider registry.
    pub fn from_env() -> Self {
        let mut keys = HashMap::new();
        for provider in PROVIDERS {
            if let Ok(key) = env::var(provider.env_var) {
                if !key.is_empty() {
                    keys.insert(provider.name.to_string(), key);
                }
            }
        }
        Self { keys }
    }

    fn get(&self, provider_name: &str) -> Option<&str> {
        self.keys.get(provider_name).map(String::as_str)
    }
}

/// Result of detecting an LLM API request.
pub struct LlmDetection {
    pub provider: String,
    pub endpoint: String,
    /// Header name to strip from the VM's request (the VM-provided auth header).
    pub strip_header: Option<String>,
    /// (header_name, header_value) to inject with the server-managed key.
    pub inject_header: Option<(String, String)>,
}

/// Check if a request targets a known LLM API. Returns detection info if so.
pub fn detect_llm_request(
    host: &str,
    path: &str,
    _headers: &HashMap<String, String>,
    key_store: &LlmKeyStore,
) -> Option<LlmDetection> {
    // Strip port from host for matching (e.g. "api.anthropic.com:443" -> "api.anthropic.com")
    let host_bare = host.split(':').next().unwrap_or(host);

    for provider in PROVIDERS {
        if !host_bare.eq_ignore_ascii_case(provider.host) {
            continue;
        }

        // Host matches — find the specific endpoint
        let endpoint_name = provider
            .endpoints
            .iter()
            .find(|ep| path.starts_with(ep.path_prefix))
            .map_or("unknown", |ep| ep.name);

        // Build key injection
        let (strip_header, inject_header) = if let Some(key) = key_store.get(provider.name) {
            let value = if provider.bearer_format {
                format!("Bearer {key}")
            } else {
                key.to_string()
            };
            (
                Some(provider.auth_header.to_string()),
                Some((provider.auth_header.to_string(), value)),
            )
        } else {
            // No server-managed key — pass through VM's key unmodified
            (None, None)
        };

        return Some(LlmDetection {
            provider: provider.name.to_string(),
            endpoint: endpoint_name.to_string(),
            strip_header,
            inject_header,
        });
    }

    None
}

/// A parsed SSE event.
struct SseEvent {
    event_type: Option<String>,
    data: String,
}

/// Parse raw SSE bytes into a sequence of events.
fn parse_sse(body: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(body);
    let mut events = Vec::new();

    // Split by double newline (frame delimiter)
    for frame in text.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }

        let mut event_type = None;
        let mut data_parts: Vec<&str> = Vec::new();

        for line in frame.lines() {
            if line.starts_with(':') {
                // Comment / keepalive — skip
            } else if let Some(rest) = line.strip_prefix("event:") {
                event_type = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                let data = rest.trim();
                if data != "[DONE]" {
                    data_parts.push(data);
                }
            }
        }

        if data_parts.is_empty() {
            continue;
        }

        events.push(SseEvent {
            event_type,
            data: data_parts.join("\n"),
        });
    }

    events
}

/// Given SSE events from a streaming response, reassemble into a single
/// coherent JSON response and extract usage stats.
/// Returns (reassembled_json, model, input_tokens, output_tokens).
fn reassemble_stream(
    endpoint: &str,
    events: &[SseEvent],
) -> (serde_json::Value, Option<String>, Option<u64>, Option<u64>) {
    match endpoint {
        "messages" => reassemble_anthropic_messages(events),
        "chat_completions" => reassemble_openai_chat(events),
        "responses" => reassemble_openai_responses(events),
        _ => (serde_json::Value::Null, None, None, None),
    }
}

fn reassemble_anthropic_messages(
    events: &[SseEvent],
) -> (serde_json::Value, Option<String>, Option<u64>, Option<u64>) {
    let mut id = serde_json::Value::Null;
    let mut model = None;
    let mut input_tokens = None;
    let mut output_tokens = None;
    let mut stop_reason = serde_json::Value::Null;
    let mut content_text = String::new();

    for event in events {
        let event_name = event.event_type.as_deref().unwrap_or("");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            continue;
        };

        match event_name {
            "message_start" => {
                if let Some(msg) = json.get("message") {
                    id = msg.get("id").cloned().unwrap_or_default();
                    model = msg
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(String::from);
                    input_tokens = msg
                        .pointer("/usage/input_tokens")
                        .and_then(serde_json::Value::as_u64);
                }
            }
            "content_block_delta" => {
                if let Some(delta) = json.get("delta") {
                    if delta.get("type").and_then(serde_json::Value::as_str) == Some("text_delta") {
                        if let Some(text) = delta.get("text").and_then(serde_json::Value::as_str) {
                            content_text.push_str(text);
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = json.get("delta") {
                    if let Some(sr) = delta.get("stop_reason") {
                        stop_reason = sr.clone();
                    }
                }
                output_tokens = json
                    .pointer("/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64);
            }
            _ => {}
        }
    }

    let reassembled = serde_json::json!({
        "id": id,
        "model": model,
        "stop_reason": stop_reason,
        "content": [{"type": "text", "text": content_text}],
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }
    });

    (reassembled, model, input_tokens, output_tokens)
}

fn reassemble_openai_chat(
    events: &[SseEvent],
) -> (serde_json::Value, Option<String>, Option<u64>, Option<u64>) {
    let mut id = serde_json::Value::Null;
    let mut model = None;
    let mut content_text = String::new();
    let mut finish_reason = serde_json::Value::Null;
    let mut input_tokens = None;
    let mut output_tokens = None;

    for event in events {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            continue;
        };

        if let Some(m) = json
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(String::from)
        {
            model = Some(m);
        }

        if id.is_null() {
            if let Some(i) = json.get("id") {
                id = i.clone();
            }
        }

        // Accumulate content deltas
        if let Some(choices) = json.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(text) = delta.get("content").and_then(serde_json::Value::as_str) {
                        content_text.push_str(text);
                    }
                }
                if let Some(fr) = choice.get("finish_reason") {
                    if !fr.is_null() {
                        finish_reason = fr.clone();
                    }
                }
            }
        }

        // Check for usage in the final chunk
        if let Some(usage) = json.get("usage") {
            input_tokens = usage
                .get("prompt_tokens")
                .and_then(serde_json::Value::as_u64);
            output_tokens = usage
                .get("completion_tokens")
                .and_then(serde_json::Value::as_u64);
        }
    }

    let reassembled = serde_json::json!({
        "id": id,
        "model": model,
        "choices": [{
            "message": {"role": "assistant", "content": content_text},
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
        }
    });

    (reassembled, model, input_tokens, output_tokens)
}

fn reassemble_openai_responses(
    events: &[SseEvent],
) -> (serde_json::Value, Option<String>, Option<u64>, Option<u64>) {
    // Look for the response.completed event which has the full response
    for event in events {
        if event.event_type.as_deref() != Some("response.completed") {
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            continue;
        };

        // The data is the full response object
        let response = json
            .get("response")
            .cloned()
            .unwrap_or_else(|| json.clone());

        let model = response
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        let input_tokens = response
            .pointer("/usage/input_tokens")
            .and_then(serde_json::Value::as_u64);
        let output_tokens = response
            .pointer("/usage/output_tokens")
            .and_then(serde_json::Value::as_u64);

        return (response, model, input_tokens, output_tokens);
    }

    (serde_json::Value::Null, None, None, None)
}

/// Process an LLM response body. Detects streaming (from content-type header),
/// parses SSE if streaming, returns (body_json, model, input_tokens, output_tokens).
pub fn process_response(
    endpoint: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> (serde_json::Value, Option<String>, Option<u64>, Option<u64>) {
    let is_streaming = content_type.is_some_and(|ct| ct.contains("text/event-stream"));

    if is_streaming {
        let events = parse_sse(body);
        reassemble_stream(endpoint, &events)
    } else {
        // Non-streaming JSON response
        let json: serde_json::Value =
            serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
        let model = json
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(String::from);

        // Anthropic uses input_tokens/output_tokens, OpenAI uses prompt_tokens/completion_tokens
        let input_tokens = json
            .pointer("/usage/input_tokens")
            .or_else(|| json.pointer("/usage/prompt_tokens"))
            .and_then(serde_json::Value::as_u64);
        let output_tokens = json
            .pointer("/usage/output_tokens")
            .or_else(|| json.pointer("/usage/completion_tokens"))
            .and_then(serde_json::Value::as_u64);

        (json, model, input_tokens, output_tokens)
    }
}

/// Extract a summary from the request body for the llm.request event.
/// Returns (model, message_count, streaming).
pub fn extract_request_summary(
    endpoint: &str,
    body: &[u8],
) -> (Option<String>, Option<usize>, Option<bool>) {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (None, None, None);
    };

    let model = json
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(String::from);
    let streaming = json.get("stream").and_then(serde_json::Value::as_bool);

    let message_count = match endpoint {
        "messages" | "chat_completions" => json
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        "responses" => match json.get("input") {
            Some(serde_json::Value::Array(arr)) => Some(arr.len()),
            Some(serde_json::Value::String(_)) => Some(1),
            _ => None,
        },
        _ => None,
    };

    (model, message_count, streaming)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key_store(keys: Vec<(&str, &str)>) -> LlmKeyStore {
        let mut map = HashMap::new();
        for (provider, key) in keys {
            map.insert(provider.to_string(), key.to_string());
        }
        LlmKeyStore { keys: map }
    }

    // --- Detection tests ---

    #[test]
    fn detect_anthropic_messages() {
        let ks = make_key_store(vec![("anthropic", "sk-ant-test")]);
        let headers = HashMap::new();
        let det = detect_llm_request("api.anthropic.com", "/v1/messages", &headers, &ks).unwrap();
        assert_eq!(det.provider, "anthropic");
        assert_eq!(det.endpoint, "messages");
        assert_eq!(det.strip_header.as_deref(), Some("x-api-key"));
        let (h, v) = det.inject_header.unwrap();
        assert_eq!(h, "x-api-key");
        assert_eq!(v, "sk-ant-test");
    }

    #[test]
    fn detect_anthropic_with_port() {
        let ks = make_key_store(vec![("anthropic", "sk-ant-test")]);
        let headers = HashMap::new();
        let det =
            detect_llm_request("api.anthropic.com:443", "/v1/messages", &headers, &ks).unwrap();
        assert_eq!(det.provider, "anthropic");
        assert_eq!(det.endpoint, "messages");
    }

    #[test]
    fn detect_openai_chat() {
        let ks = make_key_store(vec![("openai", "sk-openai-test")]);
        let headers = HashMap::new();
        let det =
            detect_llm_request("api.openai.com", "/v1/chat/completions", &headers, &ks).unwrap();
        assert_eq!(det.provider, "openai");
        assert_eq!(det.endpoint, "chat_completions");
        let (h, v) = det.inject_header.unwrap();
        assert_eq!(h, "authorization");
        assert_eq!(v, "Bearer sk-openai-test");
    }

    #[test]
    fn detect_openai_responses() {
        let ks = make_key_store(vec![("openai", "sk-openai-test")]);
        let headers = HashMap::new();
        let det = detect_llm_request("api.openai.com", "/v1/responses", &headers, &ks).unwrap();
        assert_eq!(det.provider, "openai");
        assert_eq!(det.endpoint, "responses");
    }

    #[test]
    fn detect_unknown_endpoint() {
        let ks = make_key_store(vec![("anthropic", "sk-ant-test")]);
        let headers = HashMap::new();
        let det = detect_llm_request("api.anthropic.com", "/v2/something", &headers, &ks).unwrap();
        assert_eq!(det.provider, "anthropic");
        assert_eq!(det.endpoint, "unknown");
    }

    #[test]
    fn detect_no_key_passthrough() {
        let ks = make_key_store(vec![]);
        let headers = HashMap::new();
        let det = detect_llm_request("api.anthropic.com", "/v1/messages", &headers, &ks).unwrap();
        assert_eq!(det.provider, "anthropic");
        assert!(det.strip_header.is_none());
        assert!(det.inject_header.is_none());
    }

    #[test]
    fn detect_non_llm_host() {
        let ks = make_key_store(vec![("anthropic", "sk-ant-test")]);
        let headers = HashMap::new();
        assert!(detect_llm_request("example.com", "/v1/messages", &headers, &ks).is_none());
    }

    // --- SSE parsing tests ---

    #[test]
    fn parse_sse_anthropic_format() {
        let body = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"delta\":{\"text\":\"hello\"}}\n\nevent: message_delta\ndata: {\"delta\":{}}\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type.as_deref(), Some("message_start"));
        assert_eq!(events[1].event_type.as_deref(), Some("content_block_delta"));
        assert_eq!(events[2].event_type.as_deref(), Some("message_delta"));
    }

    #[test]
    fn parse_sse_openai_chat_format() {
        let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n\ndata: [DONE]\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 2); // [DONE] skipped
        assert!(events[0].event_type.is_none());
    }

    #[test]
    fn parse_sse_with_comments() {
        let body = b": keepalive\n\ndata: {\"test\": true}\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"test\": true}");
    }

    // --- Stream reassembly tests ---

    #[test]
    fn reassemble_anthropic_stream() {
        let events = vec![
            SseEvent {
                event_type: Some("message_start".to_string()),
                data: r#"{"message":{"id":"msg_01","model":"claude-sonnet-4-20250514","usage":{"input_tokens":150}}}"#.to_string(),
            },
            SseEvent {
                event_type: Some("content_block_delta".to_string()),
                data: r#"{"delta":{"type":"text_delta","text":"Hello "}}"#.to_string(),
            },
            SseEvent {
                event_type: Some("content_block_delta".to_string()),
                data: r#"{"delta":{"type":"text_delta","text":"world"}}"#.to_string(),
            },
            SseEvent {
                event_type: Some("message_delta".to_string()),
                data: r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":85}}"#.to_string(),
            },
        ];

        let (json, model, input, output) = reassemble_stream("messages", &events);
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(input, Some(150));
        assert_eq!(output, Some(85));
        assert_eq!(
            json.pointer("/content/0/text")
                .and_then(serde_json::Value::as_str),
            Some("Hello world")
        );
    }

    #[test]
    fn reassemble_openai_chat_stream() {
        let events = vec![
            SseEvent {
                event_type: None,
                data: r#"{"id":"chatcmpl-01","model":"gpt-4o","choices":[{"delta":{"role":"assistant","content":"Hi"}}]}"#.to_string(),
            },
            SseEvent {
                event_type: None,
                data: r#"{"id":"chatcmpl-01","model":"gpt-4o","choices":[{"delta":{"content":" there"}}]}"#.to_string(),
            },
            SseEvent {
                event_type: None,
                data: r#"{"id":"chatcmpl-01","model":"gpt-4o","choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
            },
            SseEvent {
                event_type: None,
                data: r#"{"id":"chatcmpl-01","model":"gpt-4o","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#.to_string(),
            },
        ];

        let (json, model, input, output) = reassemble_stream("chat_completions", &events);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
        assert_eq!(input, Some(10));
        assert_eq!(output, Some(5));
        assert_eq!(
            json.pointer("/choices/0/message/content")
                .and_then(serde_json::Value::as_str),
            Some("Hi there")
        );
    }

    #[test]
    fn reassemble_openai_responses_stream() {
        let events = vec![
            SseEvent {
                event_type: Some("response.output_text.delta".to_string()),
                data: r#"{"delta":"Hello"}"#.to_string(),
            },
            SseEvent {
                event_type: Some("response.completed".to_string()),
                data: r#"{"response":{"id":"resp_01","model":"gpt-4o","output":[{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}],"usage":{"input_tokens":20,"output_tokens":10}}}"#.to_string(),
            },
        ];

        let (json, model, input, output) = reassemble_stream("responses", &events);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
        assert_eq!(input, Some(20));
        assert_eq!(output, Some(10));
        assert!(json.get("output").is_some());
    }

    // --- Request summary tests ---

    #[test]
    fn extract_summary_anthropic() {
        let body = br#"{"model":"claude-sonnet-4-20250514","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"user","content":"bye"}],"stream":true}"#;
        let (model, count, streaming) = extract_request_summary("messages", body);
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(count, Some(3));
        assert_eq!(streaming, Some(true));
    }

    #[test]
    fn extract_summary_openai_chat() {
        let body =
            br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}],"stream":false}"#;
        let (model, count, streaming) = extract_request_summary("chat_completions", body);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
        assert_eq!(count, Some(1));
        assert_eq!(streaming, Some(false));
    }

    #[test]
    fn extract_summary_openai_responses_array() {
        let body = br#"{"model":"gpt-4o","input":[{"role":"user","content":"hello"},{"role":"user","content":"world"}]}"#;
        let (model, count, streaming) = extract_request_summary("responses", body);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
        assert_eq!(count, Some(2));
        assert!(streaming.is_none());
    }

    #[test]
    fn extract_summary_openai_responses_string() {
        let body = br#"{"model":"gpt-4o","input":"hello"}"#;
        let (_, count, _) = extract_request_summary("responses", body);
        assert_eq!(count, Some(1));
    }

    // --- Non-streaming response parsing ---

    #[test]
    fn process_anthropic_non_streaming() {
        let body = br#"{"id":"msg_01","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Hello"}],"usage":{"input_tokens":100,"output_tokens":50}}"#;
        let (json, model, input, output) =
            process_response("messages", Some("application/json"), body);
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(input, Some(100));
        assert_eq!(output, Some(50));
        assert!(json.get("content").is_some());
    }

    #[test]
    fn process_openai_non_streaming() {
        let body = br#"{"id":"chatcmpl-01","model":"gpt-4o","choices":[{"message":{"role":"assistant","content":"Hi"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let (json, model, input, output) =
            process_response("chat_completions", Some("application/json"), body);
        assert_eq!(model.as_deref(), Some("gpt-4o"));
        assert_eq!(input, Some(10));
        assert_eq!(output, Some(5));
        assert!(json.get("choices").is_some());
    }
}
