//! Anthropic Messages API 어댑터.
//!
//! 공식 Rust SDK 가 없으므로 `reqwest` 로 `POST /v1/messages` 를 직접 호출하고,
//! `eventsource-stream` 으로 SSE 응답을 파싱한다. OpenAI 어댑터처럼 변환은 **순수
//! 함수**([`to_wire`], [`AnthropicDecoder`])로 두고 HTTP/SSE 부작용만 [`AnthropicProvider::stream`]
//! 에 둔다(functional core / imperative shell, CODING_RULES §4.1).
//!
//! 와이어 사양(요지):
//! - 엔드포인트: `POST {base_url}/v1/messages`, 토큰 카운트는 `/v1/messages/count_tokens`.
//!   aiproxy 경유는 `base_url` 에 `/anthropic` 까지 넣어 `{base_url}/v1/messages` 가
//!   `.../anthropic/v1/messages` 가 되게 한다.
//! - 헤더: 직결은 `x-api-key`, 게이트웨이(aiproxy) 경유는 `Authorization: Bearer`
//!   ([`AuthStyle`]) + `anthropic-version: 2023-06-01`, `content-type: application/json`
//! - 본문(요청): `{ model, max_tokens, system, messages, tools, stream: true,
//!     thinking: {type:"adaptive"}, output_config: {effort} }`
//! - 와이어 차이(어댑터가 흡수): ① `system` 이 **최상위 필드**(OpenAI 는 messages[0])
//!   ② 도구 입력이 **객체** `input`(OpenAI 는 문자열 arguments) ③ tool_result 가 user
//!   메시지의 content 블록 ④ 추론은 `thinking`(OpenAI 는 reasoning_effort).
//! - 스트림 이벤트 → 코어 이벤트 매핑:
//!
//!   ```text
//!   message_start                         → MessageStart (+ input_tokens 적립)
//!   content_block_start(tool_use)         → ToolUseStart
//!   content_block_delta(text_delta)       → TextDelta
//!   content_block_delta(thinking_delta)   → ThinkingDelta
//!   content_block_delta(input_json_delta) → ToolUseInputDelta
//!   content_block_stop                    → ContentBlockStop
//!   message_delta(stop_reason,usage)      → (stop_reason·output_tokens 적립)
//!   message_stop                          → MessageStop
//!   ```

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use async_trait::async_trait;
use eventsource_stream::{Event as SseEvent, Eventsource};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};

use scv_core::message::{ContentBlock, Message, Role, StopReason, StreamEvent, Usage};
use scv_core::provider::{
    CompletionRequest, Effort, EventStream, ModelInfo, Provider, ThinkingMode, ToolSchema,
};
use scv_core::{Error, Result};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// 인증 헤더 방식. 직결 Anthropic 은 `x-api-key`, aiproxy 등 게이트웨이는
/// `Authorization: Bearer`(게이트웨이가 실제 Anthropic 키를 주입). 와이어 본문은 동일하다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `x-api-key: <key>` — api.anthropic.com 직결.
    ApiKey,
    /// `Authorization: Bearer <token>` — aiproxy 등 게이트웨이 경유.
    Bearer,
}

impl AuthStyle {
    /// 설정 문자열 → [`AuthStyle`]. `"bearer"`(대소문자 무시)만 Bearer, 생략(None)·그 외는
    /// 기본 `ApiKey`(직결 호환).
    pub fn from_config(s: Option<&str>) -> Self {
        match s {
            Some(v) if v.eq_ignore_ascii_case("bearer") => Self::Bearer,
            _ => Self::ApiKey,
        }
    }
}

#[derive(Debug)]
pub struct AnthropicProvider {
    model: String,
    api_key: String,
    base_url: String,
    auth: AuthStyle,
    http: reqwest::Client,
    models: Vec<ModelInfo>,
}

impl AnthropicProvider {
    pub fn new(model: String, api_key: String, base_url: Option<String>, auth: AuthStyle) -> Self {
        Self {
            model,
            api_key,
            // 끝 슬래시를 제거해 `{base_url}/v1/messages` 가 이중 슬래시로 깨지지 않게 한다.
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            auth,
            http: reqwest::Client::new(),
            // aiproxy 경유로 제한한 모델군(Sonnet/Haiku). 직결로 다른 모델을 쓰려면 config 의
            // `model` 로 지정하면 되고, 이 목록은 능력 조회/검증용 메타데이터다.
            // 주의: effort 파라미터는 Haiku 4.5 에서 400 → to_wire 가 effort 를 보내는 현재 구성에선
            // Haiku 사용 시 thinking/effort 처리에 후속 보정이 필요(ROADMAP 5b).
            models: vec![
                ModelInfo {
                    id: "claude-sonnet-4-6".into(),
                    context_window: 1_000_000,
                    max_output_tokens: 64_000,
                    supports_thinking: true,
                },
                ModelInfo {
                    id: "claude-haiku-4-5".into(),
                    context_window: 200_000,
                    max_output_tokens: 64_000,
                    supports_thinking: true,
                },
            ],
        }
    }

    /// 요청에 인증 헤더를 붙인다([`AuthStyle`] 에 따라 `x-api-key` 또는 `Authorization: Bearer`).
    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.auth {
            AuthStyle::ApiKey => req.header("x-api-key", &self.api_key),
            AuthStyle::Bearer => req.bearer_auth(&self.api_key),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream> {
        let wire = to_wire(&request, true);
        let req = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&wire);
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("Anthropic HTTP {status}: {body}")));
        }

        let sse: SseStream = Box::pin(resp.bytes_stream().eventsource());
        let stream = futures::stream::unfold(StreamState::new(sse), drive_stream);
        Ok(Box::pin(stream))
    }

    async fn count_tokens(
        &self,
        system: Option<&str>,
        messages: &[Message],
        tools: &[ToolSchema],
    ) -> Result<u64> {
        // Anthropic 은 정확한 토큰 수를 서버에서 계산해 준다(POST /v1/messages/count_tokens).
        let mut body = json!({
            "model": self.model,
            "messages": messages_to_wire(messages),
        });
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if !tools.is_empty() {
            body["tools"] = tools_to_wire(tools);
        }
        let req = self
            .http
            .post(format!("{}/v1/messages/count_tokens", self.base_url))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("count_tokens request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!(
                "Anthropic count_tokens HTTP {status}: {body}"
            )));
        }
        let parsed: Value = resp
            .json()
            .await
            .map_err(|e| Error::Provider(format!("count_tokens parse: {e}")))?;
        Ok(parsed["input_tokens"].as_u64().unwrap_or(0))
    }
}

// ───────────────────────── 요청: 코어 → Anthropic 와이어 ─────────────────────────

/// 코어 [`CompletionRequest`] 를 Anthropic Messages 요청 본문으로 변환한다 — **순수**.
/// `stream` 이면 `stream:true` 를 넣는다(count_tokens 경로는 false).
fn to_wire(req: &CompletionRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages_to_wire(&req.messages),
    });
    if stream {
        body["stream"] = json!(true);
    }
    // 프롬프트 캐싱(ROADMAP 5b): system 을 텍스트 블록 배열로 보내고 끝에 cache_control 을 단다.
    // 렌더 순서가 tools → system → messages 이므로 system 끝의 브레이크포인트 하나가
    // **tools + system** 안정 prefix 를 함께 캐시한다(턴마다 재전송되는 고정 prefix → 2번째
    // 호출부터 cache_read ~0.1x). prefix 가 1바이트라도 바뀌면 무효화되므로 system·tool 순서를
    // 결정적으로 유지한다(BTreeMap 직렬화). 최소 캐시 prefix 미만이면 조용히 캐시 안 됨(에러 아님).
    if let Some(system) = &req.system {
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if !req.tools.is_empty() {
        body["tools"] = tools_to_wire(&req.tools);
    }
    // 추론: 4.6+ 는 thinking:{type:"adaptive"} + output_config.effort(budget_tokens 미사용).
    if req.thinking != ThinkingMode::Disabled {
        body["thinking"] = json!({ "type": "adaptive" });
        if let Some(effort) = req.effort {
            body["output_config"] = json!({ "effort": map_effort(effort) });
        }
    }
    body
}

/// 코어 Effort → Anthropic `output_config.effort`(low|medium|high). xhigh/max 는 high 로 클램프.
fn map_effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::XHigh | Effort::Max => "high",
    }
}

/// 코어 메시지 → Anthropic `messages[]`(role + content 블록 배열). system 은 최상위라 제외.
fn messages_to_wire(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .filter_map(|m| {
                let role = match m.role {
                    Role::Assistant => "assistant",
                    // System 역할 메시지는 보통 최상위 system 으로 가지만, 들어오면 user 로.
                    Role::User | Role::System => "user",
                };
                let content: Vec<Value> = m.content.iter().filter_map(block_to_wire).collect();
                if content.is_empty() {
                    return None;
                }
                Some(json!({ "role": role, "content": content }))
            })
            .collect(),
    )
}

/// 코어 콘텐츠 블록 → Anthropic content 블록. `Thinking` 은 서명 검증 이슈를 피하려고
/// 아웃바운드에서 생략한다(되돌려보낼 때 유효 signature 가 없으면 400 가능).
fn block_to_wire(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use", "id": id, "name": name, "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Some(json!({
            "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error,
        })),
        ContentBlock::Thinking { .. } => None,
    }
}

/// 코어 [`ToolSchema`] → Anthropic tool 정의(이름/설명/input_schema 그대로).
fn tools_to_wire(tools: &[ToolSchema]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect(),
    )
}

// ───────────────────────── 응답: Anthropic SSE → 코어 StreamEvent ─────────────────────────

type SseStream = Pin<
    Box<
        dyn Stream<
                Item = std::result::Result<
                    SseEvent,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            > + Send,
    >,
>;

struct StreamState {
    sse: SseStream,
    decoder: AnthropicDecoder,
    pending: VecDeque<StreamEvent>,
    done: bool,
    stop_sent: bool,
    errored: bool,
}

impl StreamState {
    fn new(sse: SseStream) -> Self {
        Self {
            sse,
            decoder: AnthropicDecoder::new(),
            pending: VecDeque::new(),
            done: false,
            stop_sent: false,
            errored: false,
        }
    }
}

/// `futures::stream::unfold` 한 스텝. Anthropic 은 `message_stop` 이벤트로 끝나므로 그때
/// `MessageStop` 을 낸다. 만약 그 이벤트 없이 스트림이 닫히면(이상 종료) 마지막에 한 번 합성.
async fn drive_stream(mut st: StreamState) -> Option<(Result<StreamEvent>, StreamState)> {
    loop {
        if let Some(ev) = st.pending.pop_front() {
            if matches!(ev, StreamEvent::MessageStop { .. }) {
                st.stop_sent = true;
            }
            return Some((Ok(ev), st));
        }
        if st.done {
            if !st.stop_sent && !st.errored {
                st.stop_sent = true;
                return Some((Ok(st.decoder.message_stop()), st));
            }
            return None;
        }
        match st.sse.next().await {
            None => st.done = true,
            Some(Err(e)) => {
                st.done = true;
                st.errored = true;
                return Some((Err(Error::Provider(format!("stream error: {e}"))), st));
            }
            Some(Ok(event)) => {
                if !event.data.trim().is_empty() {
                    match serde_json::from_str::<Value>(&event.data) {
                        Ok(value) => st.pending.extend(st.decoder.decode(&value)),
                        Err(e) => {
                            st.done = true;
                            st.errored = true;
                            return Some((
                                Err(Error::Provider(format!("malformed SSE event: {e}"))),
                                st,
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// Anthropic SSE 이벤트(JSON)를 코어 [`StreamEvent`] 로 누적 변환하는 디코더.
///
/// 상태가 필요한 이유: ① tool_use 의 `id`/`name` 은 `content_block_start` 에만 있고
/// 이후 `input_json_delta` 엔 `index` 만 오므로 index→id 매핑을 든다 ② `stop_reason`·
/// `usage` 는 `message_start`/`message_delta` 에 나눠 와서 마지막 `MessageStop` 에 합친다.
struct AnthropicDecoder {
    tool_ids: HashMap<u64, String>,
    stop: StopReason,
    usage: Usage,
}

impl AnthropicDecoder {
    fn new() -> Self {
        Self {
            tool_ids: HashMap::new(),
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        }
    }

    fn message_stop(&self) -> StreamEvent {
        StreamEvent::MessageStop {
            stop_reason: self.stop,
            usage: self.usage,
        }
    }

    fn decode(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let model = v["message"]["model"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let usage = &v["message"]["usage"];
                if let Some(n) = usage["input_tokens"].as_u64() {
                    self.usage.input_tokens = n;
                }
                // 프롬프트 캐싱 측정(ROADMAP 5b): 캐시 기록(write, ~1.25x)·적중(read, ~0.1x) 토큰.
                if let Some(n) = usage["cache_creation_input_tokens"].as_u64() {
                    self.usage.cache_write_tokens = n;
                }
                if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
                    self.usage.cache_read_tokens = n;
                }
                out.push(StreamEvent::MessageStart { model });
            }
            "content_block_start" => {
                let index = v["index"].as_u64().unwrap_or(0);
                let block = &v["content_block"];
                if block["type"].as_str() == Some("tool_use") {
                    let id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = block["name"].as_str().unwrap_or_default().to_string();
                    self.tool_ids.insert(index, id.clone());
                    out.push(StreamEvent::ToolUseStart { id, name });
                }
            }
            "content_block_delta" => {
                let index = v["index"].as_u64().unwrap_or(0);
                let delta = &v["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str() {
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str() {
                            out.push(StreamEvent::ThinkingDelta(t.to_string()));
                        }
                    }
                    "input_json_delta" => {
                        if let Some(p) = delta["partial_json"].as_str() {
                            let id = self.tool_ids.get(&index).cloned().unwrap_or_default();
                            out.push(StreamEvent::ToolUseInputDelta {
                                id,
                                partial_json: p.to_string(),
                            });
                        }
                    }
                    // signature_delta 등은 코어 이벤트로 표현하지 않으므로 무시.
                    _ => {}
                }
            }
            "content_block_stop" => out.push(StreamEvent::ContentBlockStop),
            "message_delta" => {
                if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                    self.stop = map_stop_reason(sr);
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = n;
                }
            }
            "message_stop" => out.push(self.message_stop()),
            // ping 등은 무시.
            _ => {}
        }
        out
    }
}

/// Anthropic stop_reason → 코어 [`StopReason`].
fn map_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "stop_sequence" => StopReason::StopSequence,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(messages: Vec<Message>, tools: Vec<ToolSchema>) -> CompletionRequest {
        CompletionRequest {
            model: "claude-opus-4-8".into(),
            system: Some("be terse".into()),
            messages,
            tools,
            max_tokens: 1024,
            effort: Some(Effort::Max),
            thinking: ThinkingMode::Adaptive,
        }
    }

    #[test]
    fn wire_puts_system_top_level_and_maps_blocks() {
        let assistant = Message::assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: json!({ "path": "a.rs" }),
        }]);
        let wire = to_wire(&req(vec![Message::user("hi"), assistant], vec![]), true);
        assert_eq!(wire["model"], "claude-opus-4-8");
        // system 은 cache_control 을 단 텍스트 블록 배열(프롬프트 캐싱, 5b).
        assert_eq!(wire["system"][0]["type"], "text");
        assert_eq!(wire["system"][0]["text"], "be terse");
        assert_eq!(wire["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["thinking"]["type"], "adaptive");
        // Max effort → high 로 클램프.
        assert_eq!(wire["output_config"]["effort"], "high");
        let msgs = wire["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["type"], "text");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["input"]["path"], "a.rs");
    }

    #[test]
    fn wire_disabled_thinking_omits_thinking() {
        let mut r = req(vec![Message::user("hi")], vec![]);
        r.thinking = ThinkingMode::Disabled;
        let wire = to_wire(&r, false);
        assert!(wire.get("thinking").is_none());
        assert!(wire.get("stream").is_none());
    }

    #[test]
    fn tool_result_block_maps_to_anthropic_shape() {
        let m = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "out".into(),
                is_error: false,
            }],
        };
        let wire = messages_to_wire(&[m]);
        assert_eq!(wire[0]["content"][0]["type"], "tool_result");
        assert_eq!(wire[0]["content"][0]["tool_use_id"], "t1");
    }

    fn decode_all(events: &[Value]) -> Vec<StreamEvent> {
        let mut d = AnthropicDecoder::new();
        let mut out = Vec::new();
        for e in events {
            out.extend(d.decode(e));
        }
        out
    }

    #[test]
    fn decodes_cache_tokens_from_message_start() {
        // 프롬프트 캐싱(5b): message_start.usage 의 cache_creation/read → Usage 의 write/read.
        let events = vec![
            json!({ "type": "message_start", "message": { "model": "claude-sonnet-4-6", "usage": {
                "input_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 2048
            } } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 7 } }),
            json!({ "type": "message_stop" }),
        ];
        match decode_all(&events).last().unwrap() {
            StreamEvent::MessageStop { usage, .. } => {
                assert_eq!(usage.cache_read_tokens, 2048);
                assert_eq!(usage.cache_write_tokens, 0);
                assert_eq!(usage.input_tokens, 10);
            }
            other => panic!("expected MessageStop, got {other:?}"),
        }
    }

    #[test]
    fn decodes_text_stream_with_usage_and_stop() {
        let events = vec![
            json!({ "type": "message_start", "message": { "model": "claude-opus-4-8", "usage": { "input_tokens": 42 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hel" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "lo" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
            json!({ "type": "message_stop" }),
        ];
        let out = decode_all(&events);
        assert!(
            matches!(&out[0], StreamEvent::MessageStart { model } if model == "claude-opus-4-8")
        );
        let text: String = out
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        match out.last().unwrap() {
            StreamEvent::MessageStop { stop_reason, usage } => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 42);
                assert_eq!(usage.output_tokens, 5);
            }
            other => panic!("expected MessageStop, got {other:?}"),
        }
    }

    #[test]
    fn decodes_tool_use_with_id_tracked_across_input_deltas() {
        let events = vec![
            json!({ "type": "message_start", "message": { "model": "m", "usage": { "input_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "call_1", "name": "grep" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": "\"x\"}" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 3 } }),
            json!({ "type": "message_stop" }),
        ];
        let out = decode_all(&events);
        assert!(
            matches!(&out[1], StreamEvent::ToolUseStart { id, name } if id == "call_1" && name == "grep")
        );
        // input_json_delta 가 같은 index 의 tool id 를 달고 나온다.
        let ids: Vec<&str> = out
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolUseInputDelta { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["call_1", "call_1"]);
        assert!(matches!(
            out.last().unwrap(),
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("pause_turn"), StopReason::PauseTurn);
        assert_eq!(map_stop_reason("stop_sequence"), StopReason::StopSequence);
        assert_eq!(map_stop_reason("refusal"), StopReason::Refusal);
        assert_eq!(map_stop_reason("???"), StopReason::EndTurn);
    }

    #[test]
    fn auth_style_from_config_defaults_to_api_key() {
        // 생략·미지의 값 → ApiKey(직결 호환). "bearer"(대소문자 무시)만 Bearer.
        assert_eq!(AuthStyle::from_config(None), AuthStyle::ApiKey);
        assert_eq!(AuthStyle::from_config(Some("x-api-key")), AuthStyle::ApiKey);
        assert_eq!(AuthStyle::from_config(Some("nonsense")), AuthStyle::ApiKey);
        assert_eq!(AuthStyle::from_config(Some("bearer")), AuthStyle::Bearer);
        assert_eq!(AuthStyle::from_config(Some("Bearer")), AuthStyle::Bearer);
    }

    #[test]
    fn new_lists_sonnet_and_haiku_and_trims_base_url() {
        let p = AnthropicProvider::new(
            "claude-sonnet-4-6".into(),
            "k".into(),
            Some("https://aiproxy-api.example.com/anthropic/".into()),
            AuthStyle::Bearer,
        );
        // 끝 슬래시 정규화 → `{base_url}/v1/messages` 가 이중 슬래시로 깨지지 않는다.
        assert!(p.base_url.ends_with("/anthropic"));
        let ids: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claude-sonnet-4-6", "claude-haiku-4-5"]);
    }
}
