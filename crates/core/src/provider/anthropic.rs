//! The Provider that runs Sessions on Claude models.

use genai::adapter::AdapterKind;
use genai::chat::ChatOptions;
use serde_json::json;

use super::Effort;
use super::ProviderError;
use super::genai_chat::{Dialect, GenaiChat, capturing_options};

/// The name recorded on Provider Payloads from this Provider.
pub const PROVIDER: &str = "anthropic";
/// The most output tokens to allow in one response.
const MAX_OUTPUT_TOKENS: u32 = 64_000;
/// The header that turns on retrying refused requests on another model, and tolerating outdated
/// thinking.
const BETA_FEATURES: (&str, &str) = (
    "anthropic-beta",
    "server-side-fallback-2026-07-01,thinking-binding-controls-2026-08-01",
);

/// Connects to the Claude API with `api_key`, asking for `effort` on every response.
/// `base_url` replaces the default API address, and must end with a slash.
///
/// # Errors
/// Returns an error if the connection cannot be set up.
pub fn connect(
    api_key: String,
    base_url: Option<String>,
    effort: Effort,
) -> Result<GenaiChat, ProviderError> {
    let dialect = Dialect {
        name: PROVIDER,
        adapter: AdapterKind::Anthropic,
        options: chat_options(effort),
    };
    GenaiChat::new(dialect, api_key, base_url)
}

/// Returns the options sent with every request.
fn chat_options(effort: Effort) -> ChatOptions {
    capturing_options(effort, MAX_OUTPUT_TOKENS)
        .with_extra_headers(BETA_FEATURES)
        .with_extra_body(json!({
            "thinking": {
                "type": "adaptive",
                "display": "summarized",
                // Thinking is tied to the system prompt and tools, which a Rebuild can change.
                // Losing the old thinking is better than the Session failing to resume.
                "block_binding": {"prefix_mismatch_behavior": "drop_block"},
            },
            "fallbacks": "default",
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::genai_chat::test_support::{call, read_tool, request_with, serve_once};
    use crate::provider::{Provider, StreamEvent};
    use crate::session::{Entry, StopReason};
    use serde_json::Value;
    use std::fmt::Write as _;

    fn sse(events: &[Value]) -> String {
        let mut stream = String::new();
        for event in events {
            let name = event["type"].as_str().unwrap();
            writeln!(stream, "event: {name}\ndata: {event}\n").unwrap();
        }
        stream
    }

    fn tool_use_stream() -> String {
        sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 11, "output_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Reading"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "c1", "name": "read", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 7}}),
            json!({"type": "message_stop"}),
        ])
    }

    fn provider(url: String) -> GenaiChat {
        connect("test-key".into(), Some(url), Effort::High).unwrap()
    }

    #[tokio::test]
    async fn a_streamed_tool_call_becomes_a_neutral_entry_with_a_replayable_payload() {
        let (url, server) = serve_once("200 OK", tool_use_stream());
        let entries = [Entry::User { text: "hi".into() }];
        let tools = [read_tool()];
        let mut text = String::new();

        let entry = provider(url)
            .respond(
                request_with("claude-opus-5-5", &entries, &tools),
                &mut |event| {
                    if let StreamEvent::Text(chunk) = event {
                        text.push_str(&chunk);
                    }
                },
            )
            .await
            .unwrap();

        assert_eq!(text, "Reading");
        assert_eq!(entry.text, "Reading");
        assert_eq!(entry.tool_calls, [call()]);
        assert_eq!(entry.stop, StopReason::ToolUse);
        assert_eq!(entry.usage.output_tokens, 7);
        let payload = entry.payload.unwrap();
        assert_eq!(payload.provider, PROVIDER);
        assert!(payload.data.to_string().contains("\"fn_name\":\"read\""));

        let request = server.join().unwrap().to_lowercase();
        assert!(request.contains("x-api-key: test-key"));
        assert!(request.contains("server-side-fallback-2026-07-01"));
        assert!(request.contains("thinking-binding-controls-2026-08-01"));
        assert!(request.contains("\"prefix_mismatch_behavior\":\"drop_block\""));
        assert!(request.contains("\"fallbacks\":\"default\""));
        assert!(request.contains("\"display\":\"summarized\""));
        assert!(request.contains("\"cache_control\""));
        assert!(request.contains("\"eager_input_streaming\":true"));
    }

    #[tokio::test]
    async fn api_errors_are_reported() {
        let body = json!({"type": "error", "error": {"message": "bad key"}}).to_string();
        let (url, server) = serve_once("401 Unauthorized", body);
        let entries = [Entry::User { text: "hi".into() }];

        let error = provider(url)
            .respond(request_with("claude-opus-5-5", &entries, &[]), &mut |_| {})
            .await
            .unwrap_err();

        assert!(error.to_string().contains("401"), "{error}");
        server.join().unwrap();
    }
}
