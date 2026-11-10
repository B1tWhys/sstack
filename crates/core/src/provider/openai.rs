//! The Provider that runs Sessions on OpenAI models, through the Responses API.

use genai::adapter::AdapterKind;

use super::Effort;
use super::ProviderError;
use super::genai_chat::{Dialect, GenaiChat, capturing_options};

/// The name recorded on Provider Payloads from this Provider.
pub const PROVIDER: &str = "openai";
/// The most output tokens to allow in one response, reasoning included.
const MAX_OUTPUT_TOKENS: u32 = 64_000;

/// Connects to the OpenAI Responses API with `api_key`, asking for `effort` on every response.
/// `base_url` replaces the default API address, and must end with a slash.
///
/// OpenAI keeps nothing between requests. Each response's reasoning comes back encrypted in its
/// Provider Payload, and goes back with the rest of the Session on the next request. The
/// reasoning summaries stream as reasoning.
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
        adapter: AdapterKind::OpenAIResp,
        options: capturing_options(effort, MAX_OUTPUT_TOKENS),
    };
    GenaiChat::new(dialect, api_key, base_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::genai_chat::test_support::{call, read_tool, request_with, serve_once};
    use crate::provider::{Provider, StreamEvent};
    use crate::session::{Entry, StopReason};
    use serde_json::{Value, json};
    use std::fmt::Write as _;

    /// The model the tests ask for.
    const MODEL: &str = "gpt-6.1-sol";

    /// Returns `events` as a server-sent event stream.
    fn sse(events: &[Value]) -> String {
        let mut stream = String::new();
        for event in events {
            let name = event["type"].as_str().unwrap();
            writeln!(stream, "event: {name}\ndata: {event}\n").unwrap();
        }
        stream
    }

    /// Returns a response that reasons, says "Reading", and calls `read`.
    fn tool_use_stream() -> String {
        let reasoning =
            json!({"type": "reasoning", "id": "r1", "summary": [], "encrypted_content": "enc-1"});
        let message = json!({"type": "message", "id": "m1", "role": "assistant", "content": []});
        let function = json!({"type": "function_call", "id": "f1", "call_id": "c1", "name": "read", "arguments": ""});
        let called = json!({"type": "function_call", "id": "f1", "call_id": "c1", "name": "read", "arguments": "{\"path\":\"a\"}"});
        sse(&[
            json!({"type": "response.created", "response": {"id": "resp1"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": reasoning}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "Thinking"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": message}),
            json!({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "Reading"}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": function}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": called}),
            json!({"type": "response.completed", "response": {
                "id": "resp1", "status": "completed", "model": MODEL, "output": [],
                "usage": {"input_tokens": 11, "output_tokens": 7},
            }}),
        ])
    }

    fn provider(url: String) -> GenaiChat {
        connect("test-key".into(), Some(url), Effort::High).unwrap()
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
        assert!(lowered.starts_with("post /v1/responses "), "{request}");
        assert!(lowered.contains("authorization: bearer test-key"));
        assert!(request.contains("\"store\":false"));
        assert!(request.contains("\"reasoning.encrypted_content\""));
        assert!(request.contains("\"summary\":\"detailed\""));
        assert!(request.contains("\"effort\":\"high\""));
        assert!(request.contains("\"instructions\":\"be brief\""));
    }

    #[tokio::test]
    async fn the_next_request_carries_the_encrypted_reasoning_back() {
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
            request.contains("\"encrypted_content\":\"enc-1\""),
            "{request}"
        );
        assert!(request.contains("\"type\":\"function_call\""));
        assert!(request.contains("\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\""));
        assert!(request.contains("\"type\":\"function_call_output\""));
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
