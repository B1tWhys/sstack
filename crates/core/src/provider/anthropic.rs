//! The Provider that runs Sessions on Claude models.

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use genai::adapter::AdapterKind;
use genai::chat::{
    CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart,
    MessageContent, ReasoningEffort, StreamEnd, Tool, ToolCall as GenaiToolCall, ToolResponse,
};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};
use serde_json::json;

use super::{Effort, ModelRequest, Provider, ProviderError, StreamEvent, ToolSpec};
use crate::session::{AssistantEntry, Entry, ProviderPayload, StopReason, ToolCall, Usage};

/// The name recorded on Provider Payloads from this Provider.
const PROVIDER: &str = "anthropic";
/// The most output tokens to allow in one response.
const MAX_OUTPUT_TOKENS: u32 = 64_000;
/// The header that turns on retrying refused requests on another model, and tolerating outdated
/// thinking.
const BETA_FEATURES: (&str, &str) = (
    "anthropic-beta",
    "server-side-fallback-2026-07-01,thinking-binding-controls-2026-08-01",
);

/// The Provider for Claude models.
pub struct AnthropicProvider {
    /// The underlying API client.
    client: Client,
    /// The options sent with every request.
    options: ChatOptions,
}

impl AnthropicProvider {
    /// Connects with `api_key`. `base_url` replaces the default API address, and must end with a
    /// slash.
    ///
    /// # Errors
    /// Returns an error if the connection cannot be set up.
    pub fn new(
        api_key: String,
        base_url: Option<String>,
        effort: Effort,
    ) -> Result<Self, ProviderError> {
        let resolver = ServiceTargetResolver::from_resolver_fn(
            move |target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                let endpoint = base_url
                    .as_deref()
                    .map_or(target.endpoint, Endpoint::from_owned);
                Ok(ServiceTarget {
                    endpoint,
                    auth: AuthData::from_single(api_key.clone()),
                    model: ModelIden::new(AdapterKind::Anthropic, target.model.model_name),
                })
            },
        );
        let client = Client::builder()
            .with_service_target_resolver(resolver)
            .build()
            .map_err(provider_error)?;
        Ok(Self {
            client,
            options: chat_options(effort),
        })
    }
}

/// Reports any failure as a Provider error.
fn provider_error(error: impl std::fmt::Display) -> ProviderError {
    ProviderError(error.to_string())
}

/// Returns the options sent with every request.
fn chat_options(effort: Effort) -> ChatOptions {
    ChatOptions::default()
        .with_max_tokens(MAX_OUTPUT_TOKENS)
        .with_reasoning_effort(reasoning_effort(effort))
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_reasoning_content(true)
        .with_capture_tool_calls(true)
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

/// Converts a neutral effort level to `genai`'s equivalent.
fn reasoning_effort(effort: Effort) -> ReasoningEffort {
    match effort {
        Effort::Low => ReasoningEffort::Low,
        Effort::Medium => ReasoningEffort::Medium,
        Effort::High => ReasoningEffort::High,
        Effort::Xhigh => ReasoningEffort::XHigh,
        Effort::Max => ReasoningEffort::Max,
    }
}

/// Translates a neutral request into a `genai` request.
fn chat_request(request: ModelRequest<'_>) -> ChatRequest {
    let mut messages = to_messages(request.entries);
    // A marker on the newest message caches the whole history, not just the system prompt.
    if let Some(last) = messages.last_mut() {
        last.options = Some(CacheControl::Ephemeral.into());
    }
    let tools: Vec<Tool> = request.tools.iter().map(to_tool).collect();
    ChatRequest::new(messages)
        .with_system(request.system)
        .with_tools(tools)
}

/// Translates a neutral tool definition into a `genai` one.
fn to_tool(spec: &ToolSpec) -> Tool {
    Tool::new(spec.name.as_str())
        .with_description(spec.description.as_str())
        .with_schema(spec.input_schema.clone())
        .with_eager_input_streaming(true)
}

/// Returns whether `entry` is a tool result.
fn is_tool_result(entry: &Entry) -> bool {
    matches!(entry, Entry::ToolResult { .. })
}

/// Returns whether `entry` only marks how a Turn stopped, which the model never sees.
fn is_marker(entry: &Entry) -> bool {
    matches!(entry, Entry::TurnCancelled | Entry::TurnParked)
}

/// Translates a Session into messages.
///
/// Results of one response share a message, which keeps the model making parallel calls. They
/// share it even when the Turn parked between them, as it does when a Rebuild puts off a
/// question.
fn to_messages(entries: &[Entry]) -> Vec<ChatMessage> {
    let seen: Vec<&Entry> = entries.iter().filter(|entry| !is_marker(entry)).collect();
    seen.chunk_by(|a, b| is_tool_result(a) && is_tool_result(b))
        .filter_map(to_message)
        .collect()
}

/// Translates a chunk of adjacent entries into one message, if it produces one.
fn to_message(chunk: &[&Entry]) -> Option<ChatMessage> {
    match chunk.first()? {
        Entry::User { text } => Some(ChatMessage::user(text.as_str())),
        Entry::Assistant(assistant) => assistant_message(assistant),
        Entry::ToolResult { .. } => Some(tool_message(chunk)),
        Entry::TurnCancelled | Entry::TurnParked => None,
    }
}

/// Combines adjacent tool results into a single message.
fn tool_message(results: &[&Entry]) -> ChatMessage {
    let responses: Vec<ToolResponse> = results.iter().copied().filter_map(tool_response).collect();
    responses.into()
}

/// Translates a tool result entry, returning nothing for any other entry.
fn tool_response(entry: &Entry) -> Option<ToolResponse> {
    let Entry::ToolResult {
        call_id,
        output,
        is_error,
    } = entry
    else {
        return None;
    };
    // genai has no error flag for tool results, so failures must say so in the text.
    let content = if *is_error {
        format!("Error: {output}")
    } else {
        output.clone()
    };
    Some(ToolResponse::new(call_id, content))
}

/// Translates an assistant entry, returning nothing if it has no content to send.
fn assistant_message(assistant: &AssistantEntry) -> Option<ChatMessage> {
    let content = replayed_content(assistant).unwrap_or_else(|| neutral_content(assistant));
    (!content.is_empty()).then(|| ChatMessage::assistant(content))
}

/// Returns the original response to send back, if this Provider produced it.
///
/// Claude needs its thinking blocks back unchanged, and ignores any another model wrote.
fn replayed_content(assistant: &AssistantEntry) -> Option<MessageContent> {
    let payload = assistant.payload.as_ref()?;
    if payload.provider != PROVIDER {
        return None;
    }
    serde_json::from_value(payload.data.clone()).ok()
}

/// Returns the content for a response with no usable Provider Payload.
fn neutral_content(assistant: &AssistantEntry) -> MessageContent {
    let text = (!assistant.text.is_empty()).then(|| ContentPart::Text(assistant.text.clone()));
    let calls = assistant.tool_calls.iter().map(|call| {
        ContentPart::ToolCall(GenaiToolCall {
            call_id: call.id.clone(),
            fn_name: call.name.clone(),
            fn_arguments: call.input.clone(),
            thought_signatures: None,
        })
    });
    MessageContent::from_parts(text.into_iter().chain(calls).collect::<Vec<_>>())
}

/// Converts a `genai` stream event to a neutral one, if it is one worth reporting.
fn stream_event(event: ChatStreamEvent) -> Option<StreamEvent> {
    match event {
        ChatStreamEvent::Chunk(chunk) => Some(StreamEvent::Text(chunk.content)),
        ChatStreamEvent::ReasoningChunk(chunk) => Some(StreamEvent::Reasoning(chunk.content)),
        _ => None,
    }
}

/// Reads a response stream to its end, forwarding progress to `on_event`.
async fn consume(
    mut stream: impl Stream<Item = genai::Result<ChatStreamEvent>> + Unpin + Send,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamEnd, ProviderError> {
    while let Some(event) = stream.next().await {
        let event = event.map_err(provider_error)?;
        if let ChatStreamEvent::End(end) = event {
            return Ok(end);
        }
        if let Some(event) = stream_event(event) {
            on_event(event);
        }
    }
    Err(ProviderError("stream ended early".to_owned()))
}

/// Converts `genai`'s stop reason to a neutral one, inferring it from the calls if absent.
fn stop_reason(stop: Option<&genai::chat::StopReason>, has_calls: bool) -> StopReason {
    use genai::chat::StopReason as Genai;
    match stop {
        Some(Genai::MaxTokens(_)) => StopReason::MaxTokens,
        Some(Genai::ContentFilter(_)) => StopReason::Refused,
        Some(Genai::ToolCall(_)) => StopReason::ToolUse,
        _ if has_calls => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

/// Converts a reported token count, treating a missing or negative value as zero.
fn token_count(tokens: Option<i32>) -> u64 {
    tokens.and_then(|n| u64::try_from(n).ok()).unwrap_or(0)
}

/// Converts a finished stream into an assistant entry with its Provider Payload.
fn to_entry(end: StreamEnd, model: &str) -> Result<AssistantEntry, ProviderError> {
    let content = end.captured_content.unwrap_or_default();
    let tool_calls: Vec<ToolCall> = content
        .tool_calls()
        .into_iter()
        .map(|call| ToolCall {
            id: call.call_id.clone(),
            name: call.fn_name.clone(),
            input: call.fn_arguments.clone(),
        })
        .collect();
    let usage = end.captured_usage.unwrap_or_default();
    Ok(AssistantEntry {
        text: content.joined_texts().unwrap_or_default(),
        stop: stop_reason(end.captured_stop_reason.as_ref(), !tool_calls.is_empty()),
        tool_calls,
        usage: Usage {
            input_tokens: token_count(usage.prompt_tokens),
            output_tokens: token_count(usage.completion_tokens),
        },
        payload: Some(ProviderPayload {
            provider: PROVIDER.to_owned(),
            model: model.to_owned(),
            data: serde_json::to_value(&content).map_err(provider_error)?,
        }),
    })
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn respond(
        &self,
        request: ModelRequest<'_>,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<AssistantEntry, ProviderError> {
        let chat = chat_request(request);
        let response = self
            .client
            .exec_chat_stream(request.model, chat, Some(&self.options))
            .await
            .map_err(provider_error)?;
        to_entry(consume(response.stream, on_event).await?, request.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai::chat::{StopReason as Genai, StreamChunk};
    use serde_json::Value;
    use std::fmt::Write as _;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn assistant(text: &str, calls: Vec<ToolCall>, payload: Option<Value>) -> AssistantEntry {
        AssistantEntry {
            text: text.into(),
            tool_calls: calls,
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: payload.map(|data| ProviderPayload {
                provider: PROVIDER.into(),
                model: "m".into(),
                data,
            }),
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "read".into(),
            input: json!({"path": "a"}),
        }
    }

    fn tool_result(id: &str, output: &str, is_error: bool) -> Entry {
        Entry::ToolResult {
            call_id: id.into(),
            output: output.into(),
            is_error,
        }
    }

    fn signed_content() -> MessageContent {
        MessageContent::from_parts(vec![
            ContentPart::ThoughtSignature("sig".into()),
            ContentPart::ReasoningContent("hmm".into()),
            ContentPart::Text("hi".into()),
        ])
    }

    #[test]
    fn claude_payloads_replay_with_their_thinking_intact() {
        let data = serde_json::to_value(signed_content()).unwrap();
        let message = assistant_message(&assistant("ignored", vec![], Some(data))).unwrap();
        assert_eq!(message.content.thought_signatures(), ["sig"]);
        assert_eq!(message.content.reasoning_contents(), ["hmm"]);
        assert_eq!(message.content.texts(), ["hi"]);
    }

    #[test]
    fn other_providers_payloads_fall_back_to_the_neutral_form() {
        let data = serde_json::to_value(signed_content()).unwrap();
        let mut entry = assistant("neutral", vec![call()], Some(data));
        entry.payload.as_mut().unwrap().provider = "other".into();
        let message = assistant_message(&entry).unwrap();
        assert_eq!(message.content.texts(), ["neutral"]);
        assert_eq!(message.content.tool_calls().len(), 1);
        assert_eq!(message.content.thought_signatures(), Vec::<&str>::new());
    }

    #[test]
    fn empty_responses_are_left_out_because_the_api_rejects_them() {
        assert!(assistant_message(&assistant("", vec![], None)).is_none());
    }

    #[test]
    fn adjacent_tool_results_share_one_message_and_errors_are_labelled() {
        let entries = [
            Entry::User { text: "hi".into() },
            Entry::Assistant(assistant("", vec![call()], None)),
            tool_result("c1", "ok", false),
            tool_result("c2", "bad", true),
            Entry::TurnCancelled,
            Entry::User {
                text: "again".into(),
            },
        ];
        let messages = to_messages(&entries);
        assert_eq!(messages.len(), 4);
        let responses = messages[2].content.tool_responses();
        assert_eq!(responses[0].content, "ok");
        assert_eq!(responses[1].content, "Error: bad");
        assert!(tool_response(&Entry::TurnCancelled).is_none());
        assert!(tool_response(&Entry::TurnParked).is_none());
    }

    #[test]
    fn results_either_side_of_a_parked_turn_still_share_one_message() {
        let entries = [
            Entry::User { text: "hi".into() },
            Entry::Assistant(assistant("", vec![call()], None)),
            tool_result("c1", "ok", false),
            Entry::TurnParked,
            tool_result("c2", "later", false),
        ];
        let messages = to_messages(&entries);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].content.tool_responses().len(), 2);
    }

    #[test]
    fn stop_reasons_map_to_neutral_ones() {
        let cases = [
            (
                Some(Genai::MaxTokens("x".into())),
                false,
                StopReason::MaxTokens,
            ),
            (
                Some(Genai::ContentFilter("x".into())),
                false,
                StopReason::Refused,
            ),
            (
                Some(Genai::ToolCall("x".into())),
                false,
                StopReason::ToolUse,
            ),
            (None, true, StopReason::ToolUse),
            (
                Some(Genai::Completed("x".into())),
                false,
                StopReason::EndTurn,
            ),
        ];
        for (stop, has_calls, expected) in cases {
            assert_eq!(stop_reason(stop.as_ref(), has_calls), expected);
        }
    }

    #[test]
    fn every_effort_level_maps_across() {
        let levels = [
            (Effort::Low, ReasoningEffort::Low),
            (Effort::Medium, ReasoningEffort::Medium),
            (Effort::High, ReasoningEffort::High),
            (Effort::Xhigh, ReasoningEffort::XHigh),
            (Effort::Max, ReasoningEffort::Max),
        ];
        for (effort, expected) in levels {
            assert_eq!(
                format!("{:?}", reasoning_effort(effort)),
                format!("{expected:?}")
            );
        }
    }

    #[test]
    fn invalid_token_counts_become_zero() {
        assert_eq!(token_count(Some(5)), 5);
        assert_eq!(token_count(Some(-1)), 0);
        assert_eq!(token_count(None), 0);
    }

    #[tokio::test]
    async fn stream_events_are_forwarded_until_the_end() {
        let events = vec![
            Ok(ChatStreamEvent::Start),
            Ok(ChatStreamEvent::ReasoningChunk(StreamChunk {
                content: "think".into(),
            })),
            Ok(ChatStreamEvent::Chunk(StreamChunk {
                content: "text".into(),
            })),
            Ok(ChatStreamEvent::End(StreamEnd::default())),
        ];
        let mut seen = Vec::new();
        consume(futures::stream::iter(events), &mut |e| seen.push(e))
            .await
            .unwrap();
        assert_eq!(
            seen,
            [
                StreamEvent::Reasoning("think".into()),
                StreamEvent::Text("text".into())
            ]
        );
    }

    #[tokio::test]
    async fn a_stream_with_no_end_is_an_error() {
        let result = consume(futures::stream::iter(vec![]), &mut |_| {}).await;
        assert_eq!(result.unwrap_err().to_string(), "stream ended early");
    }

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

    /// Answers one HTTP request with `status` and `body`, and returns what it received.
    fn serve_once(status: &str, body: String) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let status = status.to_owned();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_request(&mut socket);
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).unwrap();
            socket.write_all(body.as_bytes()).unwrap();
            request
        });
        (url, server)
    }

    fn read_request(socket: &mut impl Read) -> String {
        let mut received = Vec::new();
        let mut buffer = [0; 8192];
        while !request_complete(&received) {
            let count = socket.read(&mut buffer).unwrap();
            received.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(received).unwrap()
    }

    fn request_complete(received: &[u8]) -> bool {
        let text = String::from_utf8_lossy(received);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let length = head
            .lines()
            .find_map(|line| {
                line.to_lowercase()
                    .strip_prefix("content-length: ")?
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        body.len() >= length
    }

    fn provider(url: String) -> AnthropicProvider {
        AnthropicProvider::new("test-key".into(), Some(url), Effort::High).unwrap()
    }

    fn request_with<'a>(entries: &'a [Entry], tools: &'a [ToolSpec]) -> ModelRequest<'a> {
        ModelRequest {
            model: "claude-opus-5-5",
            system: "be brief",
            entries,
            tools,
        }
    }

    #[tokio::test]
    async fn a_streamed_tool_call_becomes_a_neutral_entry_with_a_replayable_payload() {
        let (url, server) = serve_once("200 OK", tool_use_stream());
        let entries = [Entry::User { text: "hi".into() }];
        let tools = [ToolSpec {
            name: "read".into(),
            description: "Read".into(),
            input_schema: json!({"type": "object"}),
        }];
        let mut text = String::new();

        let entry = provider(url)
            .respond(request_with(&entries, &tools), &mut |event| {
                if let StreamEvent::Text(chunk) = event {
                    text.push_str(&chunk);
                }
            })
            .await
            .unwrap();

        assert_eq!(text, "Reading");
        assert_eq!(entry.text, "Reading");
        assert_eq!(entry.tool_calls, [call()]);
        assert_eq!(entry.stop, StopReason::ToolUse);
        assert_eq!(entry.usage.output_tokens, 7);
        let replayed = assistant_message(&entry).unwrap();
        assert_eq!(replayed.content.tool_calls().len(), 1);

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
            .respond(request_with(&entries, &[]), &mut |_| {})
            .await
            .unwrap_err();

        assert!(error.to_string().contains("401"), "{error}");
        server.join().unwrap();
    }
}
