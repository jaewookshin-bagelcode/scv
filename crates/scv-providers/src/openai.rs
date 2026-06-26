//! OpenAI Chat Completions 어댑터(기본 프로바이더).
//!
//! 코어의 프로바이더 중립 타입([`scv_core::message`])을 OpenAI 와이어 포맷으로
//! 양방향 변환한다. 변환 로직은 **순수 함수**([`to_wire`], [`ChunkDecoder`])로 두고,
//! HTTP/SSE 같은 부작용만 [`OpenAiProvider::stream`] 에 둔다(functional core / imperative
//! shell — CODING_RULES §4.1). 덕분에 변환은 네트워크 없이 단위 테스트된다.
//!
//! - 엔드포인트: `POST {base_url}/chat/completions`
//! - 헤더: `Authorization: Bearer {api_key}`
//! - 와이어 차이(어댑터가 흡수): ① tool 입력이 **문자열** `arguments`(객체 아님)
//!   ② 도구 결과가 별도 `role:"tool"` 메시지 ③ 시스템 프롬프트가 `messages[0]`
//!   ④ 추론 깊이는 OpenAI 자체 파라미터 `reasoning_effort`(Anthropic 의 `thinking` 미전송).
//! - SSE delta(`choices[].delta`) → 코어 [`StreamEvent`] 매핑은 [`ChunkDecoder`].

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use async_trait::async_trait;
use eventsource_stream::{Event as SseEvent, Eventsource};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use scv_core::message::{ContentBlock, Message, Role, StopReason, StreamEvent, Usage};
use scv_core::provider::{
    CompletionRequest, Effort, EventStream, ModelInfo, Provider, ThinkingMode, ToolSchema,
};
use scv_core::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug)]
pub struct OpenAiProvider {
    /// 설정 `kind` 가 그대로 [`Provider::id`] 로 보고된다("openai"·"openai-compat"·"ollama" 등).
    /// Ollama 등 OpenAI-호환 백엔드는 같은 어댑터를 재사용하되 id 로 자신을 드러낸다.
    id: String,
    api_key: String,
    base_url: String,
    http: reqwest::Client,
    models: Vec<ModelInfo>,
    /// OpenAI-호환(비표준) 백엔드 호환 모드(Ollama·로컬 LLM·구형 게이트웨이). 추론 모델
    /// 전용 파라미터를 거부하는 백엔드용으로 [`to_wire`] 직렬화를 바꾼다.
    compat: bool,
}

impl OpenAiProvider {
    /// `id` 는 설정 `kind`(예 `ollama`)로 [`Provider::id`] 에 보고된다. `model` 은
    /// [`Provider::models`] 의 기준 모델 id. 실제 요청 모델은 매 호출
    /// [`CompletionRequest::model`] 로 주입된다. `compat=true` 면 OpenAI-호환(비표준)
    /// 백엔드용 와이어 호환 모드로 보낸다([`to_wire`]).
    pub fn new(
        id: impl Into<String>,
        model: String,
        api_key: String,
        base_url: Option<String>,
        compat: bool,
    ) -> Self {
        Self {
            id: id.into(),
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            http: reqwest::Client::new(),
            models: vec![ModelInfo {
                id: model,
                context_window: 400_000,
                max_output_tokens: 128_000,
                supports_thinking: true,
            }],
            compat,
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream> {
        let wire = to_wire(&request, self.compat);
        let url = format!("{}/chat/completions", self.base_url);
        let mut req = self.http.post(&url).json(&wire);
        // 키가 비어 있으면(무인증 백엔드, 예: 로컬 Ollama) Authorization 헤더를 생략한다 —
        // 빈 `Bearer ` 를 까다롭게 거르는 게이트웨이를 피한다(ROADMAP 4e).
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Provider(format!("request failed: {e}")))?;

        // 에러가 HTTP 200 이 아닌 코드로 올 때는 본문에 사유가 담겨 있다(CODING_RULES §9).
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("OpenAI HTTP {status}: {body}")));
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
        // OpenAI 엔 count 엔드포인트가 없어 **로컬 토크나이저(o200k_base)로 추정**한다.
        // 평상시 compaction 신호로는 직전 응답의 usage 를 우선 쓰고, 이건 사전 점검 보조.
        let text = render_for_count(system, messages, tools);
        let bpe = tiktoken_rs::o200k_base()
            .map_err(|e| Error::Provider(format!("tokenizer init failed: {e}")))?;
        let content = bpe.encode_ordinary(&text).len() as u64;
        // 메시지마다 role/구분자 등 포맷 오버헤드를 대략 더한다(추정값이라 정확할 필요는 없다).
        Ok(content + messages.len() as u64 * 4)
    }
}

/// count_tokens 추정용으로 요청 구성을 하나의 텍스트로 펼친다 — **순수**(테스트 가능).
/// 토큰 추정이 목적이라 형식보다 **포함된 모든 텍스트**(시스템·메시지 본문·도구 입력/결과·
/// 도구 스키마)를 빠짐없이 담는 게 중요하다.
fn render_for_count(system: Option<&str>, messages: &[Message], tools: &[ToolSchema]) -> String {
    let mut s = String::new();
    if let Some(sys) = system {
        s.push_str(sys);
        s.push('\n');
    }
    for m in messages {
        for block in &m.content {
            match block {
                ContentBlock::Text { text } => s.push_str(text),
                ContentBlock::Thinking { text, .. } => s.push_str(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    s.push_str(name);
                    s.push_str(&input.to_string());
                }
                ContentBlock::ToolResult { content, .. } => s.push_str(content),
            }
            s.push('\n');
        }
    }
    for t in tools {
        s.push_str(&t.name);
        s.push_str(&t.description);
        s.push_str(&t.input_schema.to_string());
        s.push('\n');
    }
    s
}

// ───────────────────────── 요청: 코어 → OpenAI 와이어 ─────────────────────────

/// 코어 [`CompletionRequest`] 를 OpenAI Chat Completions 요청 본문으로 변환한다.
///
/// `compat` 은 OpenAI-호환(비표준) 백엔드 대응 — OpenRouter·Gemini(OpenAI 엔드포인트)·
/// Groq·로컬 LLM 등은 추론 모델 전용 파라미터를 거부할 수 있어 직렬화를 바꾼다:
/// - `max_completion_tokens`(표준) ↔ `max_tokens`(compat)
/// - `stream_options.include_usage`·`reasoning_effort` 는 compat 에서 **생략**.
fn to_wire(req: &CompletionRequest, compat: bool) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": messages_to_wire(req.system.as_deref(), &req.messages),
        "stream": true,
    });
    // 추론 모델은 `max_completion_tokens`, 비표준 호환 백엔드는 구형 `max_tokens`.
    body[if compat {
        "max_tokens"
    } else {
        "max_completion_tokens"
    }] = json!(req.max_tokens);
    if !compat {
        // 스트림 끝에 usage 청크를 받는다(compaction 신호). 비표준 백엔드는 미지원이 많다.
        body["stream_options"] = json!({ "include_usage": true });
    }
    if !req.tools.is_empty() {
        body["tools"] = tools_to_wire(&req.tools);
        body["tool_choice"] = json!("auto");
    }
    // 추론 깊이는 OpenAI 자체 파라미터. thinking=Disabled 거나 compat 이면 생략한다.
    if !compat && req.thinking != ThinkingMode::Disabled {
        if let Some(effort) = req.effort {
            body["reasoning_effort"] = json!(map_effort(effort));
        }
    }
    body
}

/// 코어 Effort → OpenAI `reasoning_effort`. OpenAI 는 low|medium|high 만 받으므로
/// xhigh/max 는 high 로 클램프한다.
fn map_effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::XHigh | Effort::Max => "high",
    }
}

fn tools_to_wire(tools: &[ToolSchema]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    },
                })
            })
            .collect(),
    )
}

fn messages_to_wire(system: Option<&str>, messages: &[Message]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for msg in messages {
        match msg.role {
            Role::System => {
                out.push(json!({ "role": "system", "content": collect_text(&msg.content) }))
            }
            Role::User => push_user_message(&msg.content, &mut out),
            Role::Assistant => out.push(assistant_to_wire(&msg.content)),
        }
    }
    Value::Array(out)
}

/// user 턴을 OpenAI 메시지로 푼다. **tool_result 는 별도 `role:"tool"` 메시지**가 되고
/// (OpenAI 와이어 차이), 일반 텍스트는 하나의 `role:"user"` 메시지로 모인다.
fn push_user_message(content: &[ContentBlock], out: &mut Vec<Value>) {
    let mut text = String::new();
    for block in content {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                }));
            }
            ContentBlock::Text { text: t } => append_line(&mut text, t),
            // user 메시지에 thinking/tool_use 는 정상적으로 오지 않는다.
            ContentBlock::Thinking { .. } | ContentBlock::ToolUse { .. } => {}
        }
    }
    if !text.is_empty() {
        out.push(json!({ "role": "user", "content": text }));
    }
}

/// assistant 턴을 OpenAI 메시지로 변환한다. text → `content`, tool_use → `tool_calls`.
/// (thinking 블록은 OpenAI 입력에 넣지 않는다 — 추론은 서버측 비공개.)
fn assistant_to_wire(content: &[ContentBlock]) -> Value {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text: t } => append_line(&mut text, t),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        // OpenAI 는 도구 입력을 JSON **문자열**로 받는다(객체 아님).
                        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                    },
                }));
            }
            ContentBlock::Thinking { .. } | ContentBlock::ToolResult { .. } => {}
        }
    }
    let mut msg = json!({ "role": "assistant" });
    if tool_calls.is_empty() {
        msg["content"] = json!(text);
    } else {
        msg["content"] = if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        };
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    msg
}

fn append_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

fn collect_text(content: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in content {
        if let ContentBlock::Text { text: t } = block {
            append_line(&mut text, t);
        }
    }
    text
}

// ───────────────────────── 응답: OpenAI SSE → 코어 StreamEvent ─────────────────────────

/// 박싱된 SSE 이벤트 스트림(reqwest 바이트 스트림을 `eventsource-stream` 으로 파싱).
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

/// 스트림 구동 상태. [`drive_stream`] 이 SSE 를 당겨 코어 이벤트로 변환·버퍼링한다.
struct StreamState {
    sse: SseStream,
    decoder: ChunkDecoder,
    /// 한 청크가 여러 코어 이벤트를 만들 수 있어 큐에 모아 하나씩 흘린다.
    pending: VecDeque<StreamEvent>,
    /// `[DONE]` 또는 스트림 종료를 봤다.
    done: bool,
    /// 종료 시 `MessageStop` 을 이미 한 번 보냈다(중복 방지).
    stop_sent: bool,
    /// 에러로 끝났다(이 경우 `MessageStop` 을 보내지 않는다).
    errored: bool,
}

impl StreamState {
    fn new(sse: SseStream) -> Self {
        Self {
            sse,
            decoder: ChunkDecoder::new(),
            pending: VecDeque::new(),
            done: false,
            stop_sent: false,
            errored: false,
        }
    }
}

/// `futures::stream::unfold` 한 스텝: 큐에 있으면 흘리고, 없으면 SSE 를 한 청크 당겨
/// 디코드한다. 스트림이 끝나면 마지막에 정규화된 `MessageStop` 을 한 번 낸다.
async fn drive_stream(mut st: StreamState) -> Option<(Result<StreamEvent>, StreamState)> {
    loop {
        if let Some(ev) = st.pending.pop_front() {
            return Some((Ok(ev), st));
        }
        if st.done {
            if !st.stop_sent && !st.errored {
                st.stop_sent = true;
                return Some((Ok(st.decoder.finish()), st));
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
                if event.data == "[DONE]" {
                    st.done = true;
                } else if !event.data.trim().is_empty() {
                    match serde_json::from_str::<OpenAiChunk>(&event.data) {
                        Ok(chunk) => st.pending.extend(st.decoder.decode(chunk)),
                        Err(e) => {
                            st.done = true;
                            st.errored = true;
                            return Some((
                                Err(Error::Provider(format!("malformed SSE chunk: {e}"))),
                                st,
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// SSE 청크들을 코어 [`StreamEvent`] 로 누적 변환하는 디코더(스트림 전반의 상태 보존).
///
/// 상태가 필요한 이유: ① `MessageStart` 는 첫 청크에서만 ② OpenAI tool_call 증분은
/// 첫 청크에만 `id`/`name` 이 있고 이후엔 `index` 만 오므로 index→id 매핑을 들고 있어야
/// 한다 ③ `finish_reason`/`usage` 는 따로 와서 마지막 `MessageStop` 에 합친다.
struct ChunkDecoder {
    started: bool,
    tool_ids: HashMap<u32, String>,
    stop: StopReason,
    usage: Usage,
}

impl ChunkDecoder {
    fn new() -> Self {
        Self {
            started: false,
            tool_ids: HashMap::new(),
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        }
    }

    fn decode(&mut self, chunk: OpenAiChunk) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if !self.started {
            if let Some(model) = chunk.model {
                self.started = true;
                out.push(StreamEvent::MessageStart { model });
            }
        }
        for choice in chunk.choices {
            let delta = choice.delta;
            // 추론 텍스트: OpenAI 표준은 `reasoning_content`, Ollama·gemma 등은 `reasoning`.
            if let Some(rc) = delta.reasoning_content.or(delta.reasoning) {
                if !rc.is_empty() {
                    out.push(StreamEvent::ThinkingDelta(rc));
                }
            }
            if let Some(c) = delta.content {
                if !c.is_empty() {
                    out.push(StreamEvent::TextDelta(c));
                }
            }
            for tc in delta.tool_calls {
                self.decode_tool_call(tc, &mut out);
            }
            if let Some(reason) = choice.finish_reason {
                self.stop = map_finish_reason(&reason);
            }
        }
        if let Some(u) = chunk.usage {
            self.usage = Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_read_tokens: u
                    .prompt_tokens_details
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0),
                cache_write_tokens: 0,
            };
        }
        out
    }

    fn decode_tool_call(&mut self, tc: OpenAiToolCallDelta, out: &mut Vec<StreamEvent>) {
        let args = tc.function.as_ref().and_then(|f| f.arguments.clone());
        if let Some(id) = tc.id {
            // 새 tool_call 시작: id+name 이 이 청크에만 온다.
            let name = tc.function.and_then(|f| f.name).unwrap_or_default();
            self.tool_ids.insert(tc.index, id.clone());
            out.push(StreamEvent::ToolUseStart {
                id: id.clone(),
                name,
            });
            push_input_delta(out, id, args);
        } else if let Some(id) = self.tool_ids.get(&tc.index).cloned() {
            // 같은 tool_call 의 인자 증분(id 없이 index 로만 식별).
            push_input_delta(out, id, args);
        } else {
            tracing::warn!(
                index = tc.index,
                "tool_call delta before its start; dropping"
            );
        }
    }

    fn finish(&self) -> StreamEvent {
        StreamEvent::MessageStop {
            stop_reason: self.stop,
            usage: self.usage,
        }
    }
}

fn push_input_delta(out: &mut Vec<StreamEvent>, id: String, args: Option<String>) {
    if let Some(args) = args {
        if !args.is_empty() {
            out.push(StreamEvent::ToolUseInputDelta {
                id,
                partial_json: args,
            });
        }
    }
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        other => {
            tracing::warn!(finish_reason = %other, "unknown finish_reason; treating as end_turn");
            StopReason::EndTurn
        }
    }
}

// OpenAI SSE 청크의 부분 역직렬화(필요한 필드만, 나머지는 무시).
#[derive(Debug, Deserialize)]
struct OpenAiChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
    /// 추론 텍스트(ThinkingDelta 로). OpenAI 표준은 `reasoning_content`,
    /// Ollama·gemma 등 일부 백엔드는 `reasoning` 필드를 쓴다 — 둘 다 받는다.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiPromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(messages: Vec<Message>, tools: Vec<ToolSchema>) -> CompletionRequest {
        CompletionRequest {
            model: "gpt-5.5".into(),
            system: Some("be nice".into()),
            messages,
            tools,
            max_tokens: 64_000,
            effort: Some(Effort::High),
            thinking: ThinkingMode::Adaptive,
        }
    }

    #[test]
    fn wire_maps_system_and_user() {
        let wire = to_wire(&req(vec![Message::user("hi")], vec![]), false);
        assert_eq!(wire["model"], "gpt-5.5");
        assert_eq!(wire["max_completion_tokens"], 64_000);
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["reasoning_effort"], "high");
        let msgs = wire["messages"].as_array().expect("messages array");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be nice");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hi");
        assert!(wire.get("tools").is_none());
    }

    #[test]
    fn wire_maps_assistant_tool_use_and_tool_result() {
        let assistant = Message::assistant(vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "read".into(),
            input: json!({ "path": "a.rs" }),
        }]);
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "file body".into(),
                is_error: false,
            }],
        };
        let wire = to_wire(
            &req(
                vec![Message::user("read it"), assistant, tool_result],
                vec![],
            ),
            false,
        );
        let msgs = wire["messages"].as_array().expect("messages array");
        // [system, user, assistant(tool_calls), tool]
        let a = &msgs[2];
        assert_eq!(a["role"], "assistant");
        assert!(a["content"].is_null());
        let tc = &a["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "read");
        // arguments 는 JSON 문자열이어야 한다(객체가 아니라).
        assert_eq!(tc["function"]["arguments"], "{\"path\":\"a.rs\"}");
        let t = &msgs[3];
        assert_eq!(t["role"], "tool");
        assert_eq!(t["tool_call_id"], "call_1");
        assert_eq!(t["content"], "file body");
    }

    #[test]
    fn wire_includes_tools_and_tool_choice() {
        let tools = vec![ToolSchema {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({ "type": "object" }),
        }];
        let wire = to_wire(&req(vec![Message::user("hi")], tools), false);
        let t = &wire["tools"][0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["function"]["name"], "read");
        assert_eq!(t["function"]["parameters"], json!({ "type": "object" }));
        assert_eq!(wire["tool_choice"], "auto");
    }

    #[test]
    fn disabled_thinking_omits_reasoning_effort() {
        let mut r = req(vec![Message::user("hi")], vec![]);
        r.thinking = ThinkingMode::Disabled;
        assert!(to_wire(&r, false).get("reasoning_effort").is_none());
    }

    #[test]
    fn compat_mode_uses_max_tokens_and_omits_extras() {
        // compat=true: Ollama·OpenRouter·Gemini(OpenAI 엔드포인트) 등 비표준 호환 백엔드용.
        let wire = to_wire(&req(vec![Message::user("hi")], vec![]), true);
        assert_eq!(wire["max_tokens"], 64_000);
        assert!(wire.get("max_completion_tokens").is_none());
        assert!(wire.get("stream_options").is_none());
        // effort 가 Some 이어도 compat 이면 reasoning_effort 를 보내지 않는다.
        assert!(wire.get("reasoning_effort").is_none());
    }

    fn decode_data(decoder: &mut ChunkDecoder, data: &str) -> Vec<StreamEvent> {
        decoder.decode(serde_json::from_str(data).expect("valid chunk json"))
    }

    #[test]
    fn decodes_text_stream_with_usage_and_stop() {
        let mut d = ChunkDecoder::new();
        let e1 = decode_data(
            &mut d,
            r#"{"model":"gpt-5.5","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        );
        assert!(
            matches!(e1.as_slice(), [StreamEvent::MessageStart { model }] if model == "gpt-5.5")
        );
        let e2 = decode_data(
            &mut d,
            r#"{"model":"gpt-5.5","choices":[{"delta":{"content":"Hi"}}]}"#,
        );
        assert!(matches!(e2.as_slice(), [StreamEvent::TextDelta(t)] if t == "Hi"));
        let e3 = decode_data(
            &mut d,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        );
        assert!(e3.is_empty());
        decode_data(
            &mut d,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
        );
        match d.finish() {
            StreamEvent::MessageStop { stop_reason, usage } => {
                assert_eq!(stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("expected MessageStop, got {other:?}"),
        }
    }

    #[test]
    fn decodes_tool_call_split_across_chunks() {
        let mut d = ChunkDecoder::new();
        decode_data(
            &mut d,
            r#"{"model":"m","choices":[{"delta":{"role":"assistant"}}]}"#,
        );
        let start = decode_data(
            &mut d,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
        );
        assert!(matches!(start.as_slice(),
            [StreamEvent::ToolUseStart { id, name }] if id == "call_1" && name == "read"));
        let d1 = decode_data(
            &mut d,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
        );
        assert!(matches!(d1.as_slice(),
            [StreamEvent::ToolUseInputDelta { id, partial_json }] if id == "call_1" && partial_json == "{\"path\":"));
        let d2 = decode_data(
            &mut d,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        assert!(matches!(d2.as_slice(),
            [StreamEvent::ToolUseInputDelta { partial_json, .. }] if partial_json == "\"a.rs\"}"));
        assert!(matches!(
            d.finish(),
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn decodes_reasoning_field_as_thinking() {
        // Ollama·gemma 는 추론을 표준 `reasoning_content` 가 아니라 `reasoning` 으로 보낸다.
        let mut d = ChunkDecoder::new();
        let ev = decode_data(
            &mut d,
            r#"{"choices":[{"delta":{"content":"","reasoning":"thinking…"}}]}"#,
        );
        assert!(matches!(ev.as_slice(), [StreamEvent::ThinkingDelta(t)] if t == "thinking…"));
    }

    #[test]
    fn effort_clamps_above_high() {
        assert_eq!(map_effort(Effort::Low), "low");
        assert_eq!(map_effort(Effort::Medium), "medium");
        assert_eq!(map_effort(Effort::XHigh), "high");
        assert_eq!(map_effort(Effort::Max), "high");
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(map_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(map_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_finish_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_finish_reason("content_filter"), StopReason::Refusal);
        assert_eq!(map_finish_reason("weird"), StopReason::EndTurn);
    }

    #[test]
    fn render_for_count_includes_all_text() {
        let msgs = vec![
            Message::user("hello world"),
            Message::assistant(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "grep".into(),
                input: json!({ "pattern": "needle" }),
            }]),
        ];
        let tools = vec![ToolSchema {
            name: "grep".into(),
            description: "search".into(),
            input_schema: json!({ "type": "object" }),
        }];
        let rendered = render_for_count(Some("be terse"), &msgs, &tools);
        for needle in ["be terse", "hello world", "grep", "needle", "search"] {
            assert!(
                rendered.contains(needle),
                "missing `{needle}` in: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn count_tokens_is_positive_and_monotonic() {
        let p = OpenAiProvider::new("openai", "gpt-5.5".into(), "k".into(), None, false);
        let short = p
            .count_tokens(Some("sys"), &[Message::user("hi")], &[])
            .await
            .expect("count");
        let long = p
            .count_tokens(
                Some("sys"),
                &[Message::user(
                    "this is a substantially longer user message with many more tokens",
                )],
                &[],
            )
            .await
            .expect("count");
        assert!(short > 0, "non-empty request should count > 0");
        assert!(long > short, "more text should estimate more tokens");
    }
}
