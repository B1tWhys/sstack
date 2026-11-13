//! The Provider that runs Sessions on DeepSeek models, through its OpenAI-compatible Chat
//! Completions API.

use genai::adapter::AdapterKind;

use super::Effort;
use super::ProviderError;
use crate::connection::provider::genai_chat::{Dialect, GenaiChat, capturing_options};

/// The name recorded on Provider Payloads from this Provider.
pub const PROVIDER: &str = "deepseek";
/// The most output tokens to allow in one response, reasoning included.
const MAX_OUTPUT_TOKENS: u32 = 64_000;

/// Connects to the DeepSeek API with `api_key`, asking for `effort` on every response.
/// `base_url` replaces the default API address, and must end with a slash.
///
/// DeepSeek keeps nothing between requests. Each response's reasoning comes back in plain text
/// in its Provider Payload, and goes back with the rest of the Session on the next request, which
/// DeepSeek requires once tools are offered. DeepSeek serves fewer effort levels than a
/// Connection can ask for, and rounds the others to the nearest it has.
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
        adapter: AdapterKind::DeepSeek,
        options: capturing_options(effort, MAX_OUTPUT_TOKENS),
    };
    GenaiChat::new(dialect, api_key, base_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{
        Provider, StreamEvent,
        provider::genai_chat::test_support::{call, read_tool, request_with, serve_once},
    };
    use crate::session::{Entry, StopReason};
    use serde_json::{Value, json};
    use std::fmt::Write as _;

    /// The model the tests ask for.
    const MODEL: &str = "deepseek-v4-pro";

    /// Returns `chunks` as a server-sent event stream, ended the way DeepSeek ends one.
    fn sse(chunks: &[Value]) -> String {
        let mut stream = String::new();
        for chunk in chunks {
            writeln!(stream, "data: {chunk}\n").unwrap();
        }
        stream.push_str("data: [DONE]\n\n");
        stream
    }

    /// Returns a chunk of the response whose choice carries `delta` and `finish_reason`.
    fn chunk(delta: &Value, finish_reason: &Value) -> Value {
        json!({
            "id": "resp1", "object": "chat.completion.chunk", "model": MODEL,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        })
    }

    /// Returns a response that reasons, says "Reading", and calls `read`.
    fn tool_use_stream() -> String {
        let call = json!({"index": 0, "id": "c1", "type": "function",
            "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}});
        let mut done = chunk(&json!({}), &json!("tool_calls"));
        done["usage"] = json!({"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18});
        sse(&[
            chunk(
                &json!({"role": "assistant", "reasoning_content": "Thinking"}),
                &Value::Null,
            ),
            chunk(&json!({"content": "Reading"}), &Value::Null),
            chunk(&json!({"tool_calls": [call]}), &Value::Null),
            done,
        ])
    }

    fn provider(url: String) -> GenaiChat {
        connect("test-key".into(), Some(url), Effort::Max).unwrap()
    }

    #[tokio::test]
    async fn a_streamed_tool_call_becomes_a_neutral_entry_with_its_reasoning_kept() {
        let (url, server) = serve_once("200 OK", tool_use_stream());
        let entries = [Entry::User { text: "hi".into() }];
        let tools = [read_tool()];
        let mut seen = Vec::new();

        let entry = provider(url)
            .respond(request_with(MODEL, &entries, &tools), &mut |event| {
                seen.push(event);
            })
            .await
            .unwrap();

        assert_eq!(
            seen,
            [
                StreamEvent::Reasoning("Thinking".into()),
                StreamEvent::Text("Reading".into())
            ]
        );
        assert_eq!(entry.text, "Reading");
        assert_eq!(entry.tool_calls, [call()]);
        assert_eq!(entry.stop, StopReason::ToolUse);
        assert_eq!(entry.usage.input_tokens, 11);
        assert_eq!(entry.usage.output_tokens, 7);
        let payload = entry.payload.as_ref().unwrap();
        assert_eq!(payload.provider, PROVIDER);
        assert_eq!(payload.model, MODEL);

        let request = server.join().unwrap();
        let lowered = request.to_lowercase();
        assert!(
            lowered.starts_with("post /v1/chat/completions "),
            "{request}"
        );
        assert!(lowered.contains("authorization: bearer test-key"));
        assert!(request.contains("\"thinking\":{\"type\":\"enabled\"}"));
        assert!(request.contains("\"reasoning_effort\":\"max\""));
        assert!(request.contains("\"content\":\"be brief\""));
    }

    #[tokio::test]
    async fn the_next_request_carries_the_reasoning_back() {
        let (url, server) = serve_once("200 OK", tool_use_stream());
        let first = [Entry::User { text: "hi".into() }];
        let tools = [read_tool()];
        let entry = provider(url)
            .respond(request_with(MODEL, &first, &tools), &mut |_| {})
            .await
            .unwrap();
        server.join().unwrap();

        let (url, server) = serve_once("200 OK", tool_use_stream());
        let second = [
            first[0].clone(),
            Entry::Assistant(entry),
            Entry::ToolResult {
                call_id: "c1".into(),
                output: "contents".into(),
                is_error: false,
            },
        ];
        provider(url)
            .respond(request_with(MODEL, &second, &tools), &mut |_| {})
            .await
            .unwrap();

        let request = server.join().unwrap();
        assert!(
            request.contains("\"reasoning_content\":\"Thinking\""),
            "{request}"
        );
        assert!(request.contains("\"tool_calls\":["));
        assert!(request.contains("\"tool_call_id\":\"c1\""));
    }

    #[tokio::test]
    async fn api_errors_are_reported() {
        let body = json!({"error": {"message": "bad key"}}).to_string();
        let (url, server) = serve_once("401 Unauthorized", body);
        let entries = [Entry::User { text: "hi".into() }];

        let error = provider(url)
            .respond(request_with(MODEL, &entries, &[]), &mut |_| {})
            .await
            .unwrap_err();

        assert!(error.to_string().contains("401"), "{error}");
        server.join().unwrap();
    }
}
